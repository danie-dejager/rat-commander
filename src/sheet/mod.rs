//! Tables kept as text: CSV and TSV files read as rows and columns, and the
//! spreadsheet grid the viewer draws them in.
//!
//! [`csv`] knows the format — which delimiter a file uses, where its records
//! start (a quoted field may hold line breaks, so that is not simply where its
//! lines start), and how a record splits into fields. [`grid`] is the
//! presentation: the cursor cell, scrolling by whole rows and columns, and the
//! drawing. Neither owns the bytes, so the same grid serves a file paged from
//! disk as well as one held in memory.

pub mod csv;
pub mod grid;

/// Whether `name` is a table by its extension: `.csv`, or `.tsv` / `.tab` for
/// the tab-separated kind.
pub fn is_sheet_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".csv", ".tsv", ".tab"].iter().any(|ext| lower.ends_with(ext) && lower.len() > ext.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_known_by_their_extension_in_any_case() {
        for name in ["a.csv", "B.CSV", "data.tsv", "x.tab", "dir.name/file.Csv"] {
            assert!(is_sheet_name(name), "{name}");
        }
        for name in ["csv", ".csv", "a.csvx", "a.txt", "tsv.md"] {
            assert!(!is_sheet_name(name), "{name}");
        }
    }
}
