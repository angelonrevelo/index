//! p60-image-corpus :: the image tier, measured on 17,311 real scraped web files.
//!
//! `bench/roadmap/p60-image-corpus.md` is the spec. The rule this repo runs on is that a benchmark
//! runs on real data or it does not count, and for the image tier that bites harder than usual
//! because every failure mode is a property of a *messy* corpus: stripped metadata,
//! near-duplicates, mixed formats, and files that are not photographs at all.
//!
//! The corpus is `alec/expressway_dump` — a genuine scrape of a public site's asset tree. It was
//! not curated for this benchmark, which is exactly its value, and it is the WRONG shape for the
//! personal-photo question (no faces, no personal photography, CDN-stripped metadata). The census
//! below says so rather than generalising from it.
//!
//! Output order is fixed by the spec: census FIRST, because every number after it is meaningless
//! without it.
//!
//! ## This binary is the host
//!
//! `index-image` ships no pixel decoder — that is its thesis, and `crates/index-image/Cargo.toml`
//! argues it. A decoder is a host concern. This benchmark is a host, so it owns the `image` crate
//! dependency and hands `index-image` the bytes it asked for.
//!
//! ## The synthetic rule
//!
//! This crate ships no embedding model either. `--embedding synthetic` produces a seeded
//! deterministic stand-in so the *plumbing* is gated today; every number derived from it is printed
//! with a `SYNTHETIC` marker and the `p58` recall verdict is **withheld**, because a recall figure
//! over made-up vectors measures nothing.
//!
//! ## `--emit-manifest <path>` — the contract an external encoder binds to
//!
//! The embedding file is **positional**: entry `i` is the vector for the `i`th document this run
//! indexes. Nothing about that order is guessable from outside — it is the sorted walk, minus every
//! file that did not decode, and a `--limit` run strides the corpus on top of that. An external
//! encoder therefore cannot produce a correctly ordered file without being told the order, and a
//! file that is merely *plausible* mis-attributes every vector in silence.
//!
//! `--emit-manifest` writes that order down: one line per indexed document, in exactly the order
//! this run indexes them, as `<absolute path>\t<sha256 hex>`. Feed the paths to the encoder in file
//! order and write the vectors back in the same order.
//!
//! The **digest column exists so that a stale manifest can be detected rather than silently
//! mis-aligning every vector**. If the corpus changes under a manifest — a file added, removed or
//! re-encoded — the digest on that line stops matching, which is a checkable fact; without it the
//! only symptom would be a plausible-looking retrieval number computed over vectors attached to the
//! wrong images. For the same reason `--embedding` refuses to run when the vector count and the
//! indexed-document count disagree: a truncated or over-long embedding file must never produce a
//! recall figure.

use index_image::color::Palette;
use index_image::hash::{self, Hash256, Hash64};
use index_image::meta::{self, Exif, Format};
use index_image::vector::Metric;
use index_image::{sha256, FusedQuery, ImageDoc, ImageIndex, ImageIndexBuilder};
use index_text::{Doc, FacetClause, Field, Schema};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod timer;

// ---------------------------------------------------------------------------------------------
// Constants — every one of them carries the number that justifies it.
// ---------------------------------------------------------------------------------------------

/// `p57` acceptance 1: measured p50 for the whole cheap tier, per image.
const CHEAP_P50_MS_MAX: f64 = 10.0;
/// `p57` acceptance 1: total stored bytes per image.
const CHEAP_BYTE_MAX: f64 = 200.0;

/// Embedding width when synthesising. 512-d is the width `p58` prices its byte table at.
const DIM: usize = 512;

/// Page size for the fused correctness queries. `p59` acceptance 1 fixes k = 20.
const K: usize = 20;
/// `p59` acceptance 1: "the 100 hardest fused queries".
const QUERY_COUNT: usize = 100;

/// `p59` acceptance 3 fixes the cutoff at 10: "fused nDCG@10 must exceed both".
const NDCG_K: usize = 10;
/// Fewest queries the NON-DEGENERATE labelled set must yield before acceptance 3 is adjudicated on
/// it at all. Below this the mean of three nDCG figures is noise wearing a verdict's clothes, and an
/// underpowered PASS is worth nothing — so the verdict is withheld exactly as `p58`'s recall is
/// withheld under synthetic embeddings, rather than rendered from too little evidence.
const NDCG_QUERY_MIN: usize = 20;

/// Refuse to decode beyond this many pixels. A scraped corpus can contain a header claiming a
/// 60000x60000 image; allocating for it is a denial of service, not a measurement.
const MAX_PIXEL: u64 = 64_000_000;

/// dHash is 64 bits. `hash::HASH64_NEAR_MAX` (4) is the measured default and is in this list, but
/// it is printed as one row among several ON PURPOSE: `docs/research/image.md` §5 records a 12x
/// spread in published near-duplicate rates across methods, so a single headline number would be a
/// methodology choice wearing a fact's clothes.
const HASH64_THRESHOLD: [u32; 7] = [0, 2, 4, 6, 8, 10, 12];
/// PDQ-shaped 256-bit codes. Meta's recommended threshold is 31/256; 128/256 is the random-pair
/// expectation. Thresholds do NOT port across code lengths, hence a separate list.
const HASH256_THRESHOLD: [u32; 4] = [0, 16, 31, 48];

/// Cap on per-finding examples printed. Bounded output is a methodology requirement.
const EXAMPLE_MAX: usize = 5;

/// The numeric-range predicate used by the fused queries: byte length, in bytes. 2 KiB is the
/// floor because a scrape is full of tracking pixels and spacer GIFs below it, so this is the
/// filter a real caller would actually apply rather than a range chosen to always pass.
const BYTE_RANGE_LO: f64 = 2048.0;
const BYTE_RANGE_HI: f64 = 1.0e9;

// ---------------------------------------------------------------------------------------------
// Corpus location
// ---------------------------------------------------------------------------------------------

fn corpus_dir() -> PathBuf {
    std::env::var("INDEX_IMAGE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Users\maran\Code\alec\expressway_dump"))
}

/// Depth-first, **sorted at every level**. The walk order is part of the verdict: document ids,
/// synthetic embeddings and tie-breaks all derive from it.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entry: Vec<PathBuf> = read.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entry.sort();
    for path in entry {
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Panic containment
// ---------------------------------------------------------------------------------------------

/// `p57` acceptance 5: the corpus is scraped, so the parsers eat arbitrary input and a crash is a
/// FAILURE OF THIS REPO, not a corpus problem. The decoder is third-party and gets the same
/// treatment. The hook stores the message instead of printing it, so a caught panic becomes a
/// bounded finding rather than a wall of backtrace.
static PANIC_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut slot) = PANIC_MESSAGE.lock() {
            *slot = Some(info.to_string());
        }
    }));
}

fn take_panic_message() -> String {
    PANIC_MESSAGE
        .lock()
        .ok()
        .and_then(|mut s| s.take())
        .unwrap_or_else(|| "<no message>".to_string())
}

/// Run `f`, converting a panic into `Err(message)`.
fn guard<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|_| take_panic_message())
}

// ---------------------------------------------------------------------------------------------
// Per-file record
// ---------------------------------------------------------------------------------------------

struct Record {
    /// Corpus-relative path, forward-slashed. Doubles as the text field: on a scrape, the URL path
    /// IS the caption, and there is no other text.
    path: String,
    /// Lowercase file extension, kept only to measure how often it DISAGREES with `meta::sniff`.
    ext: String,
    byte_len: u64,
    digest: [u8; 32],
    format: Format,
    /// Dimensions from the cheap structural probe (no pixel decode).
    probe_dim: Option<(u32, u32)>,
    /// Dimensions from the real decode, when it happened.
    decoded_dim: Option<(u32, u32)>,
    dhash: Option<Hash64>,
    phash: Option<Hash64>,
    pdq: Option<Hash256>,
    palette: Option<Palette>,
    exif: Option<Exif>,
    /// Did the file carry an EXIF block at all, parsed or not?
    exif_block: bool,
    /// Cheap tier (luma + dHash + pHash + palette + EXIF), nanoseconds. Decode excluded — see the
    /// note where this is reported.
    cheap_ns: f64,
    /// The 256-bit PDQ-shaped hash, timed separately: it is not in `p57`'s <10 ms budget line.
    pdq_ns: f64,
    /// Third-party decode, nanoseconds. The host's cost, reported beside the engine's.
    decode_ns: f64,
    /// Bytes the cheap tier stores per image, as defined by `signal_byte`.
    signal_byte: usize,
}

/// Bytes the cheap tier costs to STORE, which is what `p57`'s <200 B budget is about.
///
/// dHash 8 + pHash 8 + palette (OKLab triple + weight, 4 x f32 per swatch) + whatever EXIF
/// survived. The 256-bit PDQ code and the 32-byte content digest are counted separately because
/// neither is in `p57`'s table.
fn signal_byte(rec: &Record) -> usize {
    let hash = rec.dhash.map_or(0, |_| 8) + rec.phash.map_or(0, |_| 8);
    let palette = rec.palette.as_ref().map_or(0, |p| p.0.len() * 16);
    hash + palette + rec.exif.as_ref().map_or(0, exif_byte)
}

/// A stored-size estimate for one parsed EXIF: text as its bytes, each scalar as 8.
fn exif_byte(exif: &Exif) -> usize {
    let text = [&exif.datetime, &exif.make, &exif.model, &exif.lens]
        .iter()
        .filter_map(|f| f.as_ref())
        .map(|s| s.len())
        .sum::<usize>();
    let scalar = exif.geo.is_some() as usize * 16
        + exif.altitude_m.is_some() as usize * 8
        + exif.orientation.is_some() as usize * 2
        + exif.iso.is_some() as usize * 4
        + exif.f_number.is_some() as usize * 8
        + exif.exposure_second.is_some() as usize * 8
        + exif.focal_length_mm.is_some() as usize * 8
        + exif.thumb.is_some() as usize * 8;
    text + scalar
}

/// Landscape / portrait / square, from the decoded dimensions. The 5 % dead band stops a
/// 1001x1000 image being called landscape, which is a distinction no user makes.
fn shape_of(w: u32, h: u32) -> &'static str {
    let (w, h) = (w as f64, h as f64);
    if w > h * 1.05 {
        "landscape"
    } else if h > w * 1.05 {
        "portrait"
    } else {
        "square"
    }
}

/// The path as searchable text. A scraped asset path is `a__b__c-hero.webp`; the separators carry
/// the words, so the host splits on them. This is host preprocessing, stated rather than hidden.
fn path_text(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' }).collect()
}

// ---------------------------------------------------------------------------------------------
// Embeddings
// ---------------------------------------------------------------------------------------------

enum Embedding {
    /// Seeded, deterministic, and **meaningless**. Present so the plumbing is gated without a
    /// model; every number it touches is marked `SYNTHETIC`.
    Synthetic,
    /// Real host-produced vectors: `u32 count`, `u32 dim`, then `count * dim` little-endian f32,
    /// in this benchmark's walk order over the indexed subset.
    Real { dim: usize, data: Vec<f32> },
}

impl Embedding {
    fn dim(&self) -> usize {
        match self {
            Embedding::Synthetic => DIM,
            Embedding::Real { dim, .. } => *dim,
        }
    }

    fn is_synthetic(&self) -> bool {
        matches!(self, Embedding::Synthetic)
    }

    /// How many vectors the file actually holds. `None` for the synthetic source, which is a
    /// function of the digest and so has exactly as many vectors as it is asked for.
    fn count(&self) -> Option<usize> {
        match self {
            Embedding::Synthetic => None,
            Embedding::Real { dim, data } => Some(data.len() / *dim),
        }
    }

    /// The vector for indexed document `i`.
    fn vector(&self, i: usize, digest: &[u8; 32]) -> Option<Vec<f32>> {
        match self {
            Embedding::Synthetic => {
                // Seeded from the CONTENT digest, so the same file yields the same vector no matter
                // where it lands in the walk — the property a real encoder has.
                let mut state = u64::from_le_bytes(digest[0..8].try_into().ok()?) | 1;
                let mut out = Vec::with_capacity(DIM);
                for _ in 0..DIM {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    out.push(((state >> 33) as f64 / (1u64 << 31) as f64 - 1.0) as f32);
                }
                Some(out)
            }
            Embedding::Real { dim, data } => {
                let lo = i.checked_mul(*dim)?;
                data.get(lo..lo + dim).map(|s| s.to_vec())
            }
        }
    }
}

fn load_embedding(path: &Path) -> Result<Embedding, String> {
    let byte = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if byte.len() < 8 {
        return Err(format!("{}: shorter than the 8-byte header", path.display()));
    }
    let count = u32::from_le_bytes(byte[0..4].try_into().unwrap()) as usize;
    let dim = u32::from_le_bytes(byte[4..8].try_into().unwrap()) as usize;
    let need = count.saturating_mul(dim).saturating_mul(4);
    if dim == 0 || byte.len() < 8 + need {
        return Err(format!(
            "{}: header says {count} x {dim} ({need} B) but the file holds {} B",
            path.display(),
            byte.len() - 8
        ));
    }
    let data = byte[8..8 + need]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    Ok(Embedding::Real { dim, data })
}

// ---------------------------------------------------------------------------------------------
// Union-find, for near-duplicate clustering
// ---------------------------------------------------------------------------------------------

struct UnionFind {
    parent: Vec<u32>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind { parent: (0..n as u32).collect() }
    }
    fn find(&mut self, mut x: u32) -> u32 {
        while self.parent[x as usize] != x {
            let grand = self.parent[self.parent[x as usize] as usize];
            self.parent[x as usize] = grand;
            x = grand;
        }
        x
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            // Lower root always wins, so the clustering does not depend on visit order.
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            self.parent[hi as usize] = lo;
        }
    }
    /// `(clusters with >= 2 members, members inside such clusters)`.
    fn summary(&mut self) -> (usize, usize) {
        let n = self.parent.len();
        let mut size: HashMap<u32, usize> = HashMap::new();
        for i in 0..n as u32 {
            *size.entry(self.find(i)).or_default() += 1;
        }
        let cluster = size.values().filter(|&&s| s >= 2).count();
        let member: usize = size.values().filter(|&&s| s >= 2).sum();
        (cluster, member)
    }
}

// ---------------------------------------------------------------------------------------------
// Reporting helpers
// ---------------------------------------------------------------------------------------------

/// Lowercase hex of a content digest — the manifest's second column.
fn hex(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The corpus root as an absolute path, with Windows' `\\?\` verbatim prefix stripped: the manifest
/// is read by an external encoder, not by this process, and that prefix trips several toolchains.
/// Falls back to the path as given rather than failing — a manifest with a relative root is worse
/// than useless but it is not this function's job to decide that.
fn absolute_dir(dir: &Path) -> PathBuf {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let text = canonical.to_string_lossy().to_string();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text.as_str()))
}

/// Binary-gain nDCG at `k`: gain 1 for a relevant document, discount `1 / log2(rank + 1)` with
/// ranks 1-based, normalised by the ideal DCG for this query's relevant count **capped at k** (a
/// query with 40 relevant documents cannot be punished for a 10-slot page holding only 10).
fn ndcg(rank: &[u32], relevant: &HashSet<u32>, k: usize) -> f64 {
    let discount = |i: usize| 1.0 / ((i + 2) as f64).log2();
    let dcg: f64 = rank
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, doc)| relevant.contains(doc))
        .map(|(i, _)| discount(i))
        .sum();
    let ideal: f64 = (0..relevant.len().min(k)).map(discount).sum();
    if ideal == 0.0 {
        0.0
    } else {
        dcg / ideal
    }
}

fn ms(sorted: &[f64], p: f64) -> f64 {
    timer::percentile(sorted, p) / 1e6
}

fn us(sorted: &[f64], p: f64) -> f64 {
    timer::percentile(sorted, p) / 1e3
}

/// Format a bucket edge without collapsing distinct sub-unit edges onto the same label. Rounding
/// 0.05 and 0.25 both to "0" made two megapixel rows read identically, which is a lie about the
/// distribution even though the counts beside them were right.
fn edge_label(v: f64, unit: &str) -> String {
    if !v.is_finite() {
        "inf".to_string()
    } else if v == 0.0 || v >= 10.0 {
        format!("{v:.0}{unit}")
    } else if v >= 0.1 {
        format!("{v:.2}{unit}")
    } else {
        format!("{v:.3}{unit}")
    }
}

/// A histogram over a sorted numeric sample, printed as edge/count/share rows.
fn histogram(label: &str, sorted: &[f64], edge: &[f64], unit: &str) {
    println!("  {label}");
    if sorted.is_empty() {
        println!("    (empty)");
        return;
    }
    let n = sorted.len() as f64;
    for i in 0..edge.len() - 1 {
        let (lo, hi) = (edge[i], edge[i + 1]);
        let count = sorted.iter().filter(|&&v| v >= lo && v < hi).count();
        println!(
            "    {:>12} .. {:<12} {:>7}  {:>5.1}%",
            edge_label(lo, unit),
            edge_label(hi, unit),
            count,
            100.0 * count as f64 / n
        );
    }
    println!(
        "    p50 {}   p90 {}   p99 {}   max {}",
        edge_label(timer::percentile(sorted, 0.5), unit),
        edge_label(timer::percentile(sorted, 0.9), unit),
        edge_label(timer::percentile(sorted, 0.99), unit),
        edge_label(sorted[sorted.len() - 1], unit)
    );
}

// ---------------------------------------------------------------------------------------------

struct Arg {
    limit: Option<usize>,
    embedding: Option<PathBuf>,
    /// Where to write the positional contract an external encoder binds to. See the module comment.
    manifest: Option<PathBuf>,
}

fn parse_arg() -> Result<Arg, String> {
    let mut arg = Arg { limit: None, embedding: None, manifest: None };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--limit" => {
                let v = it.next().ok_or("--limit needs a count")?;
                arg.limit = Some(v.parse::<usize>().map_err(|e| format!("--limit: {e}"))?);
            }
            "--embedding" => {
                let v = it.next().ok_or("--embedding needs `synthetic` or a path")?;
                if v != "synthetic" {
                    arg.embedding = Some(PathBuf::from(v));
                }
            }
            "--emit-manifest" => {
                let v = it.next().ok_or("--emit-manifest needs a path")?;
                arg.manifest = Some(PathBuf::from(v));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(arg)
}

fn main() {
    let arg = match parse_arg() {
        Ok(a) => a,
        Err(e) => {
            println!("p60-image-corpus: {e}");
            println!(
                "usage: image-corpus [--limit N] [--embedding synthetic|<path>] [--emit-manifest <path>]"
            );
            std::process::exit(2);
        }
    };

    let clock = timer::Clock::new();
    println!("p60-image-corpus :: the image tier on a real scrape of 17,311 web files");
    println!("clock backend: {}", clock.backend());

    // ---- corpus presence -----------------------------------------------------------------------
    let dir = corpus_dir();
    if !dir.is_dir() {
        println!("\nSKIP: corpus directory not found at {}", dir.display());
        println!("      This corpus is machine-local (a scrape of a public asset tree) and is not");
        println!("      committed. Set INDEX_IMAGE_CORPUS to a directory of image files to run it.");
        println!("\nOVERALL: SKIPPED — no corpus, so no verdict. This is not a pass.");
        // Exit 0 deliberately: a missing machine-local corpus must not turn the gate red.
        std::process::exit(0);
    }

    let mut file = Vec::new();
    walk(&dir, &mut file);
    let total_file = file.len();
    if total_file == 0 {
        println!("\nSKIP: {} contains no files.", dir.display());
        println!("\nOVERALL: SKIPPED — no corpus, so no verdict. This is not a pass.");
        std::process::exit(0);
    }

    // Deterministic stride sample, so a `--limit` run is a cross-section of the corpus rather than
    // whatever sorts first (which on this corpus would be one directory of hero images).
    let stride = match arg.limit {
        Some(n) if n < total_file => (total_file / n).max(1),
        _ => 1,
    };
    let sample: Vec<PathBuf> = if stride > 1 {
        file.iter().step_by(stride).take(arg.limit.unwrap_or(total_file)).cloned().collect()
    } else {
        file.clone()
    };

    println!("\ncorpus: {}", dir.display());
    println!("  {total_file} files on disk");
    if sample.len() != total_file {
        println!(
            "  SAMPLED: {} files, every {stride}th in sorted walk order ({:.1}% of the corpus)",
            sample.len(),
            100.0 * sample.len() as f64 / total_file as f64
        );
    }

    install_panic_hook();

    // ---- ingest ---------------------------------------------------------------------------------
    let mut record: Vec<Record> = Vec::with_capacity(sample.len());
    let mut crash: Vec<(String, String)> = Vec::new();
    let mut unreadable = 0usize;
    let mut oversize = 0usize;
    let mut decode_fail = 0usize;
    let mut corpus_byte: u64 = 0;

    for (i, path) in sample.iter().enumerate() {
        if i % 2000 == 0 && i > 0 {
            eprintln!("  ... {i}/{} ingested", sample.len());
        }
        let rel = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        let Ok(byte) = std::fs::read(path) else {
            unreadable += 1;
            continue;
        };
        corpus_byte += byte.len() as u64;

        // Everything below runs on attacker-shaped bytes. A panic anywhere here is a `p57` check-5
        // failure; it is recorded with the filename and the walk continues.
        let built = guard(|| {
            let digest = sha256(&byte);
            let probe = meta::probe(&byte);
            let rec = Record {
                path: rel.clone(),
                ext: ext.clone(),
                byte_len: byte.len() as u64,
                digest,
                format: probe.format,
                probe_dim: probe.width.zip(probe.height),
                decoded_dim: None,
                dhash: None,
                phash: None,
                pdq: None,
                palette: None,
                exif: None,
                exif_block: meta::exif_block(&byte).is_some(),
                cheap_ns: 0.0,
                pdq_ns: 0.0,
                decode_ns: 0.0,
                signal_byte: 0,
            };
            (rec, byte)
        });
        let (mut rec, byte) = match built {
            Ok(v) => v,
            Err(msg) => {
                if crash.len() < EXAMPLE_MAX {
                    crash.push((rel.clone(), msg));
                } else {
                    crash.push((rel.clone(), String::new()));
                }
                continue;
            }
        };

        // Only formats with a decoder are decoded. `Format::Unknown` is the correct answer for the
        // HTML pages and the `.DS_Store` this scrape contains, and it costs nothing to skip them.
        let decodable = matches!(
            rec.format,
            Format::Jpeg | Format::Png | Format::Webp | Format::Gif
        );
        let too_big = rec
            .probe_dim
            .is_some_and(|(w, h)| w as u64 * h as u64 > MAX_PIXEL);
        if too_big {
            oversize += 1;
        }

        if decodable && !too_big {
            let mut decoded: Option<(u32, u32, Vec<u8>)> = None;
            let decode_ns = clock.measure_ns(|| {
                // The decoder is third-party and gets the panic guard too: `p57` check 5 is about
                // this pipeline surviving hostile input, and a crash here is still a crash.
                decoded = guard(|| {
                    let img = image::load_from_memory(&byte).ok()?;
                    let rgb = img.to_rgb8();
                    let (w, h) = (rgb.width(), rgb.height());
                    Some((w, h, rgb.into_raw()))
                })
                .unwrap_or(None);
                decoded.as_ref().map_or(0, |d| d.0 as u64)
            });
            rec.decode_ns = decode_ns;

            if let Some((w, h, rgb)) = decoded {
                rec.decoded_dim = Some((w, h));
                // ---- the cheap tier, from ONE decode ------------------------------------------
                // Decode cost is real at 17 k files, so every signal derives from this single
                // buffer. What is timed here is `p57`'s table: luma reduction, dHash, pHash,
                // palette, EXIF. The decode above is the HOST's cost and is reported separately —
                // `p57`'s <10 ms budget is a claim about the engine's tier, not about libjpeg.
                let mut dhash = None;
                let mut phash = None;
                let mut palette = None;
                let mut exif = None;
                let mut luma: Option<Vec<u8>> = None;
                let cheap_ns = clock.measure_ns(|| {
                    luma = hash::luma_from_rgb(&rgb, w, h);
                    if let Some(l) = luma.as_ref() {
                        dhash = Hash64::dhash(l, w, h);
                        phash = Hash64::phash(l, w, h);
                    }
                    palette = Palette::extract_default(&rgb, w, h);
                    exif = meta::parse(&byte);
                    dhash.map_or(0, |h| h.0)
                });
                let mut pdq = None;
                let pdq_ns = clock.measure_ns(|| {
                    if let Some(l) = luma.as_ref() {
                        pdq = Hash256::pdq(l, w, h);
                    }
                    pdq.map_or(0, |h| h.0[0])
                });
                rec.dhash = dhash;
                rec.phash = phash;
                rec.pdq = pdq;
                rec.palette = palette;
                rec.exif = exif;
                rec.cheap_ns = cheap_ns;
                rec.pdq_ns = pdq_ns;
            } else {
                decode_fail += 1;
                // A file that sniffed as an image but would not decode still gets the no-decode
                // half of the cheap tier, because that half never needed pixels.
                rec.exif = guard(|| meta::parse(&byte)).unwrap_or(None);
            }
        } else {
            rec.exif = guard(|| meta::parse(&byte)).unwrap_or(None);
        }

        rec.signal_byte = signal_byte(&rec);
        record.push(rec);
    }
    let _ = std::panic::take_hook();

    let mut fail = 0usize;
    let mut check: Vec<(String, bool)> = Vec::new();
    let mut withheld: Vec<String> = Vec::new();
    // Measured, reported, and deliberately given NO verdict — a figure whose labelled set cannot
    // adjudicate the question it looks like it answers. It is neither a PASS nor a FAIL and must
    // never be counted as either.
    let mut note: Vec<String> = Vec::new();

    // =============================================================================================
    // 1. CENSUS — first, because every number below is meaningless without it
    // =============================================================================================
    println!("\n=== 1. CORPUS CENSUS ===================================================");
    println!("  {} files read, {unreadable} unreadable, {} bytes total ({:.1} MB)",
        record.len(), corpus_byte, corpus_byte as f64 / 1e6);

    // Format mix from `meta::sniff`, NOT the extension.
    let mut by_format: HashMap<&'static str, usize> = HashMap::new();
    let mut by_ext: HashMap<String, usize> = HashMap::new();
    let mut divergent: Vec<(&str, &str, &str)> = Vec::new();
    let mut divergence = 0usize;
    for rec in &record {
        *by_format.entry(rec.format.name()).or_default() += 1;
        *by_ext.entry(rec.ext.clone()).or_default() += 1;
        let claimed = match rec.ext.as_str() {
            "jpg" | "jpeg" => Some("jpeg"),
            "png" => Some("png"),
            "webp" => Some("webp"),
            "gif" => Some("gif"),
            _ => None,
        };
        if let Some(c) = claimed {
            if c != rec.format.name() {
                divergence += 1;
                if divergent.len() < EXAMPLE_MAX {
                    divergent.push((rec.path.as_str(), c, rec.format.name()));
                }
            }
        }
    }
    let mut fmt: Vec<(&&str, &usize)> = by_format.iter().collect();
    fmt.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\n  --- format, by meta::sniff (magic bytes, not extension) ---");
    for (name, count) in &fmt {
        println!("    {:<10} {:>7}  {:>5.1}%", name, count, 100.0 * **count as f64 / record.len() as f64);
    }
    let mut ext: Vec<(&String, &usize)> = by_ext.iter().collect();
    ext.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    println!("\n  --- extension, for comparison (the extension is a CLAIM, not evidence) ---");
    for (name, count) in ext.iter().take(8) {
        println!("    {:<10} {:>7}", if name.is_empty() { "<none>" } else { name.as_str() }, count);
    }
    println!(
        "\n  extension DISAGREES with sniffed format on {divergence} of {} files ({:.2}%)",
        record.len(),
        100.0 * divergence as f64 / record.len() as f64
    );
    for (path, claimed, actual) in &divergent {
        println!("    e.g. .{claimed} that is actually {actual}: {path}");
    }
    if divergence > EXAMPLE_MAX {
        println!("    ... and {} more", divergence - EXAMPLE_MAX);
    }

    // Dimensions.
    let mut area: Vec<f64> = Vec::new();
    let mut width: Vec<f64> = Vec::new();
    for rec in &record {
        if let Some((w, h)) = rec.decoded_dim.or(rec.probe_dim) {
            area.push(w as f64 * h as f64 / 1e6);
            width.push(w as f64);
        }
    }
    area.sort_by(|a, b| a.partial_cmp(b).unwrap());
    width.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("\n  --- dimension ({} files with known dimensions) ---", area.len());
    histogram("megapixel", &area, &[0.0, 0.05, 0.25, 1.0, 4.0, 12.0, f64::INFINITY], " MP");
    histogram("width", &width, &[0.0, 200.0, 640.0, 1280.0, 1920.0, 4000.0, f64::INFINITY], " px");

    // Byte size.
    let mut size: Vec<f64> = record.iter().map(|r| r.byte_len as f64 / 1024.0).collect();
    size.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!();
    histogram("byte size", &size, &[0.0, 4.0, 32.0, 128.0, 512.0, 2048.0, f64::INFINITY], " KiB");

    // EXIF — the direct test of docs/research/image.md §7.
    let image_like = record.iter().filter(|r| r.format != Format::Unknown).count();
    let block = record.iter().filter(|r| r.exif_block).count();
    let parsed = record.iter().filter(|r| r.exif.as_ref().is_some_and(|e| !e.is_empty())).count();
    let with_geo = record.iter().filter(|r| r.exif.as_ref().is_some_and(|e| e.geo.is_some())).count();
    let with_model = record.iter().filter(|r| r.exif.as_ref().is_some_and(|e| e.model.is_some())).count();
    let exif_rate = if image_like == 0 { 0.0 } else { 100.0 * parsed as f64 / image_like as f64 };
    println!("\n  --- EXIF presence (the test of docs/research/image.md §7) ---");
    println!("    {image_like} files sniff as a real image container");
    println!("    {block} carry an EXIF/APP1 block ({:.2}%)", 100.0 * block as f64 / image_like.max(1) as f64);
    println!("    {parsed} parse to NON-EMPTY EXIF ({exif_rate:.2}%)");
    println!("    {with_geo} carry GPS, {with_model} carry a camera model");
    println!(
        "    -> §7 predicts scraped/CDN assets are stripped. Measured stripping rate {:.2}%: {}",
        100.0 - exif_rate,
        if exif_rate < 5.0 { "CONFIRMS §7" } else { "CONTRADICTS §7" }
    );

    println!("\n  --- what this corpus is NOT ---");
    println!("    Scraped CDN assets. No faces, no personal photography, and (per the rate above)");
    println!("    essentially no camera metadata. It is the right shape for the OSINT question and");
    println!("    the wrong shape for the personal-photo question. Corpus ceiling {} files —",
        total_file);
    println!("    below the 20-30 k the product question asks about and far below the 100 k+ where");
    println!("    incumbents' measured failures appear. Nothing above this count is claimed.");

    // ---- p57 check 5: no panic on any file ------------------------------------------------------
    println!("\n  --- ingest robustness (p57 check 5) ---");
    println!("    {unreadable} unreadable, {oversize} refused as over {MAX_PIXEL} px, {decode_fail} sniffed-as-image but failed to decode");
    println!("    {} panics", crash.len());
    for (path, msg) in crash.iter().take(EXAMPLE_MAX) {
        println!("      PANIC {path}: {}", msg.lines().next().unwrap_or(""));
    }
    check.push((format!("no panic on any of {} files", record.len() + crash.len()), crash.is_empty()));
    if !crash.is_empty() {
        fail += 1;
    }

    // =============================================================================================
    // 2. CHEAP-TIER COST (p57)
    // =============================================================================================
    println!("\n=== 2. CHEAP-TIER COST (p57) ===========================================");
    let mut cheap: Vec<f64> = record.iter().filter(|r| r.cheap_ns > 0.0).map(|r| r.cheap_ns).collect();
    let mut pdq_t: Vec<f64> = record.iter().filter(|r| r.pdq_ns > 0.0).map(|r| r.pdq_ns).collect();
    let mut dec: Vec<f64> = record.iter().filter(|r| r.decode_ns > 0.0).map(|r| r.decode_ns).collect();
    cheap.sort_by(|a, b| a.partial_cmp(b).unwrap());
    pdq_t.sort_by(|a, b| a.partial_cmp(b).unwrap());
    dec.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!("  {} images decoded and fully signalled", cheap.len());
    println!("  {:<38} {:>10} {:>10}", "", "p50", "p99");
    println!("  {:<38} {:>8.3}ms {:>8.3}ms", "cheap tier (luma+dHash+pHash+palette+EXIF)", ms(&cheap, 0.5), ms(&cheap, 0.99));
    println!("  {:<38} {:>8.3}ms {:>8.3}ms", "  + PDQ-shaped 256-bit (not in p57 budget)", ms(&pdq_t, 0.5), ms(&pdq_t, 0.99));
    println!("  {:<38} {:>8.3}ms {:>8.3}ms", "third-party decode (the HOST's cost)", ms(&dec, 0.5), ms(&dec, 0.99));

    let mut byte: Vec<f64> = record.iter().filter(|r| r.cheap_ns > 0.0).map(|r| r.signal_byte as f64).collect();
    byte.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let byte_mean = if byte.is_empty() { 0.0 } else { byte.iter().sum::<f64>() / byte.len() as f64 };
    println!("\n  stored bytes/image (dHash 8 + pHash 8 + palette 16/swatch + parsed EXIF):");
    println!("    mean {byte_mean:.1} B   p50 {:.0} B   p99 {:.0} B   max {:.0} B",
        timer::percentile(&byte, 0.5), timer::percentile(&byte, 0.99),
        byte.last().copied().unwrap_or(0.0));
    println!("    (+32 B content digest and +32 B PDQ code, counted separately: neither is in p57's table)");

    let cheap_p50 = ms(&cheap, 0.5);
    let cost_ok = cheap_p50 < CHEAP_P50_MS_MAX;
    let byte_ok = byte_mean < CHEAP_BYTE_MAX;
    println!("\n  -> p57 budget: p50 < {CHEAP_P50_MS_MAX} ms — measured {cheap_p50:.3} ms: {}",
        if cost_ok { "HOLDS" } else { "DOES NOT HOLD" });
    println!("  -> p57 budget: < {CHEAP_BYTE_MAX} B/image — measured {byte_mean:.1} B: {}",
        if byte_ok { "HOLDS" } else { "DOES NOT HOLD" });
    check.push((format!("p57 cheap tier p50 {cheap_p50:.3} ms < {CHEAP_P50_MS_MAX} ms"), cost_ok));
    check.push((format!("p57 cheap tier {byte_mean:.1} B/image < {CHEAP_BYTE_MAX} B"), byte_ok));
    if !cost_ok {
        fail += 1;
    }
    if !byte_ok {
        fail += 1;
    }

    // =============================================================================================
    // 3. DEDUP (p61)
    // =============================================================================================
    println!("\n=== 3. DEDUP ===========================================================");
    let mut by_digest: HashMap<[u8; 32], usize> = HashMap::new();
    for rec in &record {
        *by_digest.entry(rec.digest).or_default() += 1;
    }
    let dup_group = by_digest.values().filter(|&&c| c >= 2).count();
    let dup_file: usize = by_digest.values().filter(|&&c| c >= 2).sum();
    let redundant = dup_file - dup_group;
    // Reported twice, and it must be: the near-duplicate tables below cover only files that
    // produced a hash, so an all-files exact rate is not comparable to them. On this corpus the
    // difference is large, because 30 % of the files are HTML pages.
    let mut img_digest: HashMap<[u8; 32], usize> = HashMap::new();
    for rec in record.iter().filter(|r| r.format != Format::Unknown) {
        *img_digest.entry(rec.digest).or_default() += 1;
    }
    let img_group = img_digest.values().filter(|&&c| c >= 2).count();
    let img_dup: usize = img_digest.values().filter(|&&c| c >= 2).sum();
    let img_redundant = img_dup - img_group;
    println!("  exact duplicates by sha256 over the ORIGINAL bytes:");
    println!("    ALL FILES: {} distinct digests over {} files", by_digest.len(), record.len());
    println!("      {dup_group} digests occur more than once, covering {dup_file} files");
    println!("      {redundant} files are redundant copies ({:.2}%)",
        100.0 * redundant as f64 / record.len() as f64);
    println!("    IMAGES ONLY (the population the near-duplicate tables below cover):");
    println!("      {} distinct digests over {image_like} files", img_digest.len());
    println!("      {img_group} digests occur more than once, covering {img_dup} files");
    println!("      {img_redundant} files are redundant copies ({:.2}%)",
        100.0 * img_redundant as f64 / image_like.max(1) as f64);

    // Near-duplicates. Restricted to files that actually produced a hash.
    let dhash: Vec<Hash64> = record.iter().filter_map(|r| r.dhash).collect();
    let pdq: Vec<Hash256> = record.iter().filter_map(|r| r.pdq).collect();
    println!("\n  near-duplicates. EVERY count carries its threshold: docs/research/image.md §5");
    println!("  records a 12x spread across published methods, so one headline number would be a");
    println!("  methodology choice masquerading as a fact.");

    println!("\n  --- dHash, 64-bit (crate default HASH64_NEAR_MAX = {}) ---", hash::HASH64_NEAR_MAX);
    println!("    {} images hashed", dhash.len());
    println!("    {:<12} {:>10} {:>12} {:>10}", "threshold", "clusters", "images", "rate");
    let mut uf64: Vec<UnionFind> = HASH64_THRESHOLD.iter().map(|_| UnionFind::new(dhash.len())).collect();
    let tmax64 = HASH64_THRESHOLD.iter().copied().max().unwrap_or(0);
    for i in 0..dhash.len() {
        for j in (i + 1)..dhash.len() {
            let d = dhash[i].distance(&dhash[j]);
            if d > tmax64 {
                continue;
            }
            for (slot, &t) in HASH64_THRESHOLD.iter().enumerate() {
                if d <= t {
                    uf64[slot].union(i as u32, j as u32);
                }
            }
        }
    }
    for (slot, &t) in HASH64_THRESHOLD.iter().enumerate() {
        let (cluster, member) = uf64[slot].summary();
        println!("    <= {t:<9} {cluster:>10} {member:>12} {:>9.2}%",
            100.0 * member as f64 / dhash.len().max(1) as f64);
    }

    println!("\n  --- PDQ-shaped, 256-bit (Meta's recommended threshold = {}/256) ---", hash::HASH256_NEAR_MAX);
    println!("    {} images hashed", pdq.len());
    println!("    {:<12} {:>10} {:>12} {:>10}", "threshold", "clusters", "images", "rate");
    let mut uf256: Vec<UnionFind> = HASH256_THRESHOLD.iter().map(|_| UnionFind::new(pdq.len())).collect();
    let tmax256 = HASH256_THRESHOLD.iter().copied().max().unwrap_or(0);
    for i in 0..pdq.len() {
        for j in (i + 1)..pdq.len() {
            let d = pdq[i].distance(&pdq[j]);
            if d > tmax256 {
                continue;
            }
            for (slot, &t) in HASH256_THRESHOLD.iter().enumerate() {
                if d <= t {
                    uf256[slot].union(i as u32, j as u32);
                }
            }
        }
    }
    for (slot, &t) in HASH256_THRESHOLD.iter().enumerate() {
        let (cluster, member) = uf256[slot].summary();
        println!("    <= {t:<9} {cluster:>10} {member:>12} {:>9.2}%",
            100.0 * member as f64 / pdq.len().max(1) as f64);
    }
    println!("\n  -> §5 predicts 3-37% for scraped corpora depending on method. The spread above IS");
    println!("     that finding reproduced on this corpus: the method chooses the answer.");

    // =============================================================================================
    // Build the index (shared by 4, 5 and 6)
    // =============================================================================================
    let embedding = match arg.embedding.as_ref() {
        None => Embedding::Synthetic,
        Some(p) => match load_embedding(p) {
            Ok(e) => e,
            Err(msg) => {
                println!("\nFAIL: --embedding {msg}");
                println!("\nOVERALL: FAIL");
                std::process::exit(1);
            }
        },
    };
    if embedding.is_synthetic() {
        println!("\n########################################################################");
        println!("# SYNTHETIC EMBEDDING MODE                                             #");
        println!("# This crate ships no embedding model. Vectors below are a seeded       #");
        println!("# deterministic stand-in derived from each file's content digest. They  #");
        println!("# carry NO visual semantics. Every number they touch is marked          #");
        println!("# SYNTHETIC, and the p58 recall verdict is WITHHELD -- recall over       #");
        println!("# made-up vectors measures nothing at all.                              #");
        println!("# Run with --embedding <path> to get a real verdict.                    #");
        println!("########################################################################");
    }

    // The indexed set: files that decoded, so every facet and numeric column has a real value.
    let indexed: Vec<&Record> = record.iter().filter(|r| r.decoded_dim.is_some()).collect();
    if indexed.len() < K {
        println!("\nSKIP: only {} decodable images — too few to query.", indexed.len());
        println!("\nOVERALL: SKIPPED — corpus too small for a verdict.");
        std::process::exit(0);
    }

    // ---- the positional manifest (written BEFORE indexing, which is the point) -------------------
    //
    // `indexed` is now fixed, and it IS the order every vector file must follow. Emitting here — after
    // the decode pass that decides membership, before a single document enters the index — is what
    // makes the order a contract rather than an accident an encoder has to reverse-engineer.
    if let Some(out_path) = arg.manifest.as_ref() {
        let root = absolute_dir(&dir);
        let mut text = String::with_capacity(indexed.len() * 96);
        for r in &indexed {
            // `Record::path` is stored forward-slashed for the text field; put the platform's
            // separator back so the line names a path the host's encoder can actually open.
            let native = r.path.replace('/', std::path::MAIN_SEPARATOR_STR);
            text.push_str(&format!("{}\t{}\n", root.join(native).display(), hex(&r.digest)));
        }
        match std::fs::write(out_path, &text) {
            Ok(()) => {
                println!("\n  manifest: {} lines -> {}", indexed.len(), out_path.display());
                println!("    One line per indexed document, in indexing order, as");
                println!("    `<absolute path>\\t<sha256 hex>`. This is the order a vector file must");
                println!("    follow: entry i is the vector for line i+1. The digest column is there so");
                println!("    a stale manifest is DETECTABLE — without it, a corpus that changed under");
                println!("    the encoder mis-attributes every vector and still prints a number.");
            }
            Err(e) => {
                println!("\nFAIL: --emit-manifest {}: {e}", out_path.display());
                println!("\nOVERALL: FAIL");
                std::process::exit(1);
            }
        }
    }

    // ---- embedding alignment: refuse rather than mis-attribute -----------------------------------
    //
    // The embedding file is positional, so a count mismatch is not a rounding error — it means every
    // vector past the first discrepancy belongs to a different image. A silently truncated or
    // over-long file must never produce a recall figure, so this is fatal and prints both numbers.
    if let Some(vector_count) = embedding.count() {
        if vector_count != indexed.len() {
            println!("\nFAIL: --embedding holds {vector_count} vectors but this run indexes {} documents.",
                indexed.len());
            println!("      The file is POSITIONAL: entry i is the vector for indexed document i, so a");
            println!("      count mismatch mis-attributes vectors rather than merely losing some. Any");
            println!("      recall or nDCG computed over it would be a number about nothing.");
            println!("      Regenerate with --emit-manifest <path> and encode the paths in file order.");
            if arg.limit.is_some() {
                println!("      NOTE: --limit strides the corpus, so a manifest emitted at one --limit");
                println!("      does not describe a run at another. Emit and encode at the same limit.");
            }
            println!("\nOVERALL: FAIL");
            std::process::exit(1);
        }
    }

    // ---- schema, and the engine limit that shapes it --------------------------------------------
    //
    // `index_text::MAX_FIELD` is **4**. The columns this corpus yields are path, format, shape,
    // colour, width, height and byte length — **seven**, and an image document is therefore
    // column-hungrier than the text engine's budget. See "WHAT THIS RUN SURFACED" at the end: that
    // mismatch is a finding of this benchmark, not a detail of it.
    //
    // Raising `MAX_FIELD` is not the fix, and this bench will not ask for it: it is the width of
    // `[u16; MAX_FIELD]` on every posting in `term_post`, so widening it taxes every existing text
    // consumer to serve one image bench.
    //
    // The packing inside the budget: `path` scored, `format` and `colour` as the two facets
    // (colour because nothing else can stand in for it), and byte length as the single numeric
    // column (width and height are derivable and the census above already reports both
    // distributions). Orientation folds into the path text, where it is still queryable but is no
    // longer a hard predicate.
    let schema = || {
        Schema::new(vec![
            // The URL path IS the caption on a scrape; there is no other text.
            Field::new("path", 3.0, 0.5),
            // boost 0.0: these exist to be faceted/ranged on, not scored on.
            Field::new("format", 0.0, 0.75),
            Field::new("color", 0.0, 0.75),
            Field::new("byte", 0.0, 0.75),
        ])
    };

    struct Row {
        /// Path text, with the orientation appended — the column that lost its facet slot.
        text: String,
        format: &'static str,
        color: String,
        byte: String,
    }
    let row: Vec<Row> = indexed
        .iter()
        .map(|r| {
            let (w, h) = r.decoded_dim.unwrap();
            Row {
                text: format!("{} {}", path_text(&r.path), shape_of(w, h)),
                format: r.format.name(),
                // The dominant OKLab bucket, as a facet term. Real extracted signal, not a label.
                color: r
                    .palette
                    .as_ref()
                    .and_then(|p| p.term().first().map(|t| t.0.clone()))
                    .unwrap_or_else(|| "col:none".to_string()),
                byte: r.byte_len.to_string(),
            }
        })
        .collect();

    let build = |n: usize| -> ImageIndex {
        let mut b = ImageIndexBuilder::new(schema(), embedding.dim(), Metric::Cosine)
            .with_facet(1)
            .with_facet(2)
            .with_numeric(3);
        for (i, r) in indexed.iter().take(n).enumerate() {
            let (w, h) = r.decoded_dim.unwrap();
            let doc = ImageDoc {
                digest: r.digest,
                byte_len: r.byte_len,
                width: w,
                height: h,
                dhash: r.dhash,
                phash: r.phash,
                pdq: r.pdq,
                palette: r.palette.clone(),
                exif: r.exif.clone(),
            };
            let v = embedding.vector(i, &r.digest);
            let text = &row[i];
            let d = Doc::new(vec![
                text.text.as_str(),
                text.format,
                text.color.as_str(),
                text.byte.as_str(),
            ]);
            if let Err(e) = b.add(&doc, &d, v.as_deref()) {
                println!("    ADD FAILED {}: {e}", r.path);
            }
        }
        b.build().expect("index build")
    };

    let t_build = std::time::Instant::now();
    let ix = build(indexed.len());
    let build_s = t_build.elapsed().as_secs_f64();
    println!(
        "\n  index built: {} documents, {} embedded, {build_s:.1} s",
        ix.doc_count(),
        ix.embedded_count()
    );

    // Query terms: the most frequent path tokens, so every query hits real documents.
    let mut freq: HashMap<String, usize> = HashMap::new();
    for r in &row {
        for tok in r.text.split_whitespace() {
            if tok.len() >= 4 {
                *freq.entry(tok.to_lowercase()).or_default() += 1;
            }
        }
    }
    let mut term: Vec<(String, usize)> = freq.into_iter().collect();
    // Descending frequency, then lexicographic: deterministic, no clock, no randomness.
    term.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // Skip the handful of tokens every path carries (`storage`, `public`, ...): a term matching the
    // whole corpus is not a query, it is a scan.
    // The floor is 3, not `K`: a query matching fewer than `K` documents is the HARD case for the
    // short-page check (`min(k, matching_count)`), so excluding it would exclude the evidence.
    let probe: Vec<String> = term
        .iter()
        .filter(|(_, c)| *c * 10 < indexed.len() * 9 && *c >= 3)
        .map(|(t, _)| t.clone())
        .take(QUERY_COUNT)
        .collect();
    println!("  {} probe queries from real path tokens", probe.len());

    // =============================================================================================
    // 4. FUSED QUERY CORRECTNESS (p59)
    // =============================================================================================
    println!("\n=== 4. FUSED QUERY CORRECTNESS (p59) ===================================");
    let n_doc = ix.doc_count();
    let mut short = 0usize;
    let mut leak = 0usize;
    let mut mute = 0usize;
    let mut tried = 0usize;
    let mut example: Vec<String> = Vec::new();

    for (qi, q) in probe.iter().enumerate() {
        // Hard predicates drawn from real documents, so they select rather than empty the corpus.
        let anchor = &row[(qi * 37) % row.len()];
        let format_value = [anchor.format];
        let color_value = [anchor.color.as_str()];
        let mut clause = vec![FacetClause::any(0, &format_value)];
        // Half the queries also pin the colour bucket, so slot 1 is exercised rather than declared.
        if qi % 2 == 1 {
            clause.push(FacetClause::any(1, &color_value));
        }
        // A byte-length range wide enough to keep a page fillable, narrow enough to be a real
        // filter: it drops the sub-2-KiB tracking pixels and spacer GIFs a scrape is full of.
        let range = [(0usize, BYTE_RANGE_LO, BYTE_RANGE_HI)];

        let vec_doc = (qi * 7) % row.len();
        let qv = embedding.vector(vec_doc, &indexed[vec_doc].digest);
        let hash_doc = (qi * 13) % row.len();
        let qh = indexed[hash_doc].dhash;

        let fq = FusedQuery {
            text: Some(q.as_str()),
            facet: &clause,
            range: &range,
            vector: qv.as_deref(),
            hash: qh.map(|h| (h, hash::HASH64_NEAR_MAX)),
            alpha: None,
        };
        let hit = ix.search_fused(&fq, K);
        tried += 1;

        // --- 1. no short page ---
        // The oracle is the text arm's own full match count under the same hard predicates,
        // computed by a different entry point. Every one of those documents is a legal candidate,
        // so if there are at least K of them a full page is owed. This is exactly the defect a
        // fan-out architecture has structurally: it selects top-k per subsystem and intersects.
        let matching = ix.text().search_clause(q, n_doc, 0, &clause, &range).len();
        let owed = K.min(matching);
        if hit.len() < owed {
            short += 1;
            if example.len() < EXAMPLE_MAX {
                example.push(format!("SHORT {q:?}: {} of {owed} ({matching} match)", hit.len()));
            }
        }

        // --- 2. no leak ---
        for h in &hit {
            let format_ok = ix.text().facet_of_at(h.doc, 0) == Some(anchor.format);
            let color_ok = clause.len() < 2
                || ix.text().facet_of_at(h.doc, 1) == Some(anchor.color.as_str());
            let range_ok = ix
                .text()
                .numeric_of(h.doc, 0)
                .is_some_and(|v| v >= range[0].1 && v < range[0].2);
            if !format_ok || !color_ok || !range_ok {
                leak += 1;
                if example.len() < EXAMPLE_MAX {
                    example.push(format!(
                        "LEAK {q:?} doc {}: format {:?} wanted {:?}, colour {:?}, byte {:?}",
                        h.doc,
                        ix.text().facet_of_at(h.doc, 0),
                        anchor.format,
                        ix.text().facet_of_at(h.doc, 1),
                        ix.text().numeric_of(h.doc, 0)
                    ));
                }
            }
            // --- 3. explicability ---
            if h.why.agreement() == 0 {
                mute += 1;
            }
        }
    }
    println!("  {tried} fused queries at k={K}: text + facet(format) + facet(colour, every 2nd)");
    println!("  + range(byte >= {BYTE_RANGE_LO:.0}) + vector + dHash radius {}", hash::HASH64_NEAR_MAX);
    println!("  short pages: {short}   filter leaks: {leak}   hits with agreement 0: {mute}");
    for e in example.iter().take(EXAMPLE_MAX) {
        println!("    {e}");
    }
    check.push((format!("p59 zero short pages over {tried} fused queries"), short == 0));
    check.push((format!("p59 zero filter leaks over {tried} fused queries"), leak == 0));
    check.push(("p59 every hit has why.agreement() >= 1".to_string(), mute == 0));
    if short > 0 {
        fail += 1;
    }
    if leak > 0 {
        fail += 1;
    }
    if mute > 0 {
        fail += 1;
    }

    // --- p58 recall, or the refusal to report one -------------------------------------------------
    println!("\n  --- p58 vector recall vs the exact oracle ---");
    if embedding.is_synthetic() {
        println!("    WITHHELD. The vectors are SYNTHETIC — seeded noise with no visual semantics.");
        println!("    A recall figure over made-up vectors measures the arithmetic, not the");
        println!("    retrieval, and printing one would be the exact overclaim p58 exists to avoid.");
        println!("    docs/research/image.md §4 records the image-embedding recall number as");
        println!("    UNVERIFIED and it STAYS UNVERIFIED until a real encoder runs here.");
        withheld.push("p58 recall verdict (synthetic embedding mode)".to_string());
    } else {
        let mut recalled = 0usize;
        let mut expected = 0usize;
        for qi in 0..QUERY_COUNT.min(row.len()) {
            let d = (qi * 11) % row.len();
            let Some(qv) = embedding.vector(d, &indexed[d].digest) else { continue };
            let approx: Vec<u32> = ix.vector().search(&qv, 10, 4).into_iter().map(|(s, _)| s).collect();
            let exact = ix.vector().search_exact(&qv, 10);
            expected += exact.len();
            recalled += exact.iter().filter(|(s, _)| approx.contains(s)).count();
        }
        let recall = if expected == 0 { 0.0 } else { recalled as f64 / expected as f64 };
        println!("    recall@10 of the binary-prefilter pipeline vs search_exact: {recall:.4}");
        println!("    This is THIS REPO'S OWN measurement on IMAGE embeddings. Every published");
        println!("    binary-quantisation recall figure is for TEXT embeddings.");
        check.push((format!("p58 recall@10 = {recall:.4} (real embeddings)"), recall >= 0.98));
        if recall < 0.98 {
            fail += 1;
        }
    }

    // --- p59 acceptance 3: fused nDCG@10 against each half alone ----------------------------------
    //
    // Acceptance 3 says fused nDCG@10 must EXCEED both text-only and vector-only, and that a row
    // which fails "is wrong and must be rejected, not tuned until it passes". This block is written
    // to honour that literally: it reports the loss and fails the check. Nothing here is tuned to
    // make fusion win — the arms share one query set, one k and one scoring function.
    //
    // TWO labelled sets are reported and only ONE of them votes; the block below says at length
    // which and why. The short version: exact-duplicate labels make the vector arm perfect BY
    // CONSTRUCTION, so they cannot adjudicate "fused > vector" at all, and the near-duplicate set
    // with the exact duplicates removed is the one that can.
    println!("\n  --- p59 acceptance 3: nDCG@10, fused vs each half alone ---");
    if embedding.is_synthetic() {
        println!("    WITHHELD, for the same reason p58's recall is. The vectors are SYNTHETIC —");
        println!("    seeded noise keyed on the content digest, carrying no visual semantics. An");
        println!("    nDCG over them would measure that identical bytes hash identically, which is");
        println!("    arithmetic, not retrieval. Run with --embedding <path> for a verdict.");
        withheld.push("p59 acceptance 3 nDCG@10 verdict (synthetic embedding mode)".to_string());
    } else if ix.doc_count() != indexed.len() {
        // Document ordinals are assigned in insertion order, so the labels below are only valid if
        // every indexed record actually became a document. If one failed to add, say so and decline.
        println!("    WITHHELD: {} documents indexed but {} records offered, so a document ordinal",
            ix.doc_count(), indexed.len());
        println!("    no longer names the record it came from and the labels cannot be trusted.");
        withheld.push("p59 acceptance 3 nDCG@10 verdict (document ordinals not aligned)".to_string());
    } else {
        // ---- TWO labelled sets, and which one is allowed to vote ---------------------------------
        //
        // There are two, and only the second one votes. The reason is a property of the corpus, not
        // a preference, and it has to be written down because the first set is the obvious one:
        //
        //  [A] EXACT-DUPLICATE GROUPS, by sha256. Objective and abundant — membership falls out of
        //      the bytes, so nobody tuned the labels — but **DEGENERATE FOR ACCEPTANCE 3**.
        //      Byte-identical files decode to byte-identical pixels, so a real encoder assigns them
        //      byte-identical embeddings, so the vector arm retrieves the whole group at rank 1..n
        //      and scores nDCG@10 = 1.0 BY CONSTRUCTION. "Fused must exceed vector-only" is then
        //      unreachable no matter how good fusion is: nothing exceeds a perfect oracle. A FAIL
        //      printed off this set would be a verdict produced by the instrument, so this set is
        //      reported as INFORMATIONAL and is given a `[note]`, never a PASS or a FAIL.
        //
        //  [B] NEAR-DUPLICATE GROUPS WITH THE EXACT DUPLICATES REMOVED: dHash Hamming distance
        //      within `hash::HASH64_NEAR_MAX`, AND a DIFFERENT sha256. These are the same image
        //      resized or re-encoded, which is what makes the set non-degenerate:
        //        - the vector arm is strong but NOT perfect, because the pixels genuinely differ;
        //        - the text arm is independent, because the paths genuinely differ;
        //        - the label comes from a THIRD function — a perceptual hash — which is neither the
        //          CLIP embedding nor the path, so it advantages neither arm by construction.
        //      Acceptance 3 is adjudicated here and nowhere else. If fusion loses on this set that
        //      is a rejection of the row, not an invitation to tune, and it is printed as measured.
        //
        // Both sets exclude the query document from its own result list before scoring, and both
        // measure the vector arm with `search_exact`, so a fusion win can never be an artefact of
        // quantisation loss in the arm it is being compared against.

        /// The three arms over one labelled set, plus how much of the set actually scored.
        struct Score {
            scored: usize,
            skipped: usize,
            relevant_total: usize,
            text: f64,
            vector: f64,
            fused: f64,
        }

        // One scoring function, shared by both sets: same k, same query construction, same
        // self-exclusion. Anything that differs between the two blocks below is the LABELS.
        let evaluate = |label: &[(u32, HashSet<u32>)]| -> Score {
            let (mut text_sum, mut vector_sum, mut fused_sum) = (0.0f64, 0.0f64, 0.0f64);
            let (mut scored, mut skipped, mut relevant_total) = (0usize, 0usize, 0usize);
            for (q_doc, relevant) in label.iter().take(QUERY_COUNT) {
                let q_doc = *q_doc;
                let Some(qv) = embedding.vector(q_doc as usize, &indexed[q_doc as usize].digest)
                else {
                    skipped += 1;
                    continue;
                };
                let q_text = row[q_doc as usize].text.as_str();

                // k+1 everywhere, then drop the query document: it matches itself perfectly on both
                // arms, and leaving it in would inflate all three numbers with a trivial self-match.
                let take = NDCG_K + 1;
                let drop_self = |doc: u32| doc != q_doc;

                let text_rank: Vec<u32> = ix
                    .text()
                    .search(q_text, take)
                    .into_iter()
                    .map(|h| h.doc)
                    .filter(|&d| drop_self(d))
                    .take(NDCG_K)
                    .collect();
                // `search_exact`, not the binary-prefilter pipeline: the vector arm is measured at
                // its BEST, so "fusion beats vector-only" is not an artefact of quantisation loss.
                let vector_rank: Vec<u32> = ix
                    .vector()
                    .search_exact(&qv, take)
                    .into_iter()
                    .map(|(d, _)| d)
                    .filter(|&d| drop_self(d))
                    .take(NDCG_K)
                    .collect();
                let fq = FusedQuery {
                    text: Some(q_text),
                    vector: Some(&qv),
                    ..FusedQuery::default()
                };
                let fused_rank: Vec<u32> = ix
                    .search_fused(&fq, take)
                    .into_iter()
                    .map(|h| h.doc)
                    .filter(|&d| drop_self(d))
                    .take(NDCG_K)
                    .collect();

                text_sum += ndcg(&text_rank, relevant, NDCG_K);
                vector_sum += ndcg(&vector_rank, relevant, NDCG_K);
                fused_sum += ndcg(&fused_rank, relevant, NDCG_K);
                relevant_total += relevant.len();
                scored += 1;
            }
            let n = scored.max(1) as f64;
            Score {
                scored,
                skipped,
                relevant_total,
                text: text_sum / n,
                vector: vector_sum / n,
                fused: fused_sum / n,
            }
        };

        let print_arm = |s: &Score| {
            println!("\n    {:<24} {:>10}", "arm", "nDCG@10");
            println!("    {:<24} {:>10.4}", "text-only", s.text);
            println!("    {:<24} {:>10.4}", "vector-only (exact)", s.vector);
            println!("    {:<24} {:>10.4}", "FUSED", s.fused);
            println!("    delta vs text-only:   {:+.4}", s.fused - s.text);
            println!("    delta vs vector-only: {:+.4}", s.fused - s.vector);
        };

        // ---- [A] the exact-duplicate labelled set ------------------------------------------------
        let mut group: HashMap<[u8; 32], Vec<u32>> = HashMap::new();
        for (i, r) in indexed.iter().enumerate() {
            group.entry(r.digest).or_default().push(i as u32);
        }
        // Sorted by digest: deterministic, and independent of hash-map iteration order.
        let mut dup_set: Vec<([u8; 32], Vec<u32>)> =
            group.into_iter().filter(|(_, m)| m.len() >= 2).collect();
        dup_set.sort_by_key(|entry| entry.0);
        // Members were pushed in indexing order, so member[0] is the lowest ordinal — a fixed,
        // clock-free choice of query document.
        let exact_label: Vec<(u32, HashSet<u32>)> = dup_set
            .iter()
            .map(|(_, member)| (member[0], member[1..].iter().copied().collect()))
            .collect();

        println!("\n    [A] EXACT-DUPLICATE LABELS (sha256) — INFORMATIONAL, CANNOT ADJUDICATE");
        if exact_label.is_empty() {
            println!("      no exact-duplicate group in this corpus slice, so nothing to report.");
        } else {
            let a = evaluate(&exact_label);
            if a.scored == 0 {
                println!(
                    "      {} group(s) found but none had a vector for the query document.",
                    exact_label.len()
                );
            } else {
                println!(
                    "      {} queries over exact-duplicate groups, {} relevant documents in total",
                    a.scored, a.relevant_total
                );
                println!("      ({:.1} per query).", a.relevant_total as f64 / a.scored as f64);
                if a.skipped > 0 {
                    println!("      {} group(s) skipped: no vector for the query document.", a.skipped);
                }
                print_arm(&a);
                println!("\n      -> THIS SET CANNOT ADJUDICATE p50 ACCEPTANCE 3, whatever it prints.");
                println!("         Byte-identical files decode to identical pixels, so a real encoder");
                println!("         gives them identical embeddings and the vector arm returns the whole");
                println!("         group first: nDCG@10 = 1.0 BY CONSTRUCTION. `fused > vector` is then");
                println!("         unreachable for a reason that has nothing to do with whether fusion");
                println!("         works — nothing exceeds a perfect oracle. Reporting a FAIL from this");
                println!("         set would be a verdict produced by the instrument, so it gets a");
                println!("         [note] and set [B] below carries the verdict.");
                note.push(format!(
                    "p59 acc.3 on EXACT-duplicate labels: fused {:.4}, text {:.4}, vector {:.4} ({} queries) — cannot adjudicate, the vector arm is perfect by construction",
                    a.fused, a.text, a.vector, a.scored
                ));
            }
        }

        // ---- [B] the near-duplicate labelled set, exact duplicates REMOVED -----------------------
        //
        // A query document per DISTINCT digest, taken in ordinal order, so a file that happens to
        // have twelve byte-identical copies contributes one query rather than twelve. Its relevant
        // set is every document within `HASH64_NEAR_MAX` dHash of it whose sha256 DIFFERS — the same
        // picture at another size or another encoder setting, never another copy of the same bytes.
        let mut near_label: Vec<(u32, HashSet<u32>)> = Vec::new();
        let mut query_digest: HashSet<[u8; 32]> = HashSet::new();
        for (i, r) in indexed.iter().enumerate() {
            if near_label.len() >= QUERY_COUNT {
                break;
            }
            let Some(qh) = r.dhash else { continue };
            if !query_digest.insert(r.digest) {
                continue;
            }
            let mut relevant: HashSet<u32> = HashSet::new();
            for (j, other) in indexed.iter().enumerate() {
                if j == i || other.digest == r.digest {
                    continue;
                }
                if other.dhash.is_some_and(|oh| qh.distance(&oh) <= hash::HASH64_NEAR_MAX) {
                    relevant.insert(j as u32);
                }
            }
            if !relevant.is_empty() {
                near_label.push((i as u32, relevant));
            }
        }

        println!(
            "\n    [B] NEAR-DUPLICATE LABELS (dHash <= {}, sha256 DIFFERS) — THIS SET VOTES",
            hash::HASH64_NEAR_MAX
        );
        let b = evaluate(&near_label);
        if b.scored < NDCG_QUERY_MIN {
            println!(
                "      {} scorable quer(ies) — under the {NDCG_QUERY_MIN} this benchmark requires",
                b.scored
            );
            println!(
                "      before it renders a verdict. {} candidate group(s) were built and {} skipped",
                near_label.len(), b.skipped
            );
            println!("      for want of a vector. At this corpus size the non-degenerate set is NOT");
            println!("      TESTABLE, and an underpowered pass is worth nothing, so the verdict is");
            println!("      WITHHELD the same way p58's recall is under synthetic embeddings.");
            if b.scored > 0 {
                print_arm(&b);
                println!("\n      (printed for information only — too few queries to mean anything.)");
            }
            withheld.push(format!(
                "p59 acceptance 3 nDCG@10 verdict (non-degenerate set has {} quer(ies), under {NDCG_QUERY_MIN})",
                b.scored
            ));
        } else {
            println!(
                "      {} queries, one per distinct digest, {} relevant documents in total",
                b.scored, b.relevant_total
            );
            println!(
                "      ({:.1} per query). The query document is fed its own indexed path text and",
                b.relevant_total as f64 / b.scored as f64
            );
            println!("      its own embedding, and is excluded from its own results before scoring.");
            if b.skipped > 0 {
                println!("      {} group(s) skipped: no vector for the query document.", b.skipped);
            }
            println!("      NON-DEGENERATE BY CONSTRUCTION: the pixels differ, so the vector arm is");
            println!("      strong but not perfect; the paths differ, so the text arm is independent;");
            println!("      and the label comes from a perceptual hash, which is neither arm's own");
            println!("      function, so it hands neither of them a free win.");
            print_arm(&b);

            let beat = b.fused > b.text && b.fused > b.vector;
            if beat {
                println!("\n      -> fusion strictly exceeds both halves on the NON-DEGENERATE set.");
                println!("         p59 acceptance 3 HOLDS.");
            } else {
                let lose = if b.fused <= b.text && b.fused <= b.vector {
                    "BOTH halves"
                } else if b.fused <= b.text {
                    "the TEXT half"
                } else {
                    "the VECTOR half"
                };
                println!("\n      -> FUSION LOSES TO {lose} on the non-degenerate set. Per p50");
                println!("         acceptance 3 this is a FAILING result and the row is wrong, not");
                println!("         under-tuned. It is printed as measured; nothing here is adjusted");
                println!("         until it passes.");
            }
            println!("\n      LIMIT, stated beside the number rather than in a footnote: this measures");
            println!("      NEAR-DUPLICATE RETRIEVAL, not semantic relevance. Nothing here shows that a");
            println!("      fused query finds a semantically SIMILAR image, only that it finds the same");
            println!("      image at another size or encoding. It is the strongest non-degenerate set");
            println!("      this corpus can yield without human judgements — a scrape carries no");
            println!("      relevance labels — and a real semantic evaluation needs a corpus with them.");

            check.push((
                format!(
                    "p59 acc.3 fused nDCG@10 {:.4} > text {:.4} and vector {:.4} (NON-DEGENERATE near-duplicate labels, {} queries)",
                    b.fused, b.text, b.vector, b.scored
                ),
                beat,
            ));
            if !beat {
                fail += 1;
            }
        }
    }

    // =============================================================================================
    // 5. LATENCY
    // =============================================================================================
    println!("\n=== 5. LATENCY =========================================================");
    let marker = if embedding.is_synthetic() { "  [vector arms SYNTHETIC]" } else { "" };
    println!("  p50/p99 per query, rdtsc-timed, single-thread.{marker}");
    println!("  {:<10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "docs", "text p50", "text p99", "vec p50", "vec p99", "fused p50", "fused p99");

    let mut size_step: Vec<usize> = vec![1000, 4000, indexed.len()];
    size_step.retain(|&s| s <= indexed.len());
    size_step.dedup();
    let mut fused_p50_full = 0.0;
    let mut fused_p99_full = 0.0;
    let mut text_p50_full = 0.0;
    for &n in &size_step {
        let sub = if n == indexed.len() { None } else { Some(build(n)) };
        let cur = sub.as_ref().unwrap_or(&ix);

        let mut t = clock.time_each(probe.len(), |i| cur.text().search(&probe[i], K).len() as u64);
        t.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let qv: Vec<Vec<f32>> = (0..probe.len())
            .filter_map(|i| embedding.vector((i * 7) % n, &indexed[(i * 7) % n].digest))
            .collect();
        let mut v = clock.time_each(qv.len(), |i| cur.vector().search(&qv[i], K, 4).len() as u64);
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mut f = clock.time_each(probe.len(), |i| {
            let fq = FusedQuery {
                text: Some(probe[i].as_str()),
                vector: qv.get(i).map(|x| x.as_slice()),
                hash: indexed[(i * 13) % n].dhash.map(|h| (h, hash::HASH64_NEAR_MAX)),
                ..FusedQuery::default()
            };
            cur.search_fused(&fq, K).len() as u64
        });
        f.sort_by(|a, b| a.partial_cmp(b).unwrap());

        println!("  {:<10} {:>8.0}us {:>8.0}us {:>8.0}us {:>8.0}us {:>8.0}us {:>8.0}us",
            n, us(&t, 0.5), us(&t, 0.99), us(&v, 0.5), us(&v, 0.99), us(&f, 0.5), us(&f, 0.99));
        if n == indexed.len() {
            fused_p50_full = us(&f, 0.5);
            fused_p99_full = us(&f, 0.99);
            text_p50_full = us(&t, 0.5);
        }
    }
    println!("\n  -> fusion costs {:.1}x the text-only p50 at full corpus ({:.0}us vs {:.0}us);",
        if text_p50_full > 0.0 { fused_p50_full / text_p50_full } else { 0.0 },
        fused_p50_full, text_p50_full);
    println!("     p99 {fused_p99_full:.0}us. A number, not a claim — p59 acceptance 6.");

    // =============================================================================================
    // 6. INDEX SIZE
    // =============================================================================================
    println!("\n=== 6. INDEX SIZE ======================================================");
    let text_byte = ix.text().to_bytes().len();
    let vector_byte = ix.vector().byte_len();
    // The hash and digest columns as `ImageIndex` actually holds them.
    let column_byte = ix.doc_count()
        * (std::mem::size_of::<Option<Hash64>>() * 2
            + 32
            + std::mem::size_of::<Option<u32>>()
            + std::mem::size_of::<u32>());
    let total = text_byte + vector_byte.total() + column_byte;
    let n = ix.doc_count().max(1);
    println!("  {:<26} {:>12} {:>12}", "component", "bytes", "B/image");
    println!("  {:<26} {:>12} {:>12.1}", "text index (to_bytes)", text_byte, text_byte as f64 / n as f64);
    println!("  {:<26} {:>12} {:>12.1}", "  vector: binary", vector_byte.binary, vector_byte.binary as f64 / n as f64);
    println!("  {:<26} {:>12} {:>12.1}", "  vector: int8", vector_byte.int8, vector_byte.int8 as f64 / n as f64);
    println!("  {:<26} {:>12} {:>12.1}", "  vector: scale", vector_byte.scale, vector_byte.scale as f64 / n as f64);
    println!("  {:<26} {:>12} {:>12.1}", "  vector: exact f32", vector_byte.exact, vector_byte.exact as f64 / n as f64);
    println!("  {:<26} {:>12} {:>12.1}", "hash + digest columns", column_byte, column_byte as f64 / n as f64);
    println!("  {:<26} {:>12} {:>12.1}", "TOTAL", total, total as f64 / n as f64);
    let indexed_source: u64 = indexed.iter().map(|r| r.byte_len).sum();
    println!("\n  source bytes for the {} indexed images: {} ({:.1} MB)",
        indexed.len(), indexed_source, indexed_source as f64 / 1e6);
    println!("  index is {:.2}% of the images it indexes{}",
        100.0 * total as f64 / indexed_source.max(1) as f64,
        if embedding.is_synthetic() { "  [vector tiers SYNTHETIC-sized but dimensionally real]" } else { "" });

    // =============================================================================================
    // What this run surfaced — findings the spec did not predict
    // =============================================================================================
    println!("\n=== WHAT THIS RUN SURFACED =============================================");
    println!("  1. AN IMAGE DOCUMENT IS COLUMN-HUNGRIER THAN THE TEXT ENGINE'S BUDGET.");
    println!("     This corpus yields seven columns per image — path, format, orientation, colour");
    println!("     bucket, width, height, byte length — and `index_text::MAX_FIELD` is 4. Three of");
    println!("     them could not be indexed at all: orientation kept only as text, and height and");
    println!("     width have no numeric column, so neither can carry a hard range predicate.");
    println!("     This is not a bench limitation to route around. `p59` claims one query plan over");
    println!("     text, facet, numeric, vector and hash predicates; that claim is bounded by how");
    println!("     many predicates fit. Raising MAX_FIELD is the WRONG fix — it is the width of");
    println!("     `[u16; MAX_FIELD]` on every posting, so it taxes every text consumer to serve");
    println!("     this one tier. A candidate roadmap row, recorded rather than papered over.");
    println!();
    println!("  2. ON THIS CORPUS THE FILE EXTENSION DOES NOT LIE.");
    println!("     `meta::sniff` disagreed with the extension on {divergence} of {} files ({:.2}%).",
        record.len(), 100.0 * divergence as f64 / record.len() as f64);
    if divergence == 0 {
        println!("     That CONTRADICTS the expectation that scraped assets carry lying extensions.");
        println!("     One Supabase-backed scrape is not the web: this host serves what it stored,");
        println!("     and the scraper wrote the extension from the URL, so the two agree by");
        println!("     construction. The sniff is still the right default — this run just does not");
        println!("     produce evidence for it, and says so instead of quietly implying otherwise.");
    } else {
        println!("     That CONFIRMS the expectation that a scraped extension is a claim, not");
        println!("     evidence, and the sniff is what should be believed.");
    }
    println!();
    println!("  3. THE NEAR-DUPLICATE RATE IS A CHOICE, NOT A MEASUREMENT.");
    println!("     The dHash table above spans thresholds 0..12 and the PDQ table 0..48. Both move");
    println!("     the answer by tens of percentage points. Any single headline rate for this");
    println!("     corpus would be the threshold talking, which is exactly §5's warning.");

    // =============================================================================================
    // Check list
    // =============================================================================================
    println!("\n=== CHECK LIST =========================================================");
    for (name, ok) in &check {
        println!("  [{}] {name}", if *ok { "PASS" } else { "FAIL" });
    }
    for name in &note {
        println!("  [note] {name}");
    }
    for name in &withheld {
        println!("  [HELD] {name}");
    }
    if !withheld.is_empty() {
        println!("\n  {} verdict(s) WITHHELD. This run does not pass p58 — it declines to answer it.",
            withheld.len());
    }
    println!("\nOVERALL: {}", if fail == 0 { "PASS" } else { "FAIL" });
    std::process::exit(if fail == 0 { 0 } else { 1 });
}
