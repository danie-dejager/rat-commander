//! The world map under the GeoJSON: land, lakes, borders, rivers and cities
//! from Natural Earth 1:10m, built into the program.
//!
//! `assets/world/make_world.py` bakes the geometry into `world.bin` (see its
//! header for the format): every layer at five levels of detail, each point a
//! short step from the one before, the whole thing zlib-compressed. It is read
//! once, on first use, and kept. A short or damaged file leaves the layers it
//! could not read empty — the map draws without them rather than not at all.

use std::io::Read;
use std::sync::LazyLock;

static BLOB: &[u8] = include_bytes!("../../assets/world/world.bin");

/// One ring of a polygon layer, or one line of a line layer.
pub struct Shape {
    /// A river's size (higher is bigger), a lake's importance (lower is
    /// bigger); 0 for land and borders.
    pub rank: u8,
    /// `[lon0, lat0, lon1, lat1]`.
    pub bbox: [f32; 4],
    /// `[lon, lat]` in degrees.
    pub pts: Box<[[f32; 2]]>,
}

/// A layer at one level of detail.
pub struct Level {
    /// How far a dropped vertex may have been from the simplified line, in
    /// degrees.
    pub eps: f32,
    pub shapes: Vec<Shape>,
}

/// A layer at every level of detail, finest first.
#[derive(Default)]
pub struct Layer {
    pub levels: Vec<Level>,
}

impl Layer {
    /// The level to draw at `deg_per_px` degrees to a pixel: the coarsest whose
    /// simplification stays under half a pixel.
    pub fn level(&self, deg_per_px: f64) -> Option<&Level> {
        self.levels
            .iter()
            .rev()
            .find(|l| f64::from(l.eps) <= deg_per_px * 0.5)
            .or_else(|| self.levels.first())
    }
}

/// A populated place.
pub struct City {
    pub lon: f32,
    pub lat: f32,
    pub pop: u32,
    pub capital: bool,
    pub name: Box<str>,
}

/// The whole map.
#[derive(Default)]
pub struct World {
    pub land: Layer,
    pub lakes: Layer,
    pub borders: Layer,
    pub rivers: Layer,
    /// Biggest first.
    pub cities: Vec<City>,
}

/// The map, read on first use.
pub fn world() -> &'static World {
    static WORLD: LazyLock<World> = LazyLock::new(|| read(BLOB));
    &WORLD
}

fn read(blob: &[u8]) -> World {
    let Some(rest) = blob.strip_prefix(b"RCWORLD1") else { return World::default() };
    let Some((len, packed)) = rest.split_at_checked(4) else { return World::default() };
    let len = u32::from_le_bytes(len.try_into().expect("four bytes")) as usize;
    let mut payload = Vec::with_capacity(len);
    // Whatever inflates is read, even if the stream is cut short.
    let _ = flate2::read::ZlibDecoder::new(packed).take(len as u64).read_to_end(&mut payload);
    let mut r = Reader { data: &payload, pos: 0 };
    let mut layers: Vec<Layer> = Vec::new();
    for _ in 0..r.u8().unwrap_or(0) {
        let Some(layer) = read_layer(&mut r) else { break };
        layers.push(layer);
    }
    let cities = read_cities(&mut r);
    let mut layers = layers.into_iter();
    World {
        land: layers.next().unwrap_or_default(),
        lakes: layers.next().unwrap_or_default(),
        borders: layers.next().unwrap_or_default(),
        rivers: layers.next().unwrap_or_default(),
        cities,
    }
}

fn read_layer(r: &mut Reader) -> Option<Layer> {
    let _kind = r.u8()?;
    let mut levels = Vec::new();
    for _ in 0..r.u8()? {
        let eps = r.f32()?;
        let count = r.u32()? as usize;
        let mut shapes = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            shapes.push(read_shape(r)?);
        }
        levels.push(Level { eps, shapes });
    }
    Some(Layer { levels })
}

fn read_shape(r: &mut Reader) -> Option<Shape> {
    let rank = r.u8()?;
    let n = r.u32()? as usize;
    let mut lat = f64::from(r.i32()?) / 1e5;
    let mut lon = f64::from(r.i32()?) / 1e5;
    let steps = r.take(4 * n.saturating_sub(1))?;
    let mut pts = Vec::with_capacity(n);
    let mut bbox = [lon as f32, lat as f32, lon as f32, lat as f32];
    pts.push([lon as f32, lat as f32]);
    for step in steps.as_chunks::<4>().0 {
        lat += f64::from(i16::from_le_bytes([step[0], step[1]])) * 1e-4;
        lon += f64::from(i16::from_le_bytes([step[2], step[3]])) * 1e-4;
        let p = [lon as f32, lat as f32];
        bbox = [bbox[0].min(p[0]), bbox[1].min(p[1]), bbox[2].max(p[0]), bbox[3].max(p[1])];
        pts.push(p);
    }
    Some(Shape { rank, bbox, pts: pts.into_boxed_slice() })
}

fn read_cities(r: &mut Reader) -> Vec<City> {
    let count = r.u32().unwrap_or(0) as usize;
    let mut out = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        let city = (|| {
            let lat = r.i32()? as f32 / 1e5;
            let lon = r.i32()? as f32 / 1e5;
            let pop = r.u32()?;
            let flags = r.u8()?;
            let n = r.u8()? as usize;
            let name = String::from_utf8_lossy(r.take(n)?).into();
            Some(City { lon, lat, pop, capital: flags & 1 != 0, name })
        })();
        match city {
            Some(c) => out.push(c),
            None => break,
        }
    }
    out
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.data.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|b| u32::from_le_bytes(b.try_into().expect("four bytes")))
    }

    fn i32(&mut self) -> Option<i32> {
        self.take(4).map(|b| i32::from_le_bytes(b.try_into().expect("four bytes")))
    }

    fn f32(&mut self) -> Option<f32> {
        self.take(4).map(|b| f32::from_le_bytes(b.try_into().expect("four bytes")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether (lon, lat) is inside a polygon layer's finest level, by the
    /// non-zero winding rule the map fills it with.
    fn inside(layer: &Layer, lon: f32, lat: f32) -> bool {
        let mut winding = 0i32;
        for s in &layer.levels[0].shapes {
            if lon < s.bbox[0] || lon > s.bbox[2] || lat < s.bbox[1] || lat > s.bbox[3] {
                continue;
            }
            let n = s.pts.len();
            for i in 0..n {
                let (a, b) = (s.pts[i], s.pts[(i + 1) % n]);
                if (a[1] <= lat) != (b[1] <= lat) {
                    let x = a[0] + (lat - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                    if x > lon {
                        winding += if b[1] > a[1] { 1 } else { -1 };
                    }
                }
            }
        }
        winding != 0
    }

    #[test]
    fn land_is_where_land_is_and_the_lakes_are_water() {
        let w = world();
        for (lon, lat) in [(-100.0, 40.0), (10.0, 50.0), (135.0, -25.0), (20.0, 0.0), (0.0, -80.0)]
        {
            assert!(inside(&w.land, lon, lat), "{lon},{lat} should be land");
        }
        for (lon, lat) in [(-140.0, 0.0), (-30.0, 30.0), (80.0, -40.0)] {
            assert!(!inside(&w.land, lon, lat), "{lon},{lat} should be sea");
        }
        // Lake Superior is a lake over the land; the Caspian, which Natural
        // Earth counts as sea, is no land at all.
        assert!(inside(&w.lakes, -87.5, 47.6), "Lake Superior should be a lake");
        assert!(inside(&w.land, -87.5, 47.6), "with land under it");
        assert!(!inside(&w.land, 51.0, 42.0), "the Caspian is not land");
    }

    #[test]
    fn every_layer_has_levels_finest_first_that_only_drop_vertices() {
        let w = world();
        for (name, layer) in
            [("land", &w.land), ("lakes", &w.lakes), ("borders", &w.borders), ("rivers", &w.rivers)]
        {
            assert_eq!(layer.levels.len(), 5, "{name}");
            let counts: Vec<usize> =
                layer.levels.iter().map(|l| l.shapes.iter().map(|s| s.pts.len()).sum()).collect();
            assert!(counts.windows(2).all(|c| c[0] > c[1]), "{name}: {counts:?}");
            assert!(layer.levels.windows(2).all(|l| l[0].eps < l[1].eps), "{name}");
            for s in layer.levels.iter().flat_map(|l| &l.shapes) {
                assert!(s.pts.len() >= 2);
                assert!(s.pts.iter().all(|p| (-181.0..=181.0).contains(&p[0]) && (-90.1..=90.1).contains(&p[1])));
            }
        }
    }

    #[test]
    fn the_level_follows_the_zoom() {
        let land = &world().land;
        assert_eq!(land.level(1e-6).unwrap().eps, land.levels[0].eps, "past the data, the finest");
        assert_eq!(land.level(10.0).unwrap().eps, land.levels[4].eps, "a world view, the coarsest");
        assert!(f64::from(land.level(0.1).unwrap().eps) <= 0.05);
    }

    #[test]
    fn the_city_table_is_sorted_and_names_the_big_places() {
        let cities = &world().cities;
        assert!(cities.len() > 5000, "{}", cities.len());
        assert!(cities.windows(2).all(|c| c[0].pop >= c[1].pop));
        assert!(cities.iter().all(|c| c.name.is_ascii() && !c.name.is_empty()));
        let tokyo = cities.iter().find(|c| &*c.name == "Tokyo").expect("Tokyo");
        assert!((tokyo.lat - 35.69).abs() < 0.3 && (tokyo.lon - 139.75).abs() < 0.3);
        assert!(cities.iter().take(80).any(|c| &*c.name == "London"));
        assert!(cities.iter().filter(|c| c.capital).count() > 150);
    }

    #[test]
    fn the_map_fits_its_budget_and_a_damaged_one_reads_as_empty() {
        assert!(BLOB.len() < 5 << 20, "world.bin grew to {} kB", BLOB.len() >> 10);
        let w = world();
        let vertices: usize = [&w.land, &w.lakes, &w.borders, &w.rivers]
            .iter()
            .flat_map(|l| &l.levels)
            .flat_map(|l| &l.shapes)
            .map(|s| s.pts.len() * std::mem::size_of::<[f32; 2]>())
            .sum();
        assert!(vertices < 32 << 20, "{} MB of vertices", vertices >> 20);
        for cut in [0, 8, 12, 100, BLOB.len() / 2] {
            let damaged = read(&BLOB[..cut]);
            assert!(damaged.land.levels.len() <= 5);
        }
        assert!(read(b"not a map").cities.is_empty());
    }
}
