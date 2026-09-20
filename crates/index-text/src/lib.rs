//! `index-text` — the embedded retrieval engine.
//!
//! This is the spine `ROADMAP.md` Part II describes, and it exists because the survey in
//! `docs/research/demand.md` found that eleven real applications need *text → ranked documents*,
//! not the `u64 → position` map the repo's first phase built.
//!
//! ```
//! use index_text::{AliasTable, Doc, Field, IndexBuilder, Schema};
//!
//! let schema = Schema::new(vec![
//!     Field::new("brand", 3.0, 0.4),
//!     Field::new("title", 2.0, 0.4),
//! ]);
//! let mut b = IndexBuilder::new(schema).with_alias(AliasTable::philippine_grocery());
//! b.add(&Doc::new(["Bear Brand", "Bear Brand Powdered Milk 300g"]));
//! b.add(&Doc::new(["Bear Brand", "Bear Brand Powdered Milk 900g"]));
//! let ix = b.build().unwrap();
//!
//! // A typo still finds the document...
//! assert_eq!(ix.search("ber brand milk 300g", 1)[0].doc, 0);
//! // ...but a different size never does.
//! assert_eq!(ix.search("bear brand milk 900g", 1)[0].doc, 1);
//! ```
//!
//! # What is bought and what is built
//!
//! Bought (audited in `docs/research/build-or-buy.md`): `tantivy-fst` and `levenshtein_automata`,
//! the exact stack Meilisearch and Tantivy ship, at 4.2 M and 4.4 M recent downloads.
//!
//! Built, because no alive crate provides it:
//! - the **analyzer** — four repos on this machine hand-rolled a worse one ([`analyze`]);
//! - the **typo policy** — where every production engine's real behaviour lives ([`dict`]);
//! - the **scorer** — Tantivy's `k1`/`b` are compile-time constants and its one-byte fieldnorm
//!   destroys the length signal on short product titles ([`index`]).

pub mod analyze;
pub mod dict;
pub mod format;
pub mod fuse;
pub mod index;
pub mod query;
pub(crate) mod reorder;
pub mod searcher;

pub use analyze::{fold, parse_quantity, tokenize, AliasTable, BaseUnit, Quantity, Token};
pub use dict::{max_edit_for, TermDict, TermMatch};
pub use format::{
    crc32, encode_posting_offset, posting_span, read_section_table, SectionTable, Span, MAGIC,
};
pub use fuse::{convex, rrf, RRF_K};
pub use index::{
    Doc, FacetClause, Field, Hit, Index, IndexBuilder, Schema, MAX_COLUMN, MAX_FIELD,
};
pub use searcher::{Searcher, DEFAULT_COMPACTION_RATIO};
