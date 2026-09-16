//! The JSON Schemas built into the program (`assets/schemas/`, packed by
//! `build.rs`), and the catalog of which files each one is for.

use std::sync::LazyLock;

mod meta {
    #![allow(dead_code)] // the hash is for deployed copies, which schemas don't have
    include!(concat!(env!("OUT_DIR"), "/schemas_meta.rs"));
}
use meta::BUNDLE_COUNT;

static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/schemas.bin"));

static ENTRIES: LazyLock<Vec<crate::util::bundle::Entry>> =
    LazyLock::new(|| crate::util::bundle::unpack(PACKED, b"RCSC0001", BUNDLE_COUNT));

/// A bundled schema: its file, the URL it is published at, and the files it
/// is for (globs matched against the whole path).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Known {
    pub file: String,
    pub url: String,
    pub files: Vec<String>,
}

#[derive(serde::Deserialize)]
struct Catalog {
    #[serde(default)]
    schema: Vec<Known>,
}

static CATALOG: LazyLock<Vec<Known>> = LazyLock::new(|| {
    get("catalog.toml")
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|t| toml::from_str::<Catalog>(t).ok())
        .map(|c| c.schema)
        .unwrap_or_default()
});

/// The bundled file named `name`.
pub fn get(name: &str) -> Option<&'static [u8]> {
    ENTRIES.iter().find(|(n, _)| &**n == name).map(|(_, b)| &**b)
}

/// Every bundled schema.
pub fn catalog() -> &'static [Known] {
    &CATALOG
}

/// The bundled schema published at `url` (a trailing `#` aside).
pub fn by_url(url: &str) -> Option<&'static Known> {
    let url = url.trim_end_matches('#');
    catalog().iter().find(|k| k.url == url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogued_schema_is_bundled_and_is_json() {
        assert!(catalog().len() >= 8, "{:?}", catalog());
        for k in catalog() {
            let bytes = get(&k.file).unwrap_or_else(|| panic!("{} isn't bundled", k.file));
            serde_json::from_slice::<serde_json::Value>(bytes)
                .unwrap_or_else(|e| panic!("{} isn't JSON: {e}", k.file));
            assert!(!k.files.is_empty());
        }
        assert_eq!(
            by_url("https://json.schemastore.org/cargo.json#").map(|k| k.file.as_str()),
            Some("cargo.json")
        );
    }
}
