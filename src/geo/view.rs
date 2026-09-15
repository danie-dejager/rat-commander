//! Where the map is looking, and how a longitude and latitude land on it.
//!
//! The projection is plate carrée corrected for the latitude in the middle of
//! the view: a degree of longitude is drawn `cos(latitude)` as wide as a degree
//! of latitude. Zoomed in on a region that is very nearly the true shape — a
//! country at 60° north is not stretched to twice its width — and zoomed out
//! to the whole world it is the familiar rectangular map.
//!
//! The zoom is kept as how many degrees *of latitude* the width of the view
//! covers, rather than as a pixel scale, so resizing the terminal keeps the
//! same stretch of the world in view, and panning north or south never zooms.

use super::geojson::Bounds;

/// Narrowest the view gets, in degrees across: about 20 m, enough for a
/// building's outline.
pub const MIN_WIDTH: f64 = 0.0002;
/// Widest: the whole world with some to spare.
pub const MAX_WIDTH: f64 = 400.0;
/// The centre stays off the poles, where a degree of longitude has no width.
const MAX_CENTER_LAT: f64 = 85.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapView {
    pub clon: f64,
    pub clat: f64,
    /// Degrees of latitude the view's width covers.
    pub width: f64,
}

impl Default for MapView {
    fn default() -> Self {
        MapView { clon: 0.0, clat: 20.0, width: 360.0 }
    }
}

/// A view laid onto a canvas of `w` × `h` square pixels.
#[derive(Debug, Clone, Copy)]
pub struct Projection {
    pub w: f64,
    pub h: f64,
    pub clon: f64,
    pub clat: f64,
    /// Pixels to a degree of latitude.
    pub k: f64,
    /// Pixels to a degree of longitude.
    pub kx: f64,
}

impl Projection {
    /// Canvas position of (`lon`, `lat`), with `lon` taken as it is — the
    /// caller adds a whole turn for the copy of the world it is drawing.
    pub fn xy(&self, lon: f64, lat: f64) -> (f32, f32) {
        (
            (self.w * 0.5 + (lon - self.clon) * self.kx) as f32,
            (self.h * 0.5 - (lat - self.clat) * self.k) as f32,
        )
    }

    /// The longitude and latitude under canvas position (`x`, `y`).
    pub fn lonlat(&self, x: f64, y: f64) -> (f64, f64) {
        (wrap180(self.clon + (x - self.w * 0.5) / self.kx), self.clat - (y - self.h * 0.5) / self.k)
    }

    /// Degrees of latitude to a pixel: what picks a level of detail.
    pub fn deg_per_px(&self) -> f64 {
        1.0 / self.k
    }

    /// The longitudes in view, `[west, east]`, unwrapped around the centre.
    pub fn lon_range(&self) -> (f64, f64) {
        let half = self.w * 0.5 / self.kx;
        (self.clon - half, self.clon + half)
    }

    /// The latitudes in view, `[south, north]`.
    pub fn lat_range(&self) -> (f64, f64) {
        let half = self.h * 0.5 / self.k;
        (self.clat - half, self.clat + half)
    }

    /// Whole turns to add to a box `[lon0, lon1]` for each copy of it that is
    /// in view: one normally, two or three when the view spans the date line
    /// or more than a whole world.
    pub fn copies(&self, lon0: f64, lon1: f64) -> impl Iterator<Item = f64> {
        let (west, east) = self.lon_range();
        let first = ((west - lon1) / 360.0).ceil() as i32;
        let last = ((east - lon0) / 360.0).floor() as i32;
        (first..=last.max(first - 1)).map(|k| f64::from(k) * 360.0)
    }
}

impl MapView {
    /// This view on a `w` × `h` canvas.
    pub fn project(&self, w: u32, h: u32) -> Projection {
        let k = f64::from(w.max(1)) / self.width;
        let kx = k * self.clat.to_radians().cos().max(0.05);
        Projection {
            w: f64::from(w.max(1)),
            h: f64::from(h.max(1)),
            clon: self.clon,
            clat: self.clat,
            k,
            kx,
        }
    }

    /// Keep the view legal: the zoom inside its limits, the centre off the
    /// poles, and the longitude wrapped.
    pub fn clamp(&mut self) {
        self.width = self.width.clamp(MIN_WIDTH, MAX_WIDTH);
        self.clat = self.clat.clamp(-MAX_CENTER_LAT, MAX_CENTER_LAT);
        self.clon = wrap180(self.clon);
    }

    /// Move the view by (`dx`, `dy`) pixels of a `w` × `h` canvas — the map
    /// under the pointer follows the pointer. The place that was that far from
    /// the middle becomes the middle, which stays exact although the width of
    /// a degree of longitude changes with the latitude.
    pub fn pan(&mut self, dx: f64, dy: f64, w: u32, h: u32) {
        let p = self.project(w, h);
        let (lon, lat) = p.lonlat(p.w * 0.5 - dx, p.h * 0.5 - dy);
        self.clon = lon;
        self.clat = lat;
        self.clamp();
    }

    /// Zoom by `factor` (below 1 zooms in) about canvas position (`x`, `y`),
    /// keeping the place under it where it is.
    pub fn zoom_about(&mut self, factor: f64, x: f64, y: f64, w: u32, h: u32) {
        let p = self.project(w, h);
        let (lon, lat) = p.lonlat(x, y);
        let lon = lon + unwrap_near(lon, self.clon);
        self.width = (self.width * factor).clamp(MIN_WIDTH, MAX_WIDTH);
        // Solve for the centre that puts (lon, lat) back under (x, y): the
        // latitude first, since the longitude scale depends on it.
        let k = p.w / self.width;
        self.clat = (lat + (y - p.h * 0.5) / k).clamp(-MAX_CENTER_LAT, MAX_CENTER_LAT);
        let kx = k * self.clat.to_radians().cos().max(0.05);
        self.clon = lon - (x - p.w * 0.5) / kx;
        self.clamp();
    }

    /// A view of `b` on a `w` × `h` canvas, with a margin around it. A point,
    /// or a box too small to frame, gets a regional view around it.
    pub fn fit(b: &Bounds, w: u32, h: u32) -> MapView {
        let clat = ((b.lat0 + b.lat1) * 0.5).clamp(-MAX_CENTER_LAT, MAX_CENTER_LAT);
        let cos = clat.to_radians().cos().max(0.05);
        let aspect = f64::from(w.max(1)) / f64::from(h.max(1));
        let across = ((b.lon1 - b.lon0) * cos).max((b.lat1 - b.lat0) * aspect);
        let width = if across <= 0.0 { 2.0 } else { across * 1.25 };
        let mut v = MapView { clon: (b.lon0 + b.lon1) * 0.5, clat, width };
        v.clamp();
        v
    }

    /// A signature of the view for the graphics cache, fine enough that a
    /// change of a tenth of a pixel on a large canvas still redraws.
    pub fn sig(&self) -> [i64; 3] {
        let q = |v: f64| (v * 1e6).round() as i64;
        [q(self.clon), q(self.clat), q(self.width.ln())]
    }
}

/// `d` wrapped into `[-180, 180)`.
pub fn wrap180(d: f64) -> f64 {
    (d + 180.0).rem_euclid(360.0) - 180.0
}

/// The whole turns to add to `lon` to bring it within half a turn of `center`.
fn unwrap_near(lon: f64, center: f64) -> f64 {
    ((center - lon) / 360.0).round() * 360.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zoom_keeps_the_place_under_the_pointer_in_place() {
        let (w, h) = (800, 400);
        for (x, y) in [(400.0, 200.0), (100.0, 50.0), (700.0, 390.0)] {
            let mut v = MapView { clon: 10.0, clat: 45.0, width: 40.0 };
            let before = v.project(w, h).lonlat(x, y);
            v.zoom_about(0.5, x, y, w, h);
            let after = v.project(w, h).lonlat(x, y);
            assert!(
                (before.0 - after.0).abs() < 1e-6 && (before.1 - after.1).abs() < 1e-6,
                "{before:?} {after:?}"
            );
            assert!((v.width - 20.0).abs() < 1e-9);
        }
    }

    #[test]
    fn panning_follows_the_pointer_and_wraps_round_the_world() {
        let (w, h) = (360, 180);
        let mut v = MapView { clon: 170.0, clat: 0.0, width: 360.0 };
        let (lon, lat) = v.project(w, h).lonlat(200.0, 90.0);
        v.pan(-30.0, 0.0, w, h);
        let (lon2, lat2) = v.project(w, h).lonlat(170.0, 90.0);
        assert!((wrap180(lon - lon2)).abs() < 1e-9 && (lat - lat2).abs() < 1e-9);
        assert!(v.clon < 0.0, "past 180 the centre wraps to the west: {}", v.clon);
        v.pan(0.0, 1e9, w, h);
        assert_eq!(v.clat, MAX_CENTER_LAT);
    }

    #[test]
    fn fitting_frames_the_box_with_room_to_spare() {
        let b = Bounds { lon0: 5.0, lat0: 45.0, lon1: 17.0, lat1: 55.0 };
        let v = MapView::fit(&b, 800, 400);
        let p = v.project(800, 400);
        let (west, east) = p.lon_range();
        let (south, north) = p.lat_range();
        assert!(west < 5.0 && east > 17.0 && south < 45.0 && north > 55.0, "{v:?}");
        let point = MapView::fit(&Bounds { lon0: 1.0, lat0: 2.0, lon1: 1.0, lat1: 2.0 }, 80, 40);
        assert!(point.width > 0.5, "a point gets a view around it");
    }

    #[test]
    fn copies_of_a_box_are_found_wherever_the_view_sees_them() {
        let v = MapView { clon: 175.0, clat: 0.0, width: 40.0 };
        let p = v.project(400, 200);
        // A box just east of the date line shows once, a turn to the east.
        let offs: Vec<f64> = p.copies(-179.0, -170.0).collect();
        assert_eq!(offs, [360.0]);
        let offs: Vec<f64> = p.copies(170.0, 179.0).collect();
        assert_eq!(offs, [0.0]);
        let world = MapView { clon: 0.0, clat: 0.0, width: 400.0 }.project(400, 200);
        assert!(world.copies(-180.0, 180.0).count() >= 1);
        assert_eq!(p.copies(0.0, 10.0).count(), 0, "out of view");
    }
}
