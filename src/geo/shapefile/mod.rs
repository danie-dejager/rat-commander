//! Shapefiles, shown and edited on the same map as GeoJSON.
//!
//! A shapefile is a *set* of files — `.shp` geometry, `.shx` index, `.dbf`
//! attributes, usually a `.prj` saying what the coordinates mean — none of which
//! is text, while the map editor in [`crate::ui::dialog::geomap`] edits GeoJSON
//! by rewriting ranges of the editor's text.
//!
//! So the set is read into **GeoJSON text**, and that is what is edited. The
//! whole existing editor then works unchanged — drawing, dragging, undo, adding
//! and removing features — and a save turns the text back into the three files.
//! [`Origin`] is what makes the return trip faithful: it carries the attribute
//! schema, the one shape type the file is allowed to hold, and the projection
//! its numbers were in, none of which survives a trip through GeoJSON.

pub mod dbf;
pub mod prj;
pub mod project;
pub mod shp;

use crate::geo::geojson::Shape;
use project::Projection;
use std::path::{Path, PathBuf};

/// Largest `.shp` that will be opened. A shapefile is read whole and turned
/// into text, which costs several times its own size in memory, so this is well
/// below what the viewer allows itself for a file it only scrolls through.
pub const MAX_SHP_BYTES: u64 = 64 * 1024 * 1024;

/// Whether `name` is the geometry file of a shapefile set.
///
/// Only `.shp` opens the set: `.shx` and `.dbf` are parts of it, and opening
/// one of those on its own would be opening half a dataset.
pub fn is_shapefile_name(name: &str) -> bool {
    name.rsplit('.').next().is_some_and(|e| e.eq_ignore_ascii_case("shp"))
}

/// What is needed to write an edited shapefile back as it was found.
#[derive(Debug, Clone)]
pub struct Origin {
    /// The `.shp` itself; the siblings are derived from it.
    pub path: PathBuf,
    /// The one shape type the file holds. A drawing of another kind cannot go
    /// into it, because the format allows a file only one.
    pub kind: shp::ShapeType,
    /// The attribute schema, kept so a rewrite does not invent one.
    pub fields: Vec<dbf::Field>,
    /// What the file's numbers meant, so they can be put back that way.
    pub projection: Projection,
}

impl Origin {
    /// The sibling with extension `ext`, matching the `.shp`'s own spelling of
    /// its case — `ROADS.SHP` keeps company with `ROADS.DBF`, not `ROADS.dbf`.
    pub fn sibling(&self, ext: &str) -> PathBuf {
        let upper = self
            .path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.chars().all(|c| c.is_uppercase() || !c.is_alphabetic()));
        self.path.with_extension(if upper { ext.to_ascii_uppercase() } else { ext.to_string() })
    }
}

/// What reading a set produced: the GeoJSON to edit, and how to put it back.
pub struct Opened {
    pub text: String,
    pub origin: Origin,
    /// Something the user should be told, though the file did open — a
    /// projection that could not be undone, or attributes that went missing.
    pub warning: Option<String>,
}

/// Read the shapefile set `path` belongs to, as GeoJSON text.
///
/// `Err` with a sentence fit to show when the set cannot be read or cannot be
/// put on a map.
pub fn open(path: &Path) -> Result<Opened, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read {}: {e}", name_of(path)))?;
    if bytes.len() as u64 > MAX_SHP_BYTES {
        return Err(format!("{} is too large to open as a map", name_of(path)));
    }
    let (kind, shapes) =
        shp::read(&bytes).ok_or_else(|| format!("{} is not a shapefile", name_of(path)))?;

    // The .prj decides what the numbers mean. Without one they are degrees, by
    // the same convention every other reader follows. It is only read: a save
    // never changes the coordinate system, so the file on disk stays valid and
    // is left exactly as it was.
    let crs = prj::classify(read_sibling_text(path, "prj").as_deref().unwrap_or(""));
    let Some(projection) = crs.projection() else {
        let prj::Crs::Unsupported(what) = &crs else { unreachable!("only Unsupported has none") };
        return Err(format!(
            "{} is in {what}, which this cannot put on a map. Only degrees, Web Mercator and \
             UTM on WGS84 are understood.",
            name_of(path)
        ));
    };

    let table = read_sibling(path, "dbf").and_then(|b| dbf::read(&b)).unwrap_or_default();
    let mut warning = None;
    if !table.rows.is_empty() && table.rows.len() != shapes.len() {
        warning = Some(format!(
            "{} has {} shapes but {} attribute rows; the extra ones are left out",
            name_of(path),
            shapes.len(),
            table.rows.len()
        ));
    }

    let text = to_geojson(&shapes, &table, projection, kind);
    Ok(Opened {
        text,
        origin: Origin { path: path.to_path_buf(), kind, fields: table.fields, projection },
        warning,
    })
}

/// The file's shapes and attributes as a GeoJSON FeatureCollection, laid out
/// one feature per block so the map editor's own edits sit among them tidily.
fn to_geojson(
    shapes: &[Shape],
    table: &dbf::Table,
    projection: Projection,
    kind: shp::ShapeType,
) -> String {
    let mut out = String::from("{\n  \"type\": \"FeatureCollection\",\n  \"features\": [\n");
    for (i, shape) in shapes.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("    {\n      \"type\": \"Feature\",\n      \"properties\": {");
        let row = table.rows.get(i);
        if let Some(row) = row.filter(|r| !r.is_empty()) {
            out.push('\n');
            for (j, field) in table.fields.iter().enumerate() {
                let value = row.get(j).map(String::as_str).unwrap_or("");
                if j > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&format!(
                    "        {}: {}",
                    json_string(&field.name),
                    property_value(field, value)
                ));
            }
            out.push_str("\n      ");
        }
        out.push_str("},\n      \"geometry\": ");
        out.push_str(&geometry_json(shape, projection, kind));
        out.push_str("\n    }");
    }
    out.push_str("\n  ]\n}\n");
    out
}

/// One geometry, with its positions turned into longitude and latitude.
fn geometry_json(shape: &Shape, projection: Projection, kind: shp::ShapeType) -> String {
    // A file already in degrees is written back exactly as it was read, so
    // opening and saving it without an edit changes nothing at all. A file that
    // had to be converted cannot be exact anyway — the inverse leaves about a
    // micrometre — so its coordinates are rounded to a clean tenth of a
    // millimetre rather than printed to seventeen digits of arithmetic noise.
    let exact = projection == Projection::Geographic;
    let pt = |p: &[f64; 2]| {
        let [lon, lat] = projection.to_lonlat(*p);
        format!("[{}, {}]", trim_float(lon, exact), trim_float(lat, exact))
    };
    let line =
        |pts: &Vec<[f64; 2]>| format!("[{}]", pts.iter().map(&pt).collect::<Vec<_>>().join(", "));
    match shape {
        Shape::Point(p) => format!("{{ \"type\": \"Point\", \"coordinates\": {} }}", pt(p)),
        Shape::Line(pts) => {
            let name = if kind == shp::ShapeType::MultiPoint { "MultiPoint" } else { "LineString" };
            format!("{{ \"type\": \"{name}\", \"coordinates\": {} }}", line(pts))
        }
        Shape::Polygon(rings) => format!(
            "{{ \"type\": \"Polygon\", \"coordinates\": [{}] }}",
            rings.iter().map(line).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// A `.dbf` value as JSON: numbers and booleans unquoted so the document reads
/// as data rather than as a table of strings.
fn property_value(field: &dbf::Field, value: &str) -> String {
    let v = value.trim();
    if v.is_empty() {
        return "null".to_string();
    }
    match field.kind {
        b'N' | b'F' if v.parse::<f64>().is_ok() => v.to_string(),
        b'L' => match v {
            "T" | "t" | "Y" | "y" => "true".to_string(),
            "F" | "f" | "N" | "n" => "false".to_string(),
            _ => "null".to_string(),
        },
        _ => json_string(v),
    }
}

/// A JSON string literal, with the escapes the format requires.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A coordinate as text. With `exact`, the shortest form that reads back as the
/// very same number (Rust's float formatting is shortest-round-trip); otherwise
/// rounded to seven decimals.
///
/// Seven decimals of a degree is about a centimetre — finer than any shapefile's
/// own accuracy — and it keeps a converted coordinate reading as `48.2082`
/// rather than as the `48.20819999895796` the projection arithmetic leaves
/// behind. Exponent form is not wanted in a coordinate list, so a value that
/// would produce one is written out in full instead.
fn trim_float(v: f64, exact: bool) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    if exact {
        let s = format!("{v}");
        if !s.contains(['e', 'E']) {
            return s;
        }
    }
    let s = format!("{v:.7}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s.is_empty() || s == "-" { "0".to_string() } else { s }
}

/// The bytes of a sibling file, if it is there.
fn read_sibling(path: &Path, ext: &str) -> Option<Vec<u8>> {
    // Both spellings, since a set may be `ROADS.SHP` + `ROADS.DBF`.
    for e in [ext.to_string(), ext.to_ascii_uppercase()] {
        if let Ok(b) = std::fs::read(path.with_extension(&e)) {
            return Some(b);
        }
    }
    None
}

fn read_sibling_text(path: &Path, ext: &str) -> Option<String> {
    read_sibling(path, ext).map(|b| String::from_utf8_lossy(&b).into_owned())
}

fn name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// -- Writing the set back -------------------------------------------------

/// What a save did, beyond succeeding: anything the user should know about the
/// shape of the data that changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Saved {
    /// Attribute columns that had to be added for properties the schema had no
    /// place for.
    pub added_fields: Vec<String>,
    /// Values cut to fit a column that was already there.
    pub truncated: usize,
    /// Features whose geometry does not suit the file's one shape type, and so
    /// could not be written.
    pub wrong_kind: usize,
}

impl Saved {
    /// A sentence for the user, or nothing when the save was uneventful.
    pub fn message(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if !self.added_fields.is_empty() {
            parts.push(format!("added the column(s) {}", self.added_fields.join(", ")));
        }
        if self.truncated > 0 {
            parts.push(format!("cut {} value(s) to fit their column", self.truncated));
        }
        if self.wrong_kind > 0 {
            parts.push(format!(
                "left out {} feature(s) whose geometry is not {}",
                self.wrong_kind,
                self.kind_word()
            ));
        }
        (!parts.is_empty()).then(|| format!("Saved, but {}", parts.join("; ")))
    }

    fn kind_word(&self) -> &'static str {
        "the kind this file holds"
    }
}

/// Write `text` — the edited GeoJSON — back into `origin`'s file set.
///
/// All three files are built in memory and written to temporary siblings, then
/// renamed into place only once every one of them is on disk: a shapefile whose
/// `.shp` had been replaced but whose `.dbf` had not would be a corrupt dataset,
/// and that must not be a state a failed save can leave behind.
pub fn save(origin: &Origin, text: &str) -> Result<Saved, String> {
    let doc = crate::geo::geojson::extract(text);
    let mut shapes: Vec<Shape> = Vec::new();
    let mut props: Vec<Vec<(String, String)>> = Vec::new();
    for object in &doc.objects {
        for feature in &object.features {
            // A feature may hold several geometries; each becomes a record, and
            // they share the feature's attributes.
            for shape in &feature.shapes {
                shapes.push(shape.clone());
                props.push(feature.props.clone());
            }
        }
    }

    let mut report = Saved::default();
    // Back into the file's own coordinates.
    let shapes: Vec<Shape> = shapes.iter().map(|s| unproject(s, origin.projection)).collect();
    // A geometry the file's type cannot hold is counted and written as a Null
    // shape, so the records still line up with the attribute rows.
    for s in &shapes {
        if !suits(origin.kind, s) {
            report.wrong_kind += 1;
        }
    }

    let table = build_table(origin, &props, &mut report);
    let (shp_bytes, shx_bytes) = shp::write(origin.kind, &shapes);
    let dbf_bytes = dbf::write(&table);

    write_set(origin, &shp_bytes, &shx_bytes, &dbf_bytes)?;
    Ok(report)
}

/// Whether `shape` is something a file of this type can hold.
fn suits(kind: shp::ShapeType, shape: &Shape) -> bool {
    matches!(
        (kind, shape),
        (shp::ShapeType::Point, Shape::Point(_))
            | (shp::ShapeType::MultiPoint, Shape::Line(_))
            | (shp::ShapeType::PolyLine, Shape::Line(_))
            | (shp::ShapeType::Polygon, Shape::Polygon(_))
    )
}

/// Every position of `shape` back in the file's coordinates.
fn unproject(shape: &Shape, p: Projection) -> Shape {
    match shape {
        Shape::Point(pt) => Shape::Point(p.of_lonlat(*pt)),
        Shape::Line(pts) => Shape::Line(pts.iter().map(|q| p.of_lonlat(*q)).collect()),
        Shape::Polygon(rings) => Shape::Polygon(
            rings.iter().map(|r| r.iter().map(|q| p.of_lonlat(*q)).collect()).collect(),
        ),
    }
}

/// The attribute table to write: the schema the file came with, plus a column
/// for any property the user has added since, and one row per shape.
fn build_table(origin: &Origin, props: &[Vec<(String, String)>], report: &mut Saved) -> dbf::Table {
    let mut fields = origin.fields.clone();

    // Properties the schema has no column for get one, rather than being
    // dropped: the user typed them on the map, and losing them silently would
    // be the worse failure.
    let mut extra_names: Vec<String> = Vec::new();
    for row in props {
        for (key, _) in row {
            let known = fields.iter().any(|f| matches_property(f, key));
            let already = extra_names.iter().any(|n| n.eq_ignore_ascii_case(key));
            if !known && !already {
                extra_names.push(key.clone());
            }
        }
    }
    for name in &extra_names {
        let values: Vec<String> = props
            .iter()
            .map(|row| {
                row.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            })
            .collect();
        fields.push(dbf::Field::inferred(name, &values));
    }
    dbf::deduplicate(&mut fields);
    report.added_fields = fields.iter().skip(origin.fields.len()).map(|f| f.name.clone()).collect();

    let rows: Vec<Vec<String>> = props
        .iter()
        .map(|row| {
            fields
                .iter()
                .map(|f| {
                    let v = row
                        .iter()
                        .find(|(k, _)| matches_property(f, k))
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    if v.chars().count() > f.len as usize && f.kind == b'C' {
                        report.truncated += 1;
                    }
                    v
                })
                .collect()
        })
        .collect();

    dbf::Table { fields, rows }
}

/// Whether a GeoJSON property key names this column. The `.dbf` upper-cases and
/// shortens names, so the comparison has to be as forgiving as that was lossy.
fn matches_property(field: &dbf::Field, key: &str) -> bool {
    field.name.eq_ignore_ascii_case(key)
        || field.name.eq_ignore_ascii_case(&key.chars().take(10).collect::<String>())
}

/// Put the three files in place together, or leave every one of them as it was.
fn write_set(origin: &Origin, shp_b: &[u8], shx_b: &[u8], dbf_b: &[u8]) -> Result<(), String> {
    let targets = [
        (origin.sibling("shp"), shp_b),
        (origin.sibling("shx"), shx_b),
        (origin.sibling("dbf"), dbf_b),
    ];
    // Write every temporary first; a failure here has touched nothing real.
    let mut temps: Vec<PathBuf> = Vec::new();
    for (path, bytes) in &targets {
        let tmp = path.with_extension(format!(
            "{}.rc-new",
            path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
        ));
        if let Err(e) = std::fs::write(&tmp, bytes) {
            temps.iter().for_each(|t| {
                let _ = std::fs::remove_file(t);
            });
            return Err(format!("Cannot write {}: {e}", name_of(&tmp)));
        }
        temps.push(tmp);
    }
    // Then move them in. A rename within a directory is as close to atomic as
    // the filesystem offers, so the window where the set disagrees with itself
    // is as small as it can be made.
    for (i, (path, _)) in targets.iter().enumerate() {
        if let Err(e) = std::fs::rename(&temps[i], path) {
            return Err(format!("Cannot replace {}: {e}", name_of(path)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
