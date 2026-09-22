//! Turning a shapefile's coordinates into longitude and latitude, and back.
//!
//! The map this feeds draws WGS84 degrees and drops anything outside that range
//! (see `GeoDoc::skipped`), but a great many shapefiles are in a *projected*
//! system — metres on a grid — so without this they would open as an empty map
//! with no hint as to why.
//!
//! The two that matter in practice are implemented: Web Mercator, which is what
//! anything web-derived uses, and UTM, which is what most national and survey
//! data uses. Both are closed-form, so a round trip is exact to well under a
//! millimetre and an edit that touches one feature leaves the rest byte-identical.
//! Anything else is reported rather than guessed at.

/// WGS84 semi-major axis, in metres.
const A: f64 = 6_378_137.0;
/// WGS84 flattening.
const F: f64 = 1.0 / 298.257_223_563;
/// UTM's scale factor on the central meridian.
const K0: f64 = 0.9996;
/// The false easting every UTM zone uses.
const FALSE_EASTING: f64 = 500_000.0;
/// The false northing a southern-hemisphere UTM zone uses.
const FALSE_NORTHING: f64 = 10_000_000.0;

/// How a file's coordinates relate to longitude and latitude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
    /// Already degrees: nothing to do.
    Geographic,
    /// Spherical Mercator (EPSG:3857 and its many aliases).
    WebMercator,
    /// UTM on WGS84, in the given zone. `north` is false for a southern zone.
    Utm { zone: u8, north: bool },
}

impl Projection {
    /// A file position as `[longitude, latitude]`.
    pub fn to_lonlat(self, p: [f64; 2]) -> [f64; 2] {
        match self {
            Projection::Geographic => p,
            Projection::WebMercator => {
                let lon = (p[0] / A).to_degrees();
                let lat =
                    (2.0 * (p[1] / A).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
                [lon, lat]
            }
            Projection::Utm { zone, north } => utm_to_lonlat(p, zone, north),
        }
    }

    /// A longitude and latitude put back into the file's own coordinates.
    pub fn of_lonlat(self, p: [f64; 2]) -> [f64; 2] {
        match self {
            Projection::Geographic => p,
            Projection::WebMercator => {
                let x = p[0].to_radians() * A;
                let lat = p[1].to_radians().clamp(-1.484_22, 1.484_22); // ±85.05°
                let y = A * (std::f64::consts::FRAC_PI_4 + lat / 2.0).tan().ln();
                [x, y]
            }
            Projection::Utm { zone, north } => lonlat_to_utm(p, zone, north),
        }
    }
}

/// The eccentricity squared, and its companion used by the UTM series.
fn ecc() -> (f64, f64) {
    let e2 = F * (2.0 - F);
    (e2, e2 / (1.0 - e2))
}

/// The central meridian of a UTM zone, in degrees.
fn central_meridian(zone: u8) -> f64 {
    (zone as f64 - 1.0) * 6.0 - 180.0 + 3.0
}

/// UTM easting/northing to longitude and latitude (Snyder's series, as used by
/// every implementation of this projection).
fn utm_to_lonlat(p: [f64; 2], zone: u8, north: bool) -> [f64; 2] {
    let (e2, ep2) = ecc();
    let x = p[0] - FALSE_EASTING;
    let y = if north { p[1] } else { p[1] - FALSE_NORTHING };

    let m = y / K0;
    let mu = m / (A * (1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2 * e2 * e2 / 256.0));
    let e1 = (1.0 - (1.0 - e2).sqrt()) / (1.0 + (1.0 - e2).sqrt());
    let (e1_2, e1_3, e1_4) = (e1 * e1, e1 * e1 * e1, e1 * e1 * e1 * e1);
    let fp = mu
        + (3.0 * e1 / 2.0 - 27.0 * e1_3 / 32.0) * (2.0 * mu).sin()
        + (21.0 * e1_2 / 16.0 - 55.0 * e1_4 / 32.0) * (4.0 * mu).sin()
        + (151.0 * e1_3 / 96.0) * (6.0 * mu).sin()
        + (1097.0 * e1_4 / 512.0) * (8.0 * mu).sin();

    let (sin_fp, cos_fp, tan_fp) = (fp.sin(), fp.cos(), fp.tan());
    let c1 = ep2 * cos_fp * cos_fp;
    let t1 = tan_fp * tan_fp;
    let n1 = A / (1.0 - e2 * sin_fp * sin_fp).sqrt();
    let r1 = A * (1.0 - e2) / (1.0 - e2 * sin_fp * sin_fp).powf(1.5);
    let d = x / (n1 * K0);
    let (d2, d3, d4, d5, d6) = (d * d, d * d * d, d.powi(4), d.powi(5), d.powi(6));

    let lat = fp
        - (n1 * tan_fp / r1)
            * (d2 / 2.0 - (5.0 + 3.0 * t1 + 10.0 * c1 - 4.0 * c1 * c1 - 9.0 * ep2) * d4 / 24.0
                + (61.0 + 90.0 * t1 + 298.0 * c1 + 45.0 * t1 * t1 - 3.0 * c1 * c1 - 252.0 * ep2)
                    * d6
                    / 720.0);
    let lon = central_meridian(zone).to_radians()
        + (d - (1.0 + 2.0 * t1 + c1) * d3 / 6.0
            + (5.0 - 2.0 * c1 + 28.0 * t1 - 3.0 * c1 * c1 + 8.0 * ep2 + 24.0 * t1 * t1) * d5
                / 120.0)
            / cos_fp;
    [lon.to_degrees(), lat.to_degrees()]
}

/// Longitude and latitude to UTM easting/northing — the inverse of the above,
/// so a read followed by a write returns the coordinates the file had.
fn lonlat_to_utm(p: [f64; 2], zone: u8, north: bool) -> [f64; 2] {
    let (e2, ep2) = ecc();
    let lat = p[1].to_radians();
    let dlon = p[0].to_radians() - central_meridian(zone).to_radians();

    let (sin_lat, cos_lat, tan_lat) = (lat.sin(), lat.cos(), lat.tan());
    let n = A / (1.0 - e2 * sin_lat * sin_lat).sqrt();
    let t = tan_lat * tan_lat;
    let c = ep2 * cos_lat * cos_lat;
    let a1 = cos_lat * dlon;
    let (a2, a3, a4, a5, a6) = (a1 * a1, a1.powi(3), a1.powi(4), a1.powi(5), a1.powi(6));

    let m = A
        * ((1.0 - e2 / 4.0 - 3.0 * e2 * e2 / 64.0 - 5.0 * e2 * e2 * e2 / 256.0) * lat
            - (3.0 * e2 / 8.0 + 3.0 * e2 * e2 / 32.0 + 45.0 * e2 * e2 * e2 / 1024.0)
                * (2.0 * lat).sin()
            + (15.0 * e2 * e2 / 256.0 + 45.0 * e2 * e2 * e2 / 1024.0) * (4.0 * lat).sin()
            - (35.0 * e2 * e2 * e2 / 3072.0) * (6.0 * lat).sin());

    let easting = K0
        * n
        * (a1
            + (1.0 - t + c) * a3 / 6.0
            + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * a5 / 120.0)
        + FALSE_EASTING;
    let northing = K0
        * (m + n
            * tan_lat
            * (a2 / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * a4 / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * a6 / 720.0));
    let northing = if north { northing } else { northing + FALSE_NORTHING };
    [easting, northing]
}
