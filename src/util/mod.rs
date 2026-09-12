//! Cross-cutting utilities: error type, formatting, async plumbing.

pub mod async_bridge;
pub mod bytes;
pub mod checksum;
pub mod clipboard;
pub mod error;
pub mod filetype;
pub mod http;
pub mod img;
pub mod qr;
pub mod rng;
pub mod scroll;
pub mod sysinfo;
pub mod temp;
pub mod text;
pub mod treemap;

pub use error::{Error, Result};
