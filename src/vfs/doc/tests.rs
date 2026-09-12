//! Tests for browsing a document as a directory.

use super::*;
use crate::vfs::VfsKind;
use tokio::io::AsyncReadExt;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn write(tag: &str, name: &str, body: &str) -> (Scratch, PathBuf) {
    let root = crate::util::temp::rc_temp_path(&format!("doctest-{tag}"));
    std::fs::create_dir_all(&root).unwrap();
    let p = root.join(name);
    std::fs::write(&p, body).unwrap();
    (Scratch(root), p)
}

fn path(syntax: Syntax, doc: &Path, inner: &str) -> VfsPath {
    VfsPath {
        scheme: syntax.scheme().into(),
        path: PathBuf::from(inner),
        container: Some(doc.to_path_buf()),
    }
}

async fn names(fs: &DocFs, doc: &Path, dir: &str) -> Vec<String> {
    let mut n: Vec<String> = fs
        .read_dir(&path(fs.syntax, doc, dir))
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    n.sort();
    n
}

async fn read_all(fs: &DocFs, p: &VfsPath) -> String {
    let mut r = fs.open_read(p).await.unwrap();
    let mut s = String::new();
    r.read_to_string(&mut s).await.unwrap();
    s
}

const JSON: &str = r#"{
  "name": "rat-commander",
  "version": 2,
  "nested": { "deep": { "leaf": "found me" } },
  "list": ["a", "b", "c"],
  "flag": true,
  "nothing": null
}"#;

#[tokio::test]
async fn a_json_object_browses_as_a_directory() {
    let (_s, doc) = write("json", "d.json", JSON);
    let fs = DocFs::new(Syntax::Json);
    let root = names(&fs, &doc, "/").await;
    assert_eq!(root, ["flag", "list", "name", "nested", "nothing", "version"], "{root:?}");
    assert_eq!(fs.stat(&path(Syntax::Json, &doc, "/nested")).await.unwrap().kind, VfsKind::Dir);
    assert_eq!(fs.stat(&path(Syntax::Json, &doc, "/name")).await.unwrap().kind, VfsKind::File);
}

/// The point of the whole feature: `cd` to a deeply nested setting and read it.
#[tokio::test]
async fn a_nested_scalar_reads_as_its_bare_value() {
    let (_s, doc) = write("nested", "d.json", JSON);
    let fs = DocFs::new(Syntax::Json);
    assert_eq!(names(&fs, &doc, "/nested/deep").await, ["leaf"]);
    let leaf = read_all(&fs, &path(Syntax::Json, &doc, "/nested/deep/leaf")).await;
    // Unquoted: viewing a string setting should show the string, not `"…"`.
    assert_eq!(leaf, "found me");
    assert_eq!(read_all(&fs, &path(Syntax::Json, &doc, "/version")).await, "2");
    assert_eq!(read_all(&fs, &path(Syntax::Json, &doc, "/flag")).await, "true");
    assert_eq!(read_all(&fs, &path(Syntax::Json, &doc, "/nothing")).await, "null");
}

/// A list read out of order would be worse than useless, so indices are padded
/// to sort the way they are numbered.
#[tokio::test]
async fn array_indices_sort_the_way_they_are_numbered() {
    let items: Vec<String> = (0..12).map(|i| format!("\"v{i}\"")).collect();
    let (_s, doc) = write("array", "d.json", &format!("{{\"xs\": [{}]}}", items.join(",")));
    let fs = DocFs::new(Syntax::Json);
    let xs = names(&fs, &doc, "/xs").await;
    assert_eq!(xs.first().map(String::as_str), Some("00"));
    assert_eq!(xs.last().map(String::as_str), Some("11"));
    // Sorted by name, `02` still precedes `10`.
    let mut sorted = xs.clone();
    sorted.sort();
    assert_eq!(xs, sorted, "name order is index order: {xs:?}");
    assert_eq!(read_all(&fs, &path(Syntax::Json, &doc, "/xs/05")).await, "v5");
}

/// A container is a directory, and behaves like one: no contents of its own, no
/// size. Rendering each one's subtree would cost a copy of the document per
/// level of nesting, for bytes the panel cannot reach anyway.
#[tokio::test]
async fn a_container_is_a_directory_with_nothing_of_its_own() {
    let (_s, doc) = write("subtree", "d.json", JSON);
    let fs = DocFs::new(Syntax::Json);
    let nested = fs.stat(&path(Syntax::Json, &doc, "/nested")).await.unwrap();
    assert_eq!(nested.kind, VfsKind::Dir);
    assert_eq!(nested.size, 0, "a directory has no size, here as anywhere");
    let err = match fs.open_read(&path(Syntax::Json, &doc, "/nested")).await {
        Ok(_) => panic!("a container is not readable as a file"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("is a directory"), "{err}");
}

/// A key with a slash in it would otherwise invent a level that is not in the
/// document.
#[tokio::test]
async fn a_key_containing_a_separator_is_made_safe() {
    let (_s, doc) = write("slash", "d.json", r#"{"a/b": 1, "c\\d": 2}"#);
    let fs = DocFs::new(Syntax::Json);
    let root = names(&fs, &doc, "/").await;
    assert!(root.iter().all(|n| !n.contains('/') && !n.contains('\\')), "{root:?}");
    assert_eq!(root.len(), 2, "and the two keys stay distinct: {root:?}");
}

const TOML: &str = r#"
# a comment, which is why this is read-only
name = "rat-commander"
version = 2

[package.metadata.deb]
depends = ["libc6", "zlib1g"]
priority = "optional"
"#;

#[tokio::test]
async fn a_toml_document_browses_as_nested_tables() {
    let (_s, doc) = write("toml", "d.toml", TOML);
    let fs = DocFs::new(Syntax::Toml);
    let root = names(&fs, &doc, "/").await;
    assert_eq!(root, ["name", "package", "version"], "{root:?}");
    assert_eq!(names(&fs, &doc, "/package/metadata/deb").await, ["depends", "priority"]);
    let dep = read_all(&fs, &path(Syntax::Toml, &doc, "/package/metadata/deb/depends/0")).await;
    assert_eq!(dep, "libc6");
    assert_eq!(read_all(&fs, &path(Syntax::Toml, &doc, "/version")).await, "2");
}

/// Read-only, and the comment in the fixture above is exactly why: writing one
/// value back would re-serialise the document and lose it.
#[tokio::test]
async fn a_document_is_read_only() {
    let (_s, doc) = write("ro", "d.toml", TOML);
    let fs = DocFs::new(Syntax::Toml);
    let p = path(Syntax::Toml, &doc, "/name");
    assert!(!fs.capabilities().writable);
    assert!(matches!(
        fs.open_write(&p, WriteMeta::default()).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(fs.mkdir(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.remove_file(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.rename(&p, &p).await.err(), Some(Error::Unsupported)));
}

/// A document that does not parse is declined, so Enter falls through and it
/// opens in the editor — where it can be fixed — rather than half-listing.
#[tokio::test]
async fn a_broken_document_is_declined_rather_than_half_parsed() {
    let (_s, doc) = write("broken", "d.json", "{ this is not json");
    assert!(!parses_as(Syntax::Json, &doc));
    let fs = DocFs::new(Syntax::Json);
    let err = fs.read_dir(&path(Syntax::Json, &doc, "/")).await.unwrap_err();
    assert!(err.to_string().contains("json"), "and says what went wrong: {err}");
}

#[tokio::test]
async fn a_valid_document_passes_its_own_probe_and_not_the_others() {
    let (_s, json) = write("probe", "d.json", JSON);
    assert!(parses_as(Syntax::Json, &json));
    assert!(!parses_as(Syntax::Toml, &json), "a JSON object is not TOML");

    let (_s2, toml_doc) = write("probe2", "d.toml", TOML);
    assert!(parses_as(Syntax::Toml, &toml_doc));
}

#[tokio::test]
async fn an_empty_document_has_a_root_but_nothing_in_it() {
    let (_s, doc) = write("empty", "d.json", "{}");
    let fs = DocFs::new(Syntax::Json);
    assert!(names(&fs, &doc, "/").await.is_empty());
    // The root is a directory, not a file.
    assert!(fs.open_read(&path(Syntax::Json, &doc, "/")).await.is_err());
}

/// A top-level array is a document too, not just an object.
#[tokio::test]
async fn a_top_level_array_browses() {
    let (_s, doc) = write("toparray", "d.json", r#"[{"a":1},{"a":2}]"#);
    let fs = DocFs::new(Syntax::Json);
    assert_eq!(names(&fs, &doc, "/").await, ["0", "1"]);
    assert_eq!(read_all(&fs, &path(Syntax::Json, &doc, "/1/a")).await, "2");
}

#[test]
fn indices_are_padded_to_the_width_the_array_needs() {
    assert_eq!(index_name(5, 12), "05");
    assert_eq!(index_name(5, 9), "5");
    assert_eq!(index_name(5, 1000), "005");
}

#[test]
fn a_missing_path_is_not_found() {
    let (_s, doc) = write("missing", "d.json", JSON);
    let fs = DocFs::new(Syntax::Json);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        assert!(matches!(
            fs.stat(&path(Syntax::Json, &doc, "/nope")).await.err(),
            Some(Error::NotFound(_))
        ));
        // A scalar is a file, so it has no listing.
        assert!(fs.read_dir(&path(Syntax::Json, &doc, "/name")).await.is_err());
    });
}
