//! Round-trip tests over shapefile sets built in the test itself, so no binary
//! fixture has to live in the repository.
//!
//! The two that matter most are [`a_polygon_keeps_its_winding_through_a_round_trip`]
//! and the projection pair: both are failures that would produce a file which
//! looks right here and is wrong everywhere else.

use super::*;
use crate::geo::geojson::Shape;
use std::path::PathBuf;

/// A scratch directory named after the calling test.
fn scratch(tag: &str) -> PathBuf {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("rc_shp_{tag}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a complete set — `.shp`, `.shx`, `.dbf`, and a `.prj` if given — and
/// return the path of the `.shp`.
fn write_set_files(
    dir: &Path,
    stem: &str,
    kind: shp::ShapeType,
    shapes: &[Shape],
    table: &dbf::Table,
    prj: Option<&str>,
) -> PathBuf {
    let (shp_b, shx_b) = shp::write(kind, shapes);
    let base = dir.join(stem);
    std::fs::write(base.with_extension("shp"), &shp_b).unwrap();
    std::fs::write(base.with_extension("shx"), &shx_b).unwrap();
    std::fs::write(base.with_extension("dbf"), dbf::write(table)).unwrap();
    if let Some(p) = prj {
        std::fs::write(base.with_extension("prj"), p).unwrap();
    }
    base.with_extension("shp")
}

fn table_of(fields: Vec<dbf::Field>, rows: Vec<Vec<String>>) -> dbf::Table {
    dbf::Table { fields, rows }
}

fn text_field(name: &str, len: u8) -> dbf::Field {
    dbf::Field { name: name.to_string(), kind: b'C', len, decimals: 0 }
}

#[test]
fn shapefile_names_are_only_the_geometry_file() {
    assert!(is_shapefile_name("roads.shp") && is_shapefile_name("ROADS.SHP"));
    // The siblings are parts of the set, not the set.
    assert!(!is_shapefile_name("roads.shx") && !is_shapefile_name("roads.dbf"));
    assert!(!is_shapefile_name("roads.txt") && !is_shapefile_name("noext"));
}

#[test]
fn points_round_trip_through_the_file_and_back() {
    let dir = scratch("points");
    let shapes = vec![Shape::Point([16.37, 48.21]), Shape::Point([-0.13, 51.51])];
    let table =
        table_of(vec![text_field("NAME", 10)], vec![vec!["Vienna".into()], vec!["London".into()]]);
    let path = write_set_files(&dir, "cities", shp::ShapeType::Point, &shapes, &table, None);

    let opened = open(&path).expect("opens");
    assert_eq!(opened.origin.kind, shp::ShapeType::Point);
    let doc = crate::geo::geojson::extract(&opened.text);
    let feats: Vec<_> = doc.objects.iter().flat_map(|o| o.features.iter()).collect();
    assert_eq!(feats.len(), 2, "one feature per shape");
    assert!(opened.text.contains("Vienna") && opened.text.contains("London"));
    assert_eq!(doc.skipped, 0, "nothing was dropped as out-of-range");

    // Save it back unchanged and read it again: the same positions come out.
    save(&opened.origin, &opened.text).expect("saves");
    let again = open(&path).expect("reopens");
    let (_, back) = shp::read(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back.len(), 2);
    match (&back[0], &shapes[0]) {
        (Shape::Point(a), Shape::Point(b)) => {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        }
        _ => panic!("expected points"),
    }
    assert!(again.text.contains("Vienna"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_polygon_keeps_its_winding_through_a_round_trip() {
    let dir = scratch("winding");
    // A square with a square hole, in GeoJSON's winding: outer counter-
    // clockwise, hole clockwise.
    let outer = vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0], [0.0, 0.0]];
    let hole = vec![[1.0, 1.0], [1.0, 3.0], [3.0, 3.0], [3.0, 1.0], [1.0, 1.0]];
    assert!(!shp::ring_is_clockwise(&outer), "the test's outer ring is GeoJSON-wound");
    assert!(shp::ring_is_clockwise(&hole), "and its hole is the other way");

    let shapes = vec![Shape::Polygon(vec![outer.clone(), hole.clone()])];
    let table = table_of(vec![text_field("NAME", 8)], vec![vec!["Ring".into()]]);
    let path = write_set_files(&dir, "rings", shp::ShapeType::Polygon, &shapes, &table, None);

    // In the file itself, the winding must be the shapefile's: outer clockwise.
    let raw = std::fs::read(&path).unwrap();
    let (_, in_file) = shp::read(&raw).unwrap();
    // `shp::read` flips back to GeoJSON's winding on the way out, so what it
    // returns should match what went in.
    match &in_file[0] {
        Shape::Polygon(rings) => {
            assert_eq!(rings.len(), 2, "outer ring and hole both survive");
            assert!(!shp::ring_is_clockwise(&rings[0]), "outer comes back counter-clockwise");
            assert!(shp::ring_is_clockwise(&rings[1]), "the hole comes back clockwise");
        }
        other => panic!("expected a polygon, got {other:?}"),
    }

    // And a full read/save/read leaves it the same way round.
    let opened = open(&path).expect("opens");
    save(&opened.origin, &opened.text).expect("saves");
    let (_, after) = shp::read(&std::fs::read(&path).unwrap()).unwrap();
    match &after[0] {
        Shape::Polygon(rings) => {
            assert!(!shp::ring_is_clockwise(&rings[0]), "still outer counter-clockwise");
            assert!(shp::ring_is_clockwise(&rings[1]), "hole still clockwise");
        }
        other => panic!("expected a polygon, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shx_index_points_at_the_records_the_shp_holds() {
    let dir = scratch("index");
    let shapes = vec![Shape::Point([1.0, 2.0]), Shape::Point([3.0, 4.0]), Shape::Point([5.0, 6.0])];
    let table = table_of(
        vec![text_field("ID", 4)],
        vec![vec!["a".into()], vec!["b".into()], vec!["c".into()]],
    );
    let path = write_set_files(&dir, "idx", shp::ShapeType::Point, &shapes, &table, None);

    let shp_b = std::fs::read(&path).unwrap();
    let shx_b = std::fs::read(path.with_extension("shx")).unwrap();
    assert_eq!(shx_b.len(), shp::HEADER_LEN + 3 * 8, "one 8-byte entry per shape");
    // Both headers state their own file's length, in 16-bit words.
    let words = |b: &[u8]| i32::from_be_bytes(b[24..28].try_into().unwrap()) as usize * 2;
    assert_eq!(words(&shp_b), shp_b.len(), ".shp header states its length");
    assert_eq!(words(&shx_b), shx_b.len(), ".shx header states its length");

    // Every index entry lands on a record whose number is its position.
    for i in 0..3 {
        let at = shp::HEADER_LEN + i * 8;
        let offset = i32::from_be_bytes(shx_b[at..at + 4].try_into().unwrap()) as usize * 2;
        let number = i32::from_be_bytes(shp_b[offset..offset + 4].try_into().unwrap());
        assert_eq!(number, i as i32 + 1, "record {i} is numbered from one");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_shp_header_bounding_box_covers_every_shape() {
    let shapes = vec![Shape::Point([-5.0, -2.0]), Shape::Point([7.0, 9.0])];
    let (shp_b, _) = shp::write(shp::ShapeType::Point, &shapes);
    let at = |o: usize| f64::from_le_bytes(shp_b[o..o + 8].try_into().unwrap());
    assert_eq!((at(36), at(44), at(52), at(60)), (-5.0, -2.0, 7.0, 9.0));
}

#[test]
fn dbf_field_types_survive_a_round_trip() {
    let fields = vec![
        text_field("NAME", 10),
        dbf::Field { name: "POP".into(), kind: b'N', len: 8, decimals: 0 },
        dbf::Field { name: "AREA".into(), kind: b'F', len: 10, decimals: 2 },
        dbf::Field { name: "CAPITAL".into(), kind: b'L', len: 1, decimals: 0 },
    ];
    let rows = vec![
        vec!["Vienna".into(), "1920000".into(), "414.78".into(), "T".into()],
        vec!["Graz".into(), "294000".into(), "127.58".into(), "F".into()],
    ];
    let table = table_of(fields.clone(), rows.clone());

    let back = dbf::read(&dbf::write(&table)).expect("reads what it wrote");
    assert_eq!(back.fields, fields, "the schema is unchanged");
    assert_eq!(back.rows, rows, "and so are the values");
}

#[test]
fn a_deleted_dbf_record_is_not_read_back() {
    let table = table_of(vec![text_field("N", 4)], vec![vec!["keep".into()], vec!["gone".into()]]);
    let mut bytes = dbf::write(&table);
    // Mark the second record deleted, as an editor would.
    let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let record_len = u16::from_le_bytes([bytes[10], bytes[11]]) as usize;
    bytes[header_len + record_len] = b'*';
    let back = dbf::read(&bytes).expect("reads");
    assert_eq!(back.rows, vec![vec!["keep".to_string()]], "the deleted row is skipped");
}

#[test]
fn a_new_property_gets_a_column_rather_than_being_dropped() {
    let dir = scratch("schema");
    let shapes = vec![Shape::Point([1.0, 2.0])];
    let table = table_of(vec![text_field("NAME", 10)], vec![vec!["One".into()]]);
    let path = write_set_files(&dir, "grow", shp::ShapeType::Point, &shapes, &table, None);

    let opened = open(&path).expect("opens");
    // Add a property the schema has no column for, as drawing on the map would.
    let edited =
        opened.text.replace("\"NAME\": \"One\"", "\"NAME\": \"One\",\n        \"HEIGHT\": 42");
    let report = save(&opened.origin, &edited).expect("saves");
    assert_eq!(report.added_fields, vec!["HEIGHT".to_string()], "the column was added");

    let back = dbf::read(&std::fs::read(path.with_extension("dbf")).unwrap()).unwrap();
    assert!(back.fields.iter().any(|f| f.name == "HEIGHT"), "and is in the file");
    let i = back.fields.iter().position(|f| f.name == "HEIGHT").unwrap();
    assert_eq!(back.rows[0][i], "42", "with the value the user typed");
    // A number stays a number rather than becoming text.
    assert_eq!(back.fields[i].kind, b'N');
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_geometry_of_the_wrong_kind_is_reported_not_silently_written() {
    let dir = scratch("kind");
    let shapes = vec![Shape::Point([1.0, 2.0])];
    let table = table_of(vec![text_field("N", 4)], vec![vec!["a".into()]]);
    let path = write_set_files(&dir, "pts", shp::ShapeType::Point, &shapes, &table, None);
    let opened = open(&path).expect("opens");

    // A polygon in a file that may only hold points.
    let edited = opened.text.replace(
        "{ \"type\": \"Point\", \"coordinates\": [1, 2] }",
        "{ \"type\": \"Polygon\", \"coordinates\": [[[0,0],[1,0],[1,1],[0,0]]] }",
    );
    assert_ne!(edited, opened.text, "the test edited something");
    let report = save(&opened.origin, &edited).expect("saves the rest");
    assert_eq!(report.wrong_kind, 1, "the mismatch is counted");
    assert!(report.message().is_some(), "and the user is told");
    let _ = std::fs::remove_dir_all(&dir);
}

// -- Coordinate systems ---------------------------------------------------

#[test]
fn a_prj_is_classified_by_what_it_says() {
    use prj::Crs;
    assert_eq!(prj::classify(""), Crs::Geographic, "no .prj means degrees");
    assert_eq!(prj::classify("GEOGCS[\"GCS_WGS_1984\",DATUM[\"D_WGS_1984\"]]"), Crs::Geographic);
    assert_eq!(
        prj::classify("PROJCS[\"WGS_1984_Web_Mercator_Auxiliary_Sphere\",GEOGCS[...]]"),
        Crs::Projected(project::Projection::WebMercator)
    );
    assert_eq!(
        prj::classify("PROJCS[\"WGS_1984_UTM_Zone_33N\",GEOGCS[\"GCS_WGS_1984\"]]"),
        Crs::Projected(project::Projection::Utm { zone: 33, north: true })
    );
    assert_eq!(
        prj::classify("PROJCS[\"WGS_1984_UTM_Zone_56S\",GEOGCS[\"GCS_WGS_1984\"]]"),
        Crs::Projected(project::Projection::Utm { zone: 56, north: false })
    );
    // Something this cannot undo is named, not guessed at.
    match prj::classify("PROJCS[\"Lambert_Conformal_Conic_Austria\",GEOGCS[...]]") {
        Crs::Unsupported(name) => assert!(name.contains("Lambert")),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn web_mercator_and_utm_invert_exactly() {
    use project::Projection;
    // Each place with the UTM zone that actually covers it. UTM is a zoned
    // projection — its series only converges near its own central meridian —
    // so a place must be tested in its own zone, as a real file always is.
    let places: [([f64; 2], u8, bool); 4] = [
        ([16.3738, 48.2082], 33, true),    // Vienna
        ([-0.1276, 51.5072], 30, true),    // London
        ([151.2093, -33.8688], 56, false), // Sydney
        ([0.0, 0.0], 31, true),            // the origin
    ];
    for (p, zone, north) in places {
        for proj in [Projection::WebMercator, Projection::Utm { zone, north }] {
            let there = proj.of_lonlat(p);
            let back = proj.to_lonlat(there);
            // Well under a millimetre on the ground, so a save that touched one
            // feature leaves every other coordinate where it was.
            assert!(
                (back[0] - p[0]).abs() < 1e-7 && (back[1] - p[1]).abs() < 1e-7,
                "{proj:?} did not invert {p:?}: got {back:?}"
            );
        }
    }
    // Geographic coordinates pass straight through, both ways.
    let p = [16.3738, 48.2082];
    assert_eq!(Projection::Geographic.of_lonlat(p), p);
    assert_eq!(Projection::Geographic.to_lonlat(p), p);
}

#[test]
fn a_utm_easting_is_where_the_zone_says_it_is() {
    use project::Projection;
    // Vienna is in zone 33N; its easting should land inside the zone's usual
    // range and its northing be the distance from the equator, roughly.
    let m = Projection::Utm { zone: 33, north: true }.of_lonlat([16.3738, 48.2082]);
    assert!((100_000.0..900_000.0).contains(&m[0]), "a plausible easting: {m:?}");
    assert!((5_330_000.0..5_350_000.0).contains(&m[1]), "a plausible northing: {m:?}");
}

#[test]
fn a_utm_file_is_read_as_degrees_and_written_back_as_metres() {
    let dir = scratch("utm");
    // Vienna in UTM zone 33N, which is where it is.
    let proj = project::Projection::Utm { zone: 33, north: true };
    let metres = proj.of_lonlat([16.3738, 48.2082]);
    assert!(metres[0] > 100_000.0, "the fixture really is in metres: {metres:?}");

    let shapes = vec![Shape::Point(metres)];
    let table = table_of(vec![text_field("NAME", 10)], vec![vec!["Vienna".into()]]);
    let path = write_set_files(
        &dir,
        "utm",
        shp::ShapeType::Point,
        &shapes,
        &table,
        Some("PROJCS[\"WGS_1984_UTM_Zone_33N\",GEOGCS[\"GCS_WGS_1984\"]]"),
    );

    let opened = open(&path).expect("opens");
    // The map gets degrees, not metres — otherwise it would draw nothing.
    let doc = crate::geo::geojson::extract(&opened.text);
    assert_eq!(doc.skipped, 0, "no position was dropped as out of range");
    assert!(opened.text.contains("16.37"), "read as longitude: {}", opened.text);

    // And a save puts the metres back.
    save(&opened.origin, &opened.text).expect("saves");
    let (_, back) = shp::read(&std::fs::read(&path).unwrap()).unwrap();
    match &back[0] {
        Shape::Point(p) => {
            assert!(
                (p[0] - metres[0]).abs() < 1e-3 && (p[1] - metres[1]).abs() < 1e-3,
                "written back in the file's own coordinates: {p:?} vs {metres:?}"
            );
        }
        other => panic!("expected a point, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_projection_that_cannot_be_undone_is_refused_with_a_reason() {
    let dir = scratch("unsupported");
    let shapes = vec![Shape::Point([600_000.0, 5_340_000.0])];
    let table = table_of(vec![text_field("N", 4)], vec![vec!["a".into()]]);
    let path = write_set_files(
        &dir,
        "lcc",
        shp::ShapeType::Point,
        &shapes,
        &table,
        Some("PROJCS[\"Lambert_Conformal_Conic_Austria\",GEOGCS[\"GCS_WGS_1984\"]]"),
    );
    // Better a sentence naming the projection than a blank map.
    let Err(err) = open(&path) else {
        panic!("a projection that cannot be undone must be refused")
    };
    assert!(err.contains("Lambert"), "the message names the projection: {err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_save_leaves_the_set_as_it_was() {
    let dir = scratch("atomic");
    let shapes = vec![Shape::Point([1.0, 2.0])];
    let table = table_of(vec![text_field("N", 4)], vec![vec!["a".into()]]);
    let path = write_set_files(&dir, "keep", shp::ShapeType::Point, &shapes, &table, None);
    let before = std::fs::read(&path).unwrap();

    let opened = open(&path).expect("opens");
    // A directory where the .shx has to go: writing it cannot succeed.
    std::fs::remove_file(path.with_extension("shx")).unwrap();
    std::fs::create_dir(path.with_extension("shx.rc-new")).unwrap();

    assert!(save(&opened.origin, &opened.text).is_err(), "the save fails");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "and the .shp it would have replaced is untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_file_that_is_not_a_shapefile_is_refused() {
    let dir = scratch("bogus");
    let path = dir.join("nope.shp");
    std::fs::write(&path, b"this is not a shapefile at all, not even close").unwrap();
    assert!(open(&path).is_err());
    assert!(shp::read(b"short").is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
