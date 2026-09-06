//! `index-image` — the image tier of the embedded retrieval engine.
//!
//! # The finding this crate exists to exploit
//!
//! `docs/research/image.md` surveyed every alive FOSS photo system. The gap is not that semantic
//! image search is hard — `rclip` indexes 1.28 M images in 3 hours on a laptop. The gap is that
//! **nobody answers metadata, perceptual and semantic predicates in one query plan over one
//! index.** Immich splits pgvector from Postgres rows from a separate face-clustering job.
//! LibrePhotos splits FAISS from Django. PhotoPrism combines them in its *query syntax* and then
//! fans out underneath. Every one of them needs a server to answer a question about your own
//! files, and every one of them is AGPL-3.0.
//!
//! So this crate does not build a better vector index. It builds the **column types that make an
//! image a first-class `index-text` document**, so that
//!
//! > `"beach"` (caption, BM25F) AND `camera = "iPhone 15"` (facet) AND `2019 <= year < 2021`
//! > (numeric range) AND *looks like this photo* (vector) AND *is not a re-upload of that one*
//! > (perceptual hash)
//!
//! is **one call, one k-selection, one file**.
//!
//! # What this crate deliberately does not contain
//!
//! No model, no ONNX runtime, no image decoder. An embedding is an **input** — the host produces
//! it and passes `&[f32]`. That is what keeps this dependency-free, keeps the WASM artifact
//! shippable, and keeps the licence at MIT OR Apache-2.0 while the incumbents are AGPL.
//!
//! The honest cost of that choice is stated rather than hidden: **this crate cannot embed an image
//! for you.** `crates/index-bench/src/image_corpus.rs` carries the reference ingest.

#![forbid(unsafe_code)]

pub mod color;
pub mod digest;
pub mod hash;
pub mod image;
pub mod meta;
pub mod vector;

pub use color::{Palette, Swatch, BUCKET_COUNT};
pub use digest::sha256;
pub use hash::{Hash256, Hash64};
pub use image::{FusedHit, FusedQuery, ImageIndex, ImageIndexBuilder};
pub use meta::{Exif, Geo};
pub use vector::{Metric, VectorColumn};

/// How a candidate was found, so a fused result can say *why* it is here.
///
/// A unified query plan is only worth having if the answer is explicable. When a photo ranks
/// because its caption matched **and** its pixels matched, that is a materially stronger result
/// than either alone, and the caller is entitled to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Why {
    /// The text/facet/range half of `index-text` accepted this document.
    pub text: bool,
    /// A vector neighbour within the requested radius.
    pub vector: bool,
    /// A perceptual-hash near-duplicate.
    pub hash: bool,
    /// A colour-bucket match.
    pub color: bool,
}

impl Why {
    /// How many independent signals agreed. Used to break ties in a fused ranking: a document two
    /// signals found outranks one that a single signal found more strongly.
    pub fn agreement(&self) -> u32 {
        self.text as u32 + self.vector as u32 + self.hash as u32 + self.color as u32
    }
}

/// One image's extracted signal, as it enters the index.
///
/// Every field is optional because real corpora are ragged: a scraped web image has no EXIF, a
/// screenshot has no GPS, and an image nobody has embedded yet has no vector. The index must rank
/// what it has rather than refuse the document — `docs/research/image.md` §5 measured that social
/// platforms strip EXIF from essentially every downloadable image, so "metadata absent" is the
/// common case in an OSINT corpus, not the exception.
#[derive(Debug, Clone, Default)]
pub struct ImageDoc {
    /// Content address of the ORIGINAL bytes. Dedup key, and the proof that a 1:1 restore is 1:1.
    pub digest: [u8; 32],
    /// Original file size in bytes, before any transcode.
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    /// 64-bit difference hash — near-free, catches re-encodes and resizes.
    pub dhash: Option<Hash64>,
    /// 64-bit DCT hash — survives gamma and colour shifts that defeat `dhash`.
    pub phash: Option<Hash64>,
    /// 256-bit PDQ-shaped hash, for corpora that need the wider code's discrimination.
    pub pdq: Option<Hash256>,
    /// Dominant colours in OKLab.
    pub palette: Option<Palette>,
    pub exif: Option<Exif>,
}
