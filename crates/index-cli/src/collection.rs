//! A collection on disk: a directory of numbered segments.
//!
//! # Why a directory rather than a file
//!
//! Because deletes have to persist. A tombstone lives inside a segment's own bytes, so applying a
//! change stream rewrites the segments it touched and appends a new one. That is two files changing
//! at once, which a single-file index cannot express without rewriting the whole thing on every
//! change — the cost `Searcher` exists to avoid.
//!
//! ```text
//! data/
//!   0000.idx   base segment
//!   0001.idx   appended by `index apply`
//!   0002.idx   ...
//! ```
//!
//! Segments load in **name order**, which is also insertion order, which is what makes
//! newest-wins key resolution correct. Zero-padded names keep lexical order equal to numeric order
//! past nine segments — the bug that eventually bites every `1.idx`, `10.idx`, `2.idx` scheme.
//!
//! **The single-artifact case is a collection with one segment**, so shipping to a browser is still
//! "copy `0000.idx`". `index compact` puts a collection back into that shape.
//!
//! # Durability
//!
//! Every write goes to a temporary file in the same directory and is renamed into place, because a
//! process killed mid-write must not leave a half-written segment that `Searcher` will then refuse
//! to open. Rename within a directory is atomic on every platform this targets.

use index_text::{Index, Searcher};
use std::path::{Path, PathBuf};

/// Load every segment of a collection, in order.
///
/// Returns the searcher and the segment paths, parallel to the searcher's segment order, so a
/// caller that mutates tombstones knows which file each segment came from.
pub fn open(dir: &Path) -> Result<(Searcher, Vec<PathBuf>), String> {
    let path = segment_path(dir)?;
    if path.is_empty() {
        return Err(format!("{} holds no .idx segments", dir.display()));
    }
    let mut searcher: Option<Searcher> = None;
    for p in &path {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let ix = Index::from_bytes(&bytes).map_err(|e| format!("{}: {e}", p.display()))?;
        match searcher.as_mut() {
            // `push` is what applies newest-wins key shadowing, so loading a collection
            // reconstructs exactly the state `apply` left behind.
            Some(s) => {
                s.push(ix);
            }
            None => searcher = Some(Searcher::new(ix)),
        }
    }
    Ok((searcher.expect("non-empty"), path))
}

/// Every `*.idx` in `dir`, sorted by name.
pub fn segment_path(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut path: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "idx"))
        .collect();
    path.sort();
    Ok(path)
}

/// The path the next appended segment should take.
pub fn next_segment(dir: &Path) -> Result<PathBuf, String> {
    let n = segment_path(dir)?.len();
    Ok(dir.join(format!("{n:04}.idx")))
}

/// Write bytes to `path` atomically: temp file in the same directory, then rename.
///
/// A partially written segment is worse than a missing one — it is a file that looks like an index
/// and is refused at load, taking the whole collection with it.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("idx.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use index_text::{Doc, Field, IndexBuilder, Schema};

    fn seg(row: &[(&str, &str)]) -> Vec<u8> {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("sku", 0.0, 0.6),
            Field::new("name", 3.0, 0.4),
        ]))
        .with_key(0);
        for (sku, name) in row {
            b.add(&Doc::new([*sku, *name]));
        }
        b.build().unwrap().to_bytes()
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("index-cli-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Ten segments is where a naive `1.idx, 2.idx, 10.idx` scheme starts loading out of order,
    /// which silently inverts newest-wins and resurrects superseded rows.
    #[test]
    fn segments_load_in_insertion_order_past_nine() {
        let dir = tmpdir("order");
        for i in 0..12 {
            let p = next_segment(&dir).unwrap();
            write_atomic(&p, &seg(&[("sku-1", &format!("version {i}"))])).unwrap();
        }
        let (s, path) = open(&dir).unwrap();
        assert_eq!(path.len(), 12);
        assert_eq!(
            path.last().unwrap().file_name().unwrap().to_str().unwrap(),
            "0011.idx",
            "zero padding keeps lexical order equal to numeric order"
        );
        // Eleven of the twelve are shadowed, and the survivor is the LAST one written.
        assert_eq!(s.live_count(), 1);
        assert_eq!(s.doc_of_key("sku-1"), Some(11));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_atomic_write_leaves_no_partial_file_behind() {
        let dir = tmpdir("atomic");
        let p = next_segment(&dir).unwrap();
        write_atomic(&p, &seg(&[("sku-1", "Colgate Total")])).unwrap();
        assert!(p.exists());
        assert!(!p.with_extension("idx.tmp").exists(), "the temp file is renamed, not left");
        assert_eq!(segment_path(&dir).unwrap().len(), 1, "the .tmp is not mistaken for a segment");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_directory_is_an_error_rather_than_an_empty_index() {
        let dir = tmpdir("empty");
        assert!(open(&dir).is_err(), "an empty collection must not read as a working one");
        std::fs::remove_dir_all(&dir).ok();
    }
}
