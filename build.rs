//! Packs the vendored binary templates (`assets/templates/*.bt`) and JSON
//! Schemas (`assets/schemas/`) into compressed bundles that `src/bt/bundle.rs`
//! and `src/schema/bundle.rs` embed.
//!
//! Format: an 8-byte magic (`b"RCBT0001"`, `b"RCSC0001"`), a u32 entry count, then zlib of the entries, each a
//! u16 name length, the name, a u32 content length and the content (all
//! little-endian). Entries are sorted by name and line endings normalised to
//! LF, so the bundle — and its hash — is the same on every OS.

use std::io::Write;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    pack("assets/templates", &["bt"], b"RCBT0001", "templates");
    // The JSON Schemas the editor validates configuration files with, and
    // the catalog of which files each is for.
    pack("assets/schemas", &["json", "toml"], b"RCSC0001", "schemas");
}

/// Pack the files in `dir` with one of the extensions `exts` into
/// `OUT_DIR/{stem}.bin`, and their count and hash into `OUT_DIR/{stem}_meta.rs`.
fn pack(dir: &str, exts: &[&str], magic: &[u8; 8], stem: &str) {
    println!("cargo:rerun-if-changed={dir}");
    let dir = Path::new(dir);
    let mut entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .unwrap_or_else(|_| panic!("{}", dir.display()))
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let lower = name.to_ascii_lowercase();
            if !exts.iter().any(|ext| lower.ends_with(&format!(".{ext}"))) {
                return None;
            }
            let raw = std::fs::read(e.path()).ok()?;
            let mut text = Vec::with_capacity(raw.len());
            let mut i = 0;
            while i < raw.len() {
                if raw[i] == b'\r' {
                    text.push(b'\n');
                    if raw.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                } else {
                    text.push(raw[i]);
                }
                i += 1;
            }
            Some((name, text))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut payload = Vec::new();
    for (name, text) in &entries {
        payload.extend_from_slice(&(name.len() as u16).to_le_bytes());
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(&(text.len() as u32).to_le_bytes());
        payload.extend_from_slice(text);
    }
    // FNV-1a over the uncompressed entries: the deployed copy is refreshed
    // whenever this changes.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &payload {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }

    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&payload).expect("compress bundle");
    let compressed = z.finish().expect("compress bundle");

    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut bin = Vec::with_capacity(compressed.len() + 12);
    bin.extend_from_slice(magic);
    bin.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    bin.extend_from_slice(&compressed);
    std::fs::write(out.join(format!("{stem}.bin")), bin).expect("write bundle");
    std::fs::write(
        out.join(format!("{stem}_meta.rs")),
        format!(
            "/// FNV-1a hash of the bundled files.\npub const BUNDLE_HASH: u64 = {hash:#018x};\n\
             /// Number of bundled files.\npub const BUNDLE_COUNT: usize = {};\n",
            entries.len()
        ),
    )
    .expect("write bundle meta");
}
