//! Maps: GeoJSON drawn over a world map built into the program.
//!
//! [`geojson`] finds the GeoJSON in a JSON document; [`world`] is the map
//! under it; [`view`] is where the map looks and how it projects; [`draw`] lays
//! everything onto a canvas with [`cover`]'s anti-aliasing, as pixels for a
//! graphics terminal or — through [`cells`] — as braille for any other.

pub mod cells;
pub mod cover;
pub mod draw;
pub mod edit;
pub mod geojson;
pub mod palette;
pub mod view;
pub mod world;
