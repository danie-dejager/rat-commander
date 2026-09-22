//! Reading the `.prj` beside a shapefile — the WKT that says what its numbers
//! mean — well enough to know whether they are degrees, and if not, which
//! projection would turn them into degrees.
//!
//! This is not a WKT parser. It looks for the few things that decide the
//! question, because the alternative (assuming degrees) opens a projected file
//! as a blank map, and the alternative to *that* (a full parser plus a datum
//! database) is a different program.

use super::project::Projection;

/// What a `.prj` turned out to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Crs {
    /// Degrees, or no `.prj` at all — which conventionally means degrees.
    Geographic,
    /// A projection that can be undone here.
    Projected(Projection),
    /// A projection that cannot, with its name for the message.
    Unsupported(String),
}

impl Crs {
    /// The projection to read through, or `None` when the file cannot be shown.
    pub fn projection(&self) -> Option<Projection> {
        match self {
            Crs::Geographic => Some(Projection::Geographic),
            Crs::Projected(p) => Some(*p),
            Crs::Unsupported(_) => None,
        }
    }
}

/// Classify a `.prj`'s contents. An absent file is [`Crs::Geographic`], which
/// is the long-standing convention for a shapefile without one.
pub fn classify(wkt: &str) -> Crs {
    let t = wkt.trim();
    if t.is_empty() {
        return Crs::Geographic;
    }
    let upper = t.to_ascii_uppercase();
    // A bare GEOGCS is already degrees.
    if !upper.contains("PROJCS") {
        return Crs::Geographic;
    }
    let name = projcs_name(t).unwrap_or_else(|| "this projection".to_string());
    let uname = name.to_ascii_uppercase();

    // Web Mercator, under all the names it goes by.
    if uname.contains("WEB_MERCATOR")
        || uname.contains("WEB MERCATOR")
        || uname.contains("PSEUDO-MERCATOR")
        || uname.contains("PSEUDO_MERCATOR")
        || uname.contains("3857")
        || uname.contains("900913")
    {
        return Crs::Projected(Projection::WebMercator);
    }

    // UTM: the zone is in the name, and the hemisphere with it.
    if let Some(zone) = utm_zone(&uname) {
        let north = !(uname.contains("SOUTH") || uname.ends_with('S') || uname.contains("_S"));
        // Only WGS84 is undone here; another datum would shift the result by
        // enough to matter, so it is reported instead of quietly mis-placing.
        if uname.contains("WGS_1984") || uname.contains("WGS84") || uname.contains("WGS 84") {
            return Crs::Projected(Projection::Utm { zone, north });
        }
    }
    Crs::Unsupported(name)
}

/// The quoted name straight after `PROJCS[`.
fn projcs_name(wkt: &str) -> Option<String> {
    let at = wkt.to_ascii_uppercase().find("PROJCS")?;
    let rest = &wkt[at..];
    let open = rest.find('"')? + 1;
    let close = rest[open..].find('"')? + open;
    Some(rest[open..close].to_string())
}

/// The UTM zone named in `upper`, if it names one.
fn utm_zone(upper: &str) -> Option<u8> {
    let at = upper.find("UTM")?;
    let rest = &upper[at..];
    // "UTM_ZONE_33N", "UTM ZONE 33N", "UTM33N" — take the first number after it.
    let digits: String =
        rest.chars().skip_while(|c| !c.is_ascii_digit()).take_while(char::is_ascii_digit).collect();
    let zone: u8 = digits.parse().ok()?;
    (1..=60).contains(&zone).then_some(zone)
}
