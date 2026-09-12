//! Tests for browsing a SQLite database as a directory.
//!
//! The questions worth asking of this backend are not "does it read a row" but
//! "what is a row *called*", and "what happens to a table with a million of
//! them" — so that is most of what is here.

use super::*;
use tokio::io::AsyncReadExt;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Build a database from `stmts` and return the path to it.
fn db_with(tag: &str, stmts: &[&str]) -> (Scratch, PathBuf) {
    let root = crate::util::temp::rc_temp_path(&format!("sqlitetest-{tag}"));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("test.db");
    let db = Connection::open(&path).unwrap();
    for s in stmts {
        db.execute_batch(s).unwrap();
    }
    drop(db);
    (Scratch(root), path)
}

fn path(db: &Path, inner: &str) -> VfsPath {
    VfsPath {
        scheme: "sqlite".into(),
        path: PathBuf::from(inner),
        container: Some(db.to_path_buf()),
    }
}

async fn names(fs: &SqliteFs, db: &Path, dir: &str) -> Vec<String> {
    fs.read_dir(&path(db, dir)).await.unwrap().into_iter().map(|e| e.name).collect()
}

async fn read_all(fs: &SqliteFs, p: &VfsPath) -> String {
    let mut r = fs.open_read(p).await.unwrap();
    let mut s = String::new();
    r.read_to_string(&mut s).await.unwrap();
    s
}

const PEOPLE: &str = "CREATE TABLE people (name TEXT, age INTEGER);
     INSERT INTO people VALUES ('ada', 36), ('grace', 45), ('alan', 41);";

#[tokio::test]
async fn the_root_lists_the_tables_and_the_schema() {
    let (_s, db) = db_with("root", &[PEOPLE, "CREATE VIEW adults AS SELECT * FROM people;"]);
    let fs = SqliteFs::new();
    let root = names(&fs, &db, "/").await;
    assert!(root.contains(&SCHEMA.to_string()), "{root:?}");
    assert!(root.contains(&"people".to_string()), "{root:?}");
    assert!(root.contains(&"adults".to_string()), "a view browses like a table: {root:?}");
    // SQLite's own bookkeeping tables are not the user's data.
    assert!(!root.iter().any(|n| n.starts_with("sqlite_")), "{root:?}");
}

#[tokio::test]
async fn the_schema_file_holds_the_statements_that_would_rebuild_it() {
    let (_s, db) = db_with("schema", &[PEOPLE]);
    let fs = SqliteFs::new();
    let sql = read_all(&fs, &path(&db, "/_schema.sql")).await;
    assert!(sql.contains("CREATE TABLE people"), "{sql}");
    assert!(sql.trim_end().ends_with(';'), "statements are terminated: {sql}");
}

/// An ordinary table's rows are named by rowid, which is unique and stable.
#[tokio::test]
async fn rows_of_an_ordinary_table_are_named_by_rowid() {
    let (_s, db) = db_with("rowid", &[PEOPLE]);
    let fs = SqliteFs::new();
    let rows = names(&fs, &db, "/people").await;
    assert_eq!(rows, vec!["1", "2", "3"], "got {rows:?}");

    let text = read_all(&fs, &path(&db, "/people/2")).await;
    assert!(text.contains("name = grace"), "{text}");
    assert!(text.contains("age = 45"), "{text}");
}

/// A `WITHOUT ROWID` table has no rowid to name rows by, so the primary key does
/// it — the case that makes "rows as files" more than a one-liner.
#[tokio::test]
async fn rows_of_a_without_rowid_table_are_named_by_their_primary_key() {
    let (_s, db) = db_with(
        "withoutrowid",
        &["CREATE TABLE kv (k TEXT PRIMARY KEY, v TEXT) WITHOUT ROWID;
           INSERT INTO kv VALUES ('alpha','1'), ('beta','2');"],
    );
    let fs = SqliteFs::new();
    let mut rows = names(&fs, &db, "/kv").await;
    rows.sort();
    assert_eq!(rows, vec!["alpha", "beta"], "got {rows:?}");
    assert!(read_all(&fs, &path(&db, "/kv/beta")).await.contains("v = 2"));
}

#[tokio::test]
async fn a_composite_primary_key_joins_its_columns() {
    let (_s, db) = db_with(
        "composite",
        &["CREATE TABLE edge (a TEXT, b TEXT, w INT, PRIMARY KEY (a,b)) WITHOUT ROWID;
           INSERT INTO edge VALUES ('x','y',1);"],
    );
    let fs = SqliteFs::new();
    assert_eq!(names(&fs, &db, "/edge").await, vec!["x-y"]);
    assert!(read_all(&fs, &path(&db, "/edge/x-y")).await.contains("w = 1"));
}

/// A view has no key at all, so its rows are numbered by position.
#[tokio::test]
async fn rows_of_a_view_are_numbered_by_position() {
    let (_s, db) =
        db_with("view", &[PEOPLE, "CREATE VIEW older AS SELECT * FROM people WHERE age > 38;"]);
    let fs = SqliteFs::new();
    let rows = names(&fs, &db, "/older").await;
    assert_eq!(rows, vec!["0", "1"], "got {rows:?}");
    assert!(read_all(&fs, &path(&db, "/older/0")).await.contains("age = "));
}

/// A key with a slash in it would otherwise invent a directory level.
#[tokio::test]
async fn a_key_containing_a_path_separator_is_made_safe() {
    let (_s, db) = db_with(
        "slash",
        &["CREATE TABLE p (k TEXT PRIMARY KEY) WITHOUT ROWID;
           INSERT INTO p VALUES ('a/b'), ('c\\\\d');"],
    );
    let fs = SqliteFs::new();
    let rows = names(&fs, &db, "/p").await;
    assert!(rows.iter().all(|n| !n.contains('/') && !n.contains('\\')), "got {rows:?}");
}

/// The reason this backend queries rather than building a tree: a table nobody
/// could list must still open instantly, and paging is how.
#[tokio::test]
async fn a_large_table_is_paged_rather_than_listed_whole() {
    let (_s, db) = db_with(
        "paged",
        &["CREATE TABLE big (v INT);
           WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 2500)
           INSERT INTO big SELECT i FROM n;"],
    );
    let fs = SqliteFs::new();
    let pages = names(&fs, &db, "/big").await;
    assert_eq!(pages.len(), 3, "2500 rows in pages of 1000: {pages:?}");
    assert!(pages[0].contains('-'), "a page is named for the span it holds: {pages:?}");
    // Names are zero-padded, so sorting by name sorts by number.
    assert!(pages[0] < pages[1] && pages[1] < pages[2], "{pages:?}");

    let first = names(&fs, &db, &format!("/big/{}", pages[0])).await;
    assert_eq!(first.len(), PAGE_ROWS);
    let last = names(&fs, &db, &format!("/big/{}", pages[2])).await;
    assert_eq!(last.len(), 500, "the final page is short");

    // And a row inside a page still reads — addressed within the page that
    // holds it, since that is the directory it appears in.
    let second = names(&fs, &db, &format!("/big/{}", pages[1])).await;
    let row = read_all(&fs, &path(&db, &format!("/big/{}/{}", pages[1], second[0]))).await;
    assert!(row.starts_with("v = "), "{row}");
    // A row from another page is not in this one.
    assert!(fs.stat(&path(&db, &format!("/big/{}/{}", pages[1], first[0]))).await.is_err());
}

#[tokio::test]
async fn a_blob_is_described_rather_than_dumped() {
    let (_s, db) =
        db_with("blob", &["CREATE TABLE b (data BLOB); INSERT INTO b VALUES (randomblob(200));"]);
    let fs = SqliteFs::new();
    let text = read_all(&fs, &path(&db, "/b/1")).await;
    assert!(text.contains("<blob, 200 bytes>"), "{text}");
    assert!(text.ends_with("…\n"), "the preview is truncated: {text}");
    // Binary must not be pasted into what a text viewer will show.
    assert!(!text.bytes().any(|c| c < 9), "{text:?}");
}

#[tokio::test]
async fn a_null_reads_as_null_rather_than_as_nothing() {
    let (_s, db) = db_with("null", &["CREATE TABLE t (a, b); INSERT INTO t VALUES (NULL, 1);"]);
    let fs = SqliteFs::new();
    let text = read_all(&fs, &path(&db, "/t/1")).await;
    assert!(text.contains("a = NULL"), "{text}");
}

#[tokio::test]
async fn an_empty_table_lists_nothing_rather_than_failing() {
    let (_s, db) = db_with("empty", &["CREATE TABLE blank (a);"]);
    let fs = SqliteFs::new();
    assert!(names(&fs, &db, "/blank").await.is_empty());
}

#[tokio::test]
async fn a_database_is_read_only() {
    let (_s, db) = db_with("ro", &[PEOPLE]);
    let fs = SqliteFs::new();
    let p = path(&db, "/people/1");
    assert!(!fs.capabilities().writable);
    assert!(matches!(
        fs.open_write(&p, WriteMeta::default()).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(fs.mkdir(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.remove_file(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.rename(&p, &p).await.err(), Some(Error::Unsupported)));
}

#[tokio::test]
async fn missing_tables_and_rows_are_not_found() {
    let (_s, db) = db_with("missing", &[PEOPLE]);
    let fs = SqliteFs::new();
    assert!(fs.read_dir(&path(&db, "/nosuch")).await.is_err());
    assert!(fs.stat(&path(&db, "/people/999")).await.is_err());
    // A table is a directory, not a file.
    assert!(fs.open_read(&path(&db, "/people")).await.is_err());
}

/// Reading a database another process holds open must neither block nor alter
/// it, which is what `immutable=1` buys.
#[tokio::test]
async fn an_open_database_can_still_be_browsed() {
    let (_s, db) = db_with("open", &[PEOPLE]);
    let writer = Connection::open(&db).unwrap();
    writer.execute_batch("BEGIN; INSERT INTO people VALUES ('held', 1);").unwrap();

    let fs = SqliteFs::new();
    let rows = names(&fs, &db, "/people").await;
    assert!(rows.len() >= 3, "the committed rows are readable: {rows:?}");
    drop(writer);
}

#[test]
fn the_probe_matches_the_header_magic_not_the_extension() {
    let (_s, db) = db_with("probe", &[PEOPLE]);
    assert!(looks_like_sqlite(&db));

    let other = db.parent().unwrap().join("notes.db");
    std::fs::write(&other, b"this is not a database").unwrap();
    assert!(!looks_like_sqlite(&other), "a `.db` that is not one is declined");
}

#[test]
fn identifiers_are_quoted_against_injection() {
    assert_eq!(quote("plain"), "\"plain\"");
    // A table named with a quote must not end the identifier early.
    assert_eq!(quote("we\"ird"), "\"we\"\"ird\"");
}

#[test]
fn names_are_zero_padded_so_name_order_is_number_order() {
    assert_eq!(padded(7, 1000), "007");
    assert_eq!(padded(7, 10), "7");
    assert!(padded(9, 1000) < padded(10, 1000), "9 sorts before 10");
}
