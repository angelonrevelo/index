//! Container structure and metadata, parsed straight out of hostile bytes.
//!
//! # What this module does, and the much larger thing it refuses to do
//!
//! It reads **structure**, never pixels. Format sniffing, dimensions, EXIF/TIFF tags, the JPEG
//! quantisation tables and the embedded thumbnail are all available from a few hundred bytes near
//! the front of a file. Every one of those is a *column* — a facet term or a numeric range — and
//! `index-text` can answer `camera = "iPhone 15" AND 2019 <= year < 2021` over them without any
//! image ever being decoded. That is the whole reason `index-image` can stay dependency-free
//! (see `lib.rs`: no decoder, no ONNX, no C++ toolchain). Metadata does not need a decoder, and
//! pixels are the host's problem.
//!
//! # Metadata is normally ABSENT, and this API is shaped around that
//!
//! `docs/research/image.md` §5: X/Twitter, Instagram, Facebook, TikTok, Snapchat, LinkedIn, Reddit
//! and Signal all strip EXIF from the copy you can download. WhatsApp, Telegram and Discord
//! preserve it only on the "send as file / document" path — the ordinary photo path re-encodes and
//! drops it. In a scraped OSINT corpus the overwhelming majority of images therefore carry **no**
//! EXIF at all.
//!
//! So a missing tag is not an error, and nothing here returns `Err`. [`parse`] on a
//! stripped-but-valid JPEG returns `Some(Exif::default())` — *"I read the block and there was
//! nothing in it"* — which is a materially different fact from `None`, *"there was no block to
//! read."* An indexer that treated absence as failure would reject most of its corpus.
//!
//! Two corollaries, stated because they are routinely got wrong:
//!
//!   - **Stripping protects the uploader from other USERS, not from the platform.** The platform
//!     received, read and retains the original, GPS included. Absence in the downloadable copy is
//!     therefore *not* evidence of absence at the source, and must never be reported as "the photo
//!     had no location".
//!   - Because stripping is near-universal, the *presence* of intact EXIF is itself a signal: it
//!     usually means the file did not travel through a consumer social platform — a direct upload,
//!     a document-path message, a file share, or a camera roll.
//!
//! # The forensic signals here, honestly scoped
//!
//! **Quantisation tables** ([`jpeg_qtable`]). The DQT marker carries the exact 64-entry tables the
//! encoder used. They vary by camera model and by software (peer-reviewed: *JPEG Quantization
//! Tables Forensics*, Springer) and they survive metadata stripping, because they are structural
//! rather than ancillary — a platform that deletes EXIF still leaves the tables of its own
//! re-encode, and a table set that does not match the claimed camera is evidence of a re-encode
//! the file does not admit to. They **narrow** a source; they do not identify one. Many devices
//! share a table set and the standard IJG tables are shared by everything, so a fingerprint is a
//! facet with large equivalence classes, never a device serial number.
//!
//! **The embedded thumbnail** ([`Exif::thumb_slice`]). IFD1 carries a small JPEG that editors do
//! not always regenerate, so a thumbnail can disagree with an edited main image — the classic
//! result being a face or a plate still legible in a 160×120 preview after being cropped out of
//! the full frame. It is a real signal, and it is also trivially defeated: one `exiftool`
//! invocation regenerates or deletes it. It is evidence when it disagrees and no evidence at all
//! when it agrees.
//!
//! # Threat model: this module eats bytes an attacker chose
//!
//! A scraped corpus contains files crafted to break the parser. Every guarantee below is enforced
//! structurally rather than by care:
//!
//!   - no slice is ever indexed by an attacker-controlled offset — every read goes through [`at`],
//!     which returns `Option`;
//!   - IFD chains may be **cyclic**; a visited-offset set plus [`MAX_IFD`] bounds the walk;
//!   - claimed component counts may be absurd; nothing is allocated proportional to a number from
//!     the file ([`MAX_TEXT_BYTE`] caps the only string copy, and a quantisation table is a fixed
//!     64 entries);
//!   - no `unwrap`, no `expect`, no unbounded loop, and all offset arithmetic is checked or
//!     saturating, so a debug build cannot panic where a release build would silently wrap.
//!
//! Malformed input yields `None` or an empty result. The test module feeds truncations at every
//! length, a self-referential IFD, out-of-range offsets, absurd counts and several thousand
//! seeded-random buffers, and asserts exactly one thing: the parser returns.

use std::collections::BTreeSet;

/// Hard cap on IFDs visited in one file. Real files use three or four (IFD0, Exif, GPS, IFD1);
/// this exists so a crafted pointer graph cannot make the walk long even without a cycle.
pub const MAX_IFD: usize = 32;

/// Hard cap on directory entries read per IFD. The largest legitimate IFD seen in the wild is well
/// under a hundred; the count field is a `u16`, so an attacker may claim 65 535.
pub const MAX_ENTRY: usize = 512;

/// Longest string copied out of a tag. ASCII tags are attacker-sized, and this is the only place
/// the module copies a variable amount of input, so it is the only place needing a ceiling.
pub const MAX_TEXT_BYTE: usize = 512;

/// Cap on JPEG segments or PNG/RIFF chunks walked. Bounds a scan over a file that is all filler.
pub const MAX_SEGMENT: usize = 4096;

/// Cap on ISO-BMFF boxes walked when looking for HEIF/AVIF dimensions.
pub const MAX_BOX: usize = 512;

// ---------------------------------------------------------------------------------------------
// bounds-checked primitive reads
// ---------------------------------------------------------------------------------------------

/// `byte[off .. off + len]`, or `None`.
///
/// The single choke point for every read in this module. If a panic is possible anywhere in the
/// parser it is possible here — and here it is one line long, with the overflow checked.
#[inline]
pub fn at(byte: &[u8], off: usize, len: usize) -> Option<&[u8]> {
    byte.get(off..off.checked_add(len)?)
}

#[inline]
fn u16_at(byte: &[u8], off: usize, le: bool) -> Option<u16> {
    let b = at(byte, off, 2)?;
    let a = [b[0], b[1]];
    Some(if le { u16::from_le_bytes(a) } else { u16::from_be_bytes(a) })
}

#[inline]
fn u32_at(byte: &[u8], off: usize, le: bool) -> Option<u32> {
    let b = at(byte, off, 4)?;
    let a = [b[0], b[1], b[2], b[3]];
    Some(if le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) })
}

#[inline]
fn be16(byte: &[u8], off: usize) -> Option<u16> {
    u16_at(byte, off, false)
}

#[inline]
fn be32(byte: &[u8], off: usize) -> Option<u32> {
    u32_at(byte, off, false)
}

#[inline]
fn le32(byte: &[u8], off: usize) -> Option<u32> {
    u32_at(byte, off, true)
}

// ---------------------------------------------------------------------------------------------
// format sniffing
// ---------------------------------------------------------------------------------------------

/// The container, identified by magic bytes rather than by file extension.
///
/// Extension is attacker-controlled and, in a scraped corpus, frequently just wrong: CDNs serve
/// `.jpg` URLs that are WebP, and a `.png` that is really a JPEG is the oldest trick in the
/// content-filter-evasion book. Sniffing is the only honest answer and it costs twelve bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// Nothing recognised — the correct and common answer for an HTML error page saved as `.jpg`.
    #[default]
    Unknown,
    Jpeg,
    Png,
    /// RIFF/WEBP, in any of its three frame flavours (`VP8 `, `VP8L`, `VP8X`).
    Webp,
    Gif,
    /// ISO-BMFF with an `ftyp` brand in the HEIF family — covers AVIF, HEIC and relatives.
    Heif,
}

impl Format {
    /// A stable lowercase name, for use as a facet term.
    pub fn name(&self) -> &'static str {
        match self {
            Format::Unknown => "unknown",
            Format::Jpeg => "jpeg",
            Format::Png => "png",
            Format::Webp => "webp",
            Format::Gif => "gif",
            Format::Heif => "heif",
        }
    }
}

/// What a cheap structural read learned about a file.
///
/// `width`/`height` are `Option` because "I know the container but not the size" is a real state: a
/// JPEG truncated before its SOF marker, or a HEIF whose `ispe` box sits past the bytes handed
/// over. A partial answer beats a refusal — the format alone is already a usable facet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Probe {
    pub format: Format,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// Identify the container from its magic bytes. Reads nothing beyond what it checks.
pub fn sniff(byte: &[u8]) -> Format {
    if at(byte, 0, 2) == Some(&[0xFF, 0xD8]) {
        return Format::Jpeg;
    }
    if at(byte, 0, 8) == Some(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Format::Png;
    }
    if at(byte, 0, 4) == Some(b"RIFF") && at(byte, 8, 4) == Some(b"WEBP") {
        return Format::Webp;
    }
    if at(byte, 0, 6) == Some(b"GIF87a") || at(byte, 0, 6) == Some(b"GIF89a") {
        return Format::Gif;
    }
    // ISO-BMFF: the first box is `ftyp` at offset 4 and the major brand follows it. The brand list
    // is open-ended (`avif`, `avis`, `heic`, `heix`, `mif1`, `msf1`, `heim`, `hevc`, ...); an
    // unknown brand is deliberately NOT treated as HEIF, because `ftyp` also introduces MP4, and
    // calling a video an image is worse than calling it unknown.
    if at(byte, 4, 4) == Some(b"ftyp") {
        if let Some(brand) = at(byte, 8, 4) {
            const FAMILY: [&[u8; 4]; 10] = [
                b"avif", b"avis", b"heic", b"heix", b"hevc", b"hevx", b"mif1", b"msf1", b"heim",
                b"heis",
            ];
            if FAMILY.iter().any(|f| f.as_slice() == brand) {
                return Format::Heif;
            }
        }
    }
    Format::Unknown
}

/// Sniff the container and, where it is cheap, its pixel dimensions.
///
/// "Cheap" means a fixed-position header (PNG IHDR, GIF screen descriptor, WebP frame header) or a
/// short bounded walk of segment/box headers (JPEG SOFn, HEIF `ispe`). No entropy-coded data is
/// ever touched, so the cost is bounded by the header region rather than by the file.
pub fn probe(byte: &[u8]) -> Probe {
    let format = sniff(byte);
    let dim = match format {
        Format::Jpeg => jpeg_dimension(byte),
        Format::Png => png_dimension(byte),
        Format::Webp => webp_dimension(byte),
        Format::Gif => gif_dimension(byte),
        Format::Heif => heif_dimension(byte),
        Format::Unknown => None,
    };
    Probe { format, width: dim.map(|d| d.0), height: dim.map(|d| d.1) }
}

// ---------------------------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------------------------

/// Walk JPEG marker segments, calling `visit(marker, payload)` for each; `visit` returns `false` to
/// stop early.
///
/// The walk stops at SOS (`FFDA`), because everything structural — SOFn, DQT, APP1/EXIF — precedes
/// the first scan, and past it the bytes are entropy-coded and full of accidental `FF` pairs.
/// Refusing to walk compressed data is what keeps this loop both bounded and meaningful.
fn jpeg_segment<'a>(byte: &'a [u8], mut visit: impl FnMut(u8, &'a [u8]) -> bool) {
    if at(byte, 0, 2) != Some(&[0xFF, 0xD8]) {
        return;
    }
    let mut off = 2usize;
    for _ in 0..MAX_SEGMENT {
        let Some(&b) = byte.get(off) else { return };
        if b != 0xFF {
            // Not at a marker. One byte of resynchronisation rather than a bail-out, because real
            // files from real cameras do carry stray padding between segments.
            off = off.saturating_add(1);
            continue;
        }
        let Some(&marker) = byte.get(off.saturating_add(1)) else { return };
        match marker {
            // Fill byte: `FF FF ... FF xx` is a legal way to pad before a marker.
            0xFF => {
                off = off.saturating_add(1);
                continue;
            }
            // Standalone markers with no length field: SOI, TEM, RSTn.
            0xD8 | 0x01 | 0xD0..=0xD7 => {
                off = off.saturating_add(2);
                continue;
            }
            0xD9 => return, // EOI
            _ => {}
        }
        let Some(len) = be16(byte, off.saturating_add(2)) else { return };
        let len = len as usize;
        if len < 2 {
            return; // the length counts its own two bytes; below that the file is nonsense
        }
        let start = off.saturating_add(4);
        let end = off.saturating_add(2).saturating_add(len);
        let Some(payload) = byte.get(start..end) else { return };
        if !visit(marker, payload) {
            return;
        }
        if marker == 0xDA {
            return;
        }
        off = end;
    }
}

/// Dimensions from the first SOFn marker.
///
/// SOF0/1/2 are baseline/extended/progressive, but `C4`, `C8` and `CC` are DHT, JPG and DAC and are
/// **not** frame headers despite sitting in the same numeric range. Mistaking one is the classic
/// bug in a hand-rolled JPEG header reader, and it reads a Huffman table as a picture size.
fn jpeg_dimension(byte: &[u8]) -> Option<(u32, u32)> {
    let mut out = None;
    jpeg_segment(byte, |marker, payload| {
        let is_sof =
            (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC;
        if is_sof {
            // payload: precision u8, height u16, width u16, component count u8
            if let (Some(h), Some(w)) = (be16(payload, 1), be16(payload, 3)) {
                out = Some((u32::from(w), u32::from(h)));
            }
            return false;
        }
        true
    });
    out
}

/// One quantisation table, as the encoder wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qtable {
    /// Destination identifier, 0–3. Conventionally 0 is luma and 1 is chroma.
    pub id: u8,
    /// 0 for 8-bit entries, 1 for 16-bit. 16-bit is rare outside high-precision encoders.
    pub precision: u8,
    /// The 64 coefficients in the zig-zag order they are stored in — deliberately NOT
    /// de-zig-zagged, because the fingerprint should be over the bytes the encoder emitted.
    pub value: Vec<u16>,
}

/// Every quantisation table in a JPEG's DQT markers.
///
/// A forensic **narrowing** signal, not an identifier — see the module documentation. Returns an
/// empty vector for a non-JPEG, for a JPEG with no DQT, and for one truncated before it; the caller
/// cannot tell those apart and should not need to.
pub fn jpeg_qtable(byte: &[u8]) -> Vec<Qtable> {
    let mut out = Vec::new();
    jpeg_segment(byte, |marker, payload| {
        if marker != 0xDB {
            return true;
        }
        // A single DQT segment may carry several tables back to back.
        let mut off = 0usize;
        while let Some(&head) = payload.get(off) {
            let precision = head >> 4;
            let id = head & 0x0F;
            if precision > 1 || id > 3 {
                return true; // malformed header byte: abandon this segment, keep the file
            }
            let unit = if precision == 0 { 1usize } else { 2 };
            let Some(body) = at(payload, off.saturating_add(1), 64 * unit) else { return true };
            // A constant 64 entries. The allocation size never comes from the file.
            let mut value = Vec::with_capacity(64);
            if precision == 0 {
                for &v in body.iter().take(64) {
                    value.push(u16::from(v));
                }
            } else {
                for c in body.chunks_exact(2).take(64) {
                    value.push(u16::from_be_bytes([c[0], c[1]]));
                }
            }
            if value.len() != 64 {
                return true;
            }
            out.push(Qtable { id, precision, value });
            off = off.saturating_add(1).saturating_add(64 * unit);
            if out.len() >= 8 {
                return false; // four destinations, twice over; beyond that it is padding or junk
            }
        }
        true
    });
    out
}

/// A 64-bit FNV-1a digest over every table, for use as a single facet term.
///
/// Order-sensitive and precision-sensitive by design: two encoders emitting the same numbers in a
/// different destination order are different encoders. Equal fingerprints mean "same table set",
/// which is an equivalence class of devices and software — never one device.
pub fn qtable_fingerprint(table: &[Qtable]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for t in table {
        eat(t.id);
        eat(t.precision);
        for v in &t.value {
            eat((v >> 8) as u8);
            eat(*v as u8);
        }
    }
    h
}

// ---------------------------------------------------------------------------------------------
// PNG / WebP / GIF / HEIF structure
// ---------------------------------------------------------------------------------------------

/// Walk PNG chunks, calling `visit(kind, data)`. Stops at IEND or at the first malformed length.
///
/// CRCs are read past but not verified. Verification would reject files that every browser and
/// every scraper happily displays, and this module's job is to extract what is there rather than to
/// referee conformance.
fn png_chunk<'a>(byte: &'a [u8], mut visit: impl FnMut(&'a [u8], &'a [u8]) -> bool) {
    if at(byte, 0, 8) != Some(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return;
    }
    let mut off = 8usize;
    for _ in 0..MAX_SEGMENT {
        let Some(len) = be32(byte, off) else { return };
        let len = len as usize;
        // The spec caps a chunk at 2^31-1. Anything past the buffer would fail the read below in
        // any case; rejecting early keeps the arithmetic visibly in range.
        if len > byte.len() {
            return;
        }
        let Some(kind) = at(byte, off.saturating_add(4), 4) else { return };
        let Some(data) = at(byte, off.saturating_add(8), len) else { return };
        if !visit(kind, data) {
            return;
        }
        if kind == b"IEND" {
            return;
        }
        off = off.saturating_add(12).saturating_add(len); // 4 length + 4 type + data + 4 CRC
    }
}

fn png_dimension(byte: &[u8]) -> Option<(u32, u32)> {
    let mut out = None;
    png_chunk(byte, |kind, data| {
        if kind == b"IHDR" {
            if let (Some(w), Some(h)) = (be32(data, 0), be32(data, 4)) {
                out = Some((w, h));
            }
            return false;
        }
        true
    });
    out
}

/// Walk the RIFF chunks of a WebP file, calling `visit(fourcc, payload)`.
fn webp_chunk<'a>(byte: &'a [u8], mut visit: impl FnMut(&'a [u8], &'a [u8]) -> bool) {
    if at(byte, 0, 4) != Some(b"RIFF") || at(byte, 8, 4) != Some(b"WEBP") {
        return;
    }
    let mut off = 12usize;
    for _ in 0..MAX_SEGMENT {
        let Some(kind) = at(byte, off, 4) else { return };
        let Some(len) = le32(byte, off.saturating_add(4)) else { return };
        let len = len as usize;
        if len > byte.len() {
            return;
        }
        let Some(data) = at(byte, off.saturating_add(8), len) else { return };
        if !visit(kind, data) {
            return;
        }
        // RIFF pads an odd-length payload to an even boundary, and the pad byte is not counted in
        // the declared length. Forgetting it desynchronises every subsequent chunk.
        off = off.saturating_add(8).saturating_add(len).saturating_add(len & 1);
    }
}

fn webp_dimension(byte: &[u8]) -> Option<(u32, u32)> {
    let mut out = None;
    webp_chunk(byte, |kind, data| {
        match kind {
            b"VP8X" => {
                // Four flag bytes, then canvas width-1 and height-1 as 24-bit little-endian.
                if let (Some(w), Some(h)) = (at(data, 4, 3), at(data, 7, 3)) {
                    let w = u32::from(w[0]) | u32::from(w[1]) << 8 | u32::from(w[2]) << 16;
                    let h = u32::from(h[0]) | u32::from(h[1]) << 8 | u32::from(h[2]) << 16;
                    out = Some((w.saturating_add(1), h.saturating_add(1)));
                }
                // The VP8X canvas is authoritative for an extended or animated file, so stop here
                // rather than let a later sub-frame's own size overwrite it.
                false
            }
            b"VP8 " => {
                // Lossy keyframe: a 3-byte frame tag, the 3-byte start code, then 14-bit sizes.
                if at(data, 3, 3) == Some(&[0x9D, 0x01, 0x2A]) {
                    if let (Some(w), Some(h)) = (u16_at(data, 6, true), u16_at(data, 8, true)) {
                        out = Some((u32::from(w & 0x3FFF), u32::from(h & 0x3FFF)));
                    }
                }
                false
            }
            b"VP8L" => {
                // Lossless: a signature byte, then 14 bits of width-1 and 14 of height-1 packed
                // least-significant-first into a little-endian 32-bit word.
                if data.first() == Some(&0x2F) {
                    if let Some(bit) = le32(data, 1) {
                        out = Some(((bit & 0x3FFF) + 1, ((bit >> 14) & 0x3FFF) + 1));
                    }
                }
                false
            }
            _ => true,
        }
    });
    out
}

fn gif_dimension(byte: &[u8]) -> Option<(u32, u32)> {
    // The logical screen descriptor follows the six-byte signature immediately.
    let w = u16_at(byte, 6, true)?;
    let h = u16_at(byte, 8, true)?;
    Some((u32::from(w), u32::from(h)))
}

/// Dimensions from a HEIF/AVIF `ispe` (image spatial extent) property.
///
/// A bounded box walk, not a parse: it descends only through the containers that can hold `ispe`
/// (`meta` → `iprp` → `ipco`) and reads the first one it finds. A file with several items — a
/// thumbnail, an alpha plane, a burst — has several `ispe` boxes, and the first is not guaranteed
/// to belong to the primary item. Resolving `pitm`/`ipma` properly is decoder-adjacent work this
/// module declines, so the value is documented as approximate rather than being silently wrong.
fn heif_dimension(byte: &[u8]) -> Option<(u32, u32)> {
    fn walk(byte: &[u8], depth: u32, budget: &mut usize) -> Option<(u32, u32)> {
        // Depth is capped as well as box count: an attacker choosing the nesting must not get to
        // choose this parser's stack depth.
        if depth > 8 {
            return None;
        }
        let mut off = 0usize;
        while off < byte.len() {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            let declared = u64::from(be32(byte, off)?);
            let kind = at(byte, off.saturating_add(4), 4)?;
            let (head, size) = match declared {
                // 0 means "this box runs to the end of the file".
                0 => (8usize, (byte.len().saturating_sub(off)) as u64),
                // 1 means a 64-bit largesize follows the header.
                1 => {
                    let hi = u64::from(be32(byte, off.saturating_add(8))?);
                    let lo = u64::from(be32(byte, off.saturating_add(12))?);
                    (16usize, (hi << 32) | lo)
                }
                _ => (8usize, declared),
            };
            if size < head as u64 || size > byte.len() as u64 {
                return None;
            }
            let body = at(byte, off.saturating_add(head), (size as usize).saturating_sub(head))?;
            match kind {
                // `meta` is a FullBox: four bytes of version and flags precede its children.
                b"meta" => {
                    if let Some(inner) = body.get(4..) {
                        if let Some(d) = walk(inner, depth + 1, budget) {
                            return Some(d);
                        }
                    }
                }
                b"iprp" | b"ipco" => {
                    if let Some(d) = walk(body, depth + 1, budget) {
                        return Some(d);
                    }
                }
                b"ispe" => return Some((be32(body, 4)?, be32(body, 8)?)),
                _ => {}
            }
            off = off.checked_add(size as usize)?;
        }
        None
    }
    let mut budget = MAX_BOX;
    walk(byte, 0, &mut budget)
}

// ---------------------------------------------------------------------------------------------
// locating the EXIF/TIFF block inside a container
// ---------------------------------------------------------------------------------------------

/// The six bytes that introduce a TIFF header inside a JPEG APP1 segment.
const EXIF_ID: &[u8; 6] = b"Exif\0\0";

/// Find the TIFF block — the bytes from the `II`/`MM` byte-order mark onward — inside a container.
///
/// Covers the three places EXIF is actually stored:
///
///   - **JPEG**: an APP1 (`FFE1`) segment whose payload begins `Exif\0\0`. A file commonly has
///     several APP1 segments (XMP uses one too), so the identifier must be checked, not assumed.
///   - **PNG**: the `eXIf` chunk (standardised in PNG 1.5 errata, 2017), whose data is bare TIFF.
///   - **WebP**: the `EXIF` RIFF chunk. The specification says bare TIFF, but encoders in the wild
///     write the JPEG-style `Exif\0\0` prefix anyway, so both spellings are accepted.
///
/// Returns a subslice of the input — no copy, and the offsets inside it stay meaningful for
/// [`Exif::thumb_slice`], which is why the thumbnail costs nothing to extract.
pub fn exif_block(byte: &[u8]) -> Option<&[u8]> {
    match sniff(byte) {
        Format::Jpeg => {
            let mut out = None;
            jpeg_segment(byte, |marker, payload| {
                if marker == 0xE1 && at(payload, 0, 6) == Some(EXIF_ID.as_slice()) {
                    out = payload.get(6..);
                    return false;
                }
                true
            });
            out
        }
        Format::Png => {
            let mut out = None;
            png_chunk(byte, |kind, data| {
                if kind == b"eXIf" {
                    out = Some(data);
                    return false;
                }
                true
            });
            out
        }
        Format::Webp => {
            let mut out = None;
            webp_chunk(byte, |kind, data| {
                if kind == b"EXIF" {
                    out = if at(data, 0, 6) == Some(EXIF_ID.as_slice()) {
                        data.get(6..)
                    } else {
                        Some(data)
                    };
                    return false;
                }
                true
            });
            out
        }
        // GIF has no EXIF container at all. HEIF stores it as an item reached through `iloc`,
        // which needs the item-location table this module does not walk — declared unsupported
        // rather than half-supported, because a half-walked `iloc` is exactly where a parser of
        // this kind gets its offsets from an attacker.
        Format::Gif | Format::Heif | Format::Unknown => None,
    }
}

// ---------------------------------------------------------------------------------------------
// TIFF / EXIF
// ---------------------------------------------------------------------------------------------

/// A decoded coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Geo {
    pub lat: f64,
    pub lon: f64,
}

/// Decoded EXIF.
///
/// Every field is `Option` and the default is "nothing known", because that is what most files in a
/// real corpus contain — see the module documentation on stripping.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exif {
    /// Signed decimal degrees, north and east positive.
    pub geo: Option<Geo>,
    /// Metres relative to sea level, negative below it (`GPSAltitudeRef == 1`).
    pub altitude_m: Option<f64>,
    /// `DateTimeOriginal`, verbatim in EXIF's `YYYY:MM:DD HH:MM:SS` form, falling back to `DateTime`
    /// (the file-change tag) when the original is absent. Kept as written rather than normalised to
    /// an instant: an EXIF timestamp carries no zone, so converting one to UTC means inventing
    /// information. [`Exif::numeric`] exposes the year/month/day that a range query actually needs.
    pub datetime: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub lens: Option<String>,
    /// 1–8. The one tag that silently changes what an image *looks like*: a viewer that ignores it
    /// shows a sideways photo, and a perceptual hash computed without applying it will not match
    /// the same picture hashed by a viewer that did.
    pub orientation: Option<u16>,
    pub iso: Option<u32>,
    /// The f-number itself (2.8), not the APEX aperture value.
    pub f_number: Option<f64>,
    /// Exposure time in seconds: `1/250` arrives as the rational 1/250 and stays 0.004.
    pub exposure_second: Option<f64>,
    /// Focal length in millimetres as recorded — the physical length, not the 35 mm equivalent.
    pub focal_length_mm: Option<f64>,
    /// `(offset, length)` of the IFD1 thumbnail, **relative to the start of the TIFF block**.
    ///
    /// A range rather than a copy, so that parsing allocates nothing an attacker sized.
    /// [`Exif::thumb_slice`] turns it back into borrowed bytes.
    pub thumb: Option<(u32, u32)>,
}

/// Which directory an entry came from.
///
/// IFD1 describes the *thumbnail*, and carries the same tag numbers as IFD0 — so its `Make`,
/// `Model` and `Orientation` must not be allowed to overwrite the main image's. That is an easy and
/// real bug, and it is the reason this enum exists rather than one flat tag match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// IFD0 and the Exif sub-IFD: both describe the main image.
    Main,
    Gps,
    /// IFD1: the embedded thumbnail.
    Thumb,
}

/// Byte count of one component of a TIFF field type, or `None` for a type this parser does not
/// know.
///
/// An unknown type is skipped rather than guessed, because guessing its size means computing an
/// offset from a number that means something else entirely.
fn type_size(kind: u16) -> Option<usize> {
    Some(match kind {
        1 | 2 | 6 | 7 => 1, // BYTE, ASCII, SBYTE, UNDEFINED
        3 | 8 => 2,         // SHORT, SSHORT
        4 | 9 | 11 => 4,    // LONG, SLONG, FLOAT
        5 | 10 | 12 => 8,   // RATIONAL, SRATIONAL, DOUBLE
        _ => return None,
    })
}

/// A field's payload, already bounds-resolved.
///
/// Constructing one proves the bytes exist, so the accessors below cannot fail for any
/// out-of-range reason an attacker controls — only because a type or index does not fit the field.
struct Value<'a> {
    kind: u16,
    count: u32,
    data: &'a [u8],
    le: bool,
}

impl Value<'_> {
    /// An ASCII field as a `String`, NUL-trimmed.
    ///
    /// Length is capped at [`MAX_TEXT_BYTE`] *before* any copy, since the count came from the file.
    /// Bytes are decoded lossily because camera makers write Shift-JIS, Latin-1 and outright binary
    /// into nominally-ASCII tags; a mojibake `Make` is still a usable facet, whereas a rejected one
    /// is not.
    fn text(&self) -> Option<String> {
        if self.kind != 2 && self.kind != 7 {
            return None;
        }
        let end = self.data.iter().position(|&b| b == 0).unwrap_or(self.data.len());
        let raw = self.data.get(..end.min(MAX_TEXT_BYTE))?;
        let s = String::from_utf8_lossy(raw).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    /// Component `i` as an unsigned integer, for the BYTE/SHORT/LONG family.
    fn uint(&self, i: usize) -> Option<u64> {
        if i as u64 >= u64::from(self.count) {
            return None;
        }
        let unit = type_size(self.kind)?;
        let off = i.checked_mul(unit)?;
        Some(match self.kind {
            1 | 6 | 7 => u64::from(*self.data.get(off)?),
            3 | 8 => u64::from(u16_at(self.data, off, self.le)?),
            4 | 9 => u64::from(u32_at(self.data, off, self.le)?),
            _ => return None,
        })
    }

    /// Component `i` as a real number, covering the integer, rational and float types.
    ///
    /// A zero denominator yields `None` rather than an infinity: EXIF writers use `0/0` to mean
    /// "not recorded", and letting a NaN or an infinity into a numeric column would poison every
    /// range query over that column for the whole corpus.
    fn real(&self, i: usize) -> Option<f64> {
        if i as u64 >= u64::from(self.count) {
            return None;
        }
        let unit = type_size(self.kind)?;
        let off = i.checked_mul(unit)?;
        match self.kind {
            5 | 10 => {
                let n = u32_at(self.data, off, self.le)?;
                let d = u32_at(self.data, off.checked_add(4)?, self.le)?;
                if d == 0 {
                    return None;
                }
                if self.kind == 5 {
                    Some(f64::from(n) / f64::from(d))
                } else {
                    Some(f64::from(n as i32) / f64::from(d as i32))
                }
            }
            11 => {
                let b = at(self.data, off, 4)?;
                let a = [b[0], b[1], b[2], b[3]];
                let bit = if self.le { u32::from_le_bytes(a) } else { u32::from_be_bytes(a) };
                let v = f64::from(f32::from_bits(bit));
                v.is_finite().then_some(v)
            }
            12 => {
                let b = at(self.data, off, 8)?;
                let mut a = [0u8; 8];
                a.copy_from_slice(b);
                let bit = if self.le { u64::from_le_bytes(a) } else { u64::from_be_bytes(a) };
                let v = f64::from_bits(bit);
                v.is_finite().then_some(v)
            }
            _ => self.uint(i).map(|v| v as f64),
        }
    }
}

/// The half-built GPS reading, carried across the tags of the GPS IFD.
///
/// The hemisphere refs are separate tags from the magnitudes, so both orders of arrival have to
/// work: ascending tag order does put the ref first, but relying on that is relying on the file
/// being well-formed, which is precisely the assumption this module refuses to make.
#[derive(Default, Clone, Copy)]
struct GpsState {
    lat: Option<f64>,
    lat_south: bool,
    lon: Option<f64>,
    lon_west: bool,
    alt: Option<f64>,
    alt_below: bool,
}

/// Degrees-minutes-seconds as three rationals → decimal degrees.
///
/// Writers use every representation the format allows: `52/1 12/1 30/1`, or `52/1 1230/100 0/1`
/// with the seconds folded into the minutes, or the whole angle in the degree slot as
/// `521234/10000`. Summing with the 1/60 and 1/3600 weights handles all three, and a missing minute
/// or second counts as zero rather than as a failure.
fn dms(v: &Value) -> Option<f64> {
    let d = v.real(0)?;
    let deg = d + v.real(1).unwrap_or(0.0) / 60.0 + v.real(2).unwrap_or(0.0) / 3600.0;
    deg.is_finite().then_some(deg)
}

fn gps_tag(exif: &mut Exif, tag: u16, v: &Value, state: &mut GpsState) {
    match tag {
        1 => state.lat_south = v.text().is_some_and(|s| s.eq_ignore_ascii_case("S")),
        2 => state.lat = dms(v),
        3 => state.lon_west = v.text().is_some_and(|s| s.eq_ignore_ascii_case("W")),
        4 => state.lon = dms(v),
        // GPSAltitudeRef is a BYTE and not ASCII: 0 is above sea level, 1 is below.
        5 => state.alt_below = v.uint(0) == Some(1),
        6 => state.alt = v.real(0),
        _ => return,
    }
    // The sign convention, which is the only part of GPS EXIF that is easy to get backwards: the
    // stored magnitude is always positive and the ref tag carries the hemisphere. South is negative
    // latitude, West is negative longitude — matching every mapping API, and matching `index-geo`,
    // which takes `(lon, lat)` in signed degrees.
    if let (Some(lat), Some(lon)) = (state.lat, state.lon) {
        let lat = if state.lat_south { -lat } else { lat };
        let lon = if state.lon_west { -lon } else { lon };
        // Out-of-range coordinates are dropped rather than clamped. They are either corruption or a
        // deliberate probe, and one "latitude" of 900 degrees ruins every range query on the column.
        if (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon) {
            exif.geo = Some(Geo { lat, lon });
        }
    }
    if let Some(alt) = state.alt {
        exif.altitude_m = Some(if state.alt_below { -alt } else { alt });
    }
}

/// Parse a TIFF block — the bytes from the `II`/`MM` mark onward — into an [`Exif`].
///
/// Returns `None` only when there is no readable TIFF header at all. A well-formed header carrying
/// no recognised tag returns an empty `Exif`, because the distinction between "unreadable" and
/// "read, and empty" is the one an OSINT indexer actually needs.
pub fn parse_tiff(tiff: &[u8]) -> Option<Exif> {
    let le = match at(tiff, 0, 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    // 42, "an arbitrary but carefully chosen number" (TIFF 6.0 §2). Checking it is what stops a
    // buffer that merely happens to begin `II` from being walked as a directory.
    if u16_at(tiff, 2, le)? != 42 {
        return None;
    }
    let first = u32_at(tiff, 4, le)? as usize;

    let mut exif = Exif::default();
    let mut gps = GpsState::default();
    let mut visited: BTreeSet<usize> = BTreeSet::new();
    // A worklist rather than recursion: the pointer graph is the attacker's, so it must not be
    // allowed to be this parser's call stack.
    let mut work: Vec<(usize, Kind)> = vec![(first, Kind::Main)];
    let mut budget = MAX_IFD;

    while let Some((off, kind)) = work.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        // The cycle guard. `A -> A` and `A -> B -> A` are both one-line files to craft, and both
        // are infinite loops without this line.
        if !visited.insert(off) {
            continue;
        }
        let Some(claimed) = u16_at(tiff, off, le) else { continue };
        let claimed = claimed as usize;
        for i in 0..claimed.min(MAX_ENTRY) {
            let base = off.saturating_add(2).saturating_add(i.saturating_mul(12));
            let (Some(tag), Some(ftype), Some(fcount)) = (
                u16_at(tiff, base, le),
                u16_at(tiff, base.saturating_add(2), le),
                u32_at(tiff, base.saturating_add(4), le),
            ) else {
                break;
            };
            let Some(unit) = type_size(ftype) else { continue };
            // `count * unit` in u64: a claimed count of 4 294 967 295 must not wrap a usize.
            let total = u64::from(fcount).saturating_mul(unit as u64);
            let data = if total <= 4 {
                // Small values live in the offset field itself.
                at(tiff, base.saturating_add(8), total as usize)
            } else if total <= tiff.len() as u64 {
                // Only now, knowing the span could fit the buffer at all, is the pointer worth
                // reading. Either way nothing is allocated.
                u32_at(tiff, base.saturating_add(8), le)
                    .and_then(|p| at(tiff, p as usize, total as usize))
            } else {
                None
            };
            let Some(data) = data else { continue };
            let v = Value { kind: ftype, count: fcount, data, le };

            match (kind, tag) {
                // Sub-directory pointers. Pushed, not followed: the cycle guard lives on the pop.
                (Kind::Main, 0x8769) => {
                    if let Some(p) = v.uint(0) {
                        work.push((p as usize, Kind::Main));
                    }
                }
                (Kind::Main, 0x8825) => {
                    if let Some(p) = v.uint(0) {
                        work.push((p as usize, Kind::Gps));
                    }
                }
                (Kind::Main, 0x010F) => {
                    if exif.make.is_none() {
                        exif.make = v.text();
                    }
                }
                (Kind::Main, 0x0110) => {
                    if exif.model.is_none() {
                        exif.model = v.text();
                    }
                }
                (Kind::Main, 0xA434) => {
                    if exif.lens.is_none() {
                        exif.lens = v.text();
                    }
                }
                (Kind::Main, 0x0112) => {
                    if exif.orientation.is_none() {
                        // Values outside 1–8 are not orientations; a viewer given one would either
                        // ignore it or rotate arbitrarily, so it is not worth indexing.
                        exif.orientation = match v.uint(0) {
                            Some(o) if (1..=8).contains(&o) => Some(o as u16),
                            _ => None,
                        };
                    }
                }
                // DateTimeOriginal outranks DateTime unconditionally: the first is when the shutter
                // fired, the second is when the file was last written, and an editor updates it.
                (Kind::Main, 0x9003) => {
                    if let Some(t) = v.text() {
                        exif.datetime = Some(t);
                    }
                }
                (Kind::Main, 0x0132) => {
                    if exif.datetime.is_none() {
                        exif.datetime = v.text();
                    }
                }
                (Kind::Main, 0x8827) | (Kind::Main, 0x8833) => {
                    if exif.iso.is_none() {
                        exif.iso = v.uint(0).map(|x| x.min(u64::from(u32::MAX)) as u32);
                    }
                }
                (Kind::Main, 0x829D) => {
                    if exif.f_number.is_none() {
                        exif.f_number = v.real(0);
                    }
                }
                (Kind::Main, 0x829A) => {
                    if exif.exposure_second.is_none() {
                        exif.exposure_second = v.real(0);
                    }
                }
                (Kind::Main, 0x920A) => {
                    if exif.focal_length_mm.is_none() {
                        exif.focal_length_mm = v.real(0);
                    }
                }
                (Kind::Gps, 1..=6) => gps_tag(&mut exif, tag, &v, &mut gps),

                // IFD1 describes the thumbnail and contributes exactly two facts.
                (Kind::Thumb, 0x0201) => {
                    if let Some(p) = v.uint(0) {
                        let len = exif.thumb.map(|t| t.1).unwrap_or(0);
                        exif.thumb = Some((p.min(u64::from(u32::MAX)) as u32, len));
                    }
                }
                (Kind::Thumb, 0x0202) => {
                    if let Some(n) = v.uint(0) {
                        let off = exif.thumb.map(|t| t.0).unwrap_or(0);
                        exif.thumb = Some((off, n.min(u64::from(u32::MAX)) as u32));
                    }
                }
                _ => {}
            }
        }
        // The next-IFD pointer, computed from the CLAIMED entry count rather than the capped one,
        // so that an over-long directory produces a failed read instead of a wrong offset. Only
        // IFD0 chains: its successor is IFD1, the thumbnail. Following a sub-IFD's chain is how a
        // crafted file gets a parser to read main-image tags as thumbnail tags, or the reverse.
        if kind == Kind::Main {
            let end = off.saturating_add(2).saturating_add(claimed.saturating_mul(12));
            if let Some(next) = u32_at(tiff, end, le) {
                if next != 0 {
                    work.push((next as usize, Kind::Thumb));
                }
            }
        }
    }

    // A thumbnail range that does not fit the block is a lie. Drop it rather than hand out a length
    // the caller might trust and a slice the caller might not re-check.
    if let Some((o, n)) = exif.thumb {
        if n == 0 || at(tiff, o as usize, n as usize).is_none() {
            exif.thumb = None;
        }
    }
    Some(exif)
}

/// Locate and parse the EXIF of a whole image file.
///
/// `None` means *no EXIF block was found* — the normal case for anything off a social platform.
/// `Some(Exif::default())` means *a block was found and it was empty*.
pub fn parse(byte: &[u8]) -> Option<Exif> {
    parse_tiff(exif_block(byte)?)
}

/// The embedded thumbnail bytes of a whole image file, if it has one.
///
/// A borrowed slice of the input: no copy, no decode, and no allocation proportional to anything
/// the file claimed. A caller gets a ready-to-display JPEG for the price of a header walk — which
/// is the entire argument for extracting it here rather than resizing the main image later.
pub fn embedded_thumbnail(byte: &[u8]) -> Option<&[u8]> {
    let block = exif_block(byte)?;
    parse_tiff(block)?.thumb_slice(block)
}

impl Exif {
    /// True when nothing at all was recovered — the usual outcome, not a failure.
    pub fn is_empty(&self) -> bool {
        *self == Exif::default()
    }

    /// Resolve [`Exif::thumb`] against the TIFF block it was parsed from.
    ///
    /// The range is re-checked here rather than trusted, so that passing the wrong buffer returns
    /// `None` instead of a slice of unrelated bytes.
    pub fn thumb_slice<'a>(&self, tiff: &'a [u8]) -> Option<&'a [u8]> {
        let (off, len) = self.thumb?;
        at(tiff, off as usize, len as usize)
    }

    /// `(year, month, day)` from the EXIF `YYYY:MM:DD HH:MM:SS` form, when it is well-formed.
    ///
    /// The lower year bound is 1826 — the oldest surviving photograph — so that `0000:00:00`, which
    /// is what a camera with a dead coin cell writes, cannot enter a numeric column as year zero
    /// and drag the bottom of every date range query with it.
    fn date_part(&self) -> Option<(u32, u32, u32)> {
        let s = self.datetime.as_deref()?;
        let mut it = s.get(..10)?.split([':', '-', '/']);
        let y: u32 = it.next()?.trim().parse().ok()?;
        let m: u32 = it.next()?.trim().parse().ok()?;
        let d: u32 = it.next()?.trim().parse().ok()?;
        if !(1826..=2200).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        Some((y, m, d))
    }

    /// Facet terms as `(field, value)` pairs, ready for `index-text`.
    ///
    /// Field names are dotted and singular, and a field is simply absent when unknown — the same
    /// contract the text tier already has for a document that lacks a field, so a stripped image
    /// needs no special case anywhere downstream.
    pub fn term(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut put = |k: &str, v: &str| {
            let v = v.trim();
            if !v.is_empty() {
                out.push((k.to_string(), v.to_string()));
            }
        };
        if let Some(v) = &self.make {
            put("exif.make", v);
        }
        if let Some(v) = &self.model {
            put("exif.model", v);
        }
        if let (Some(a), Some(b)) = (&self.make, &self.model) {
            // Makers write "NIKON CORPORATION" as Make and "NIKON D750" as Model; joining blindly
            // produces "NIKON CORPORATION NIKON D750", a term nobody would ever type. The brand is
            // the maker's FIRST word — the rest is "CORPORATION", "IMAGING COMPANY, LTD." and other
            // registrar noise — so when the model already starts with the brand, the model alone IS
            // the camera. Apple/"iPhone 15" shares no prefix and is joined, which is how the
            // crate's headline query (`camera = "iPhone 15"`) still matches through `exif.model`.
            let brand = a.split_whitespace().next().unwrap_or_default().to_lowercase();
            let camera = if !brand.is_empty() && b.to_lowercase().starts_with(&brand) {
                b.clone()
            } else {
                format!("{a} {b}")
            };
            put("exif.camera", &camera);
        }
        if let Some(v) = &self.lens {
            put("exif.lens", v);
        }
        if let Some((y, m, d)) = self.date_part() {
            put("exif.date", &format!("{y:04}-{m:02}-{d:02}"));
        }
        out
    }

    /// Numeric columns as `(field, value)` pairs, for range queries.
    ///
    /// `exif.year` is emitted as its own column even though it is derivable from `exif.date`,
    /// because `2019 <= year < 2021` is the query in this crate's headline example and a numeric
    /// range answers it in one comparison where a term-prefix scan would enumerate days.
    pub fn numeric(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        let mut put = |k: &str, v: f64| {
            // Non-finite values never enter a column: one NaN makes every comparison over it false
            // and silently removes the document from both sides of a range.
            if v.is_finite() {
                out.push((k.to_string(), v));
            }
        };
        if let Some(g) = self.geo {
            put("exif.lat", g.lat);
            put("exif.lon", g.lon);
        }
        if let Some(a) = self.altitude_m {
            put("exif.altitude", a);
        }
        if let Some((y, m, d)) = self.date_part() {
            put("exif.year", f64::from(y));
            put("exif.month", f64::from(m));
            put("exif.day", f64::from(d));
        }
        if let Some(o) = self.orientation {
            put("exif.orientation", f64::from(o));
        }
        if let Some(i) = self.iso {
            put("exif.iso", f64::from(i));
        }
        if let Some(f) = self.f_number {
            put("exif.fnumber", f);
        }
        if let Some(e) = self.exposure_second {
            put("exif.exposure", e);
        }
        if let Some(f) = self.focal_length_mm {
            put("exif.focal", f);
        }
        out
    }
}

// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod test {
    use super::*;

    // Every fixture below is built byte by byte in this file. Nothing is read from disk, so the
    // suite is hermetic and the exact bytes under test are visible next to the assertion.

    fn push16(v: &mut Vec<u8>, x: u16, le: bool) {
        if le {
            v.extend_from_slice(&x.to_le_bytes());
        } else {
            v.extend_from_slice(&x.to_be_bytes());
        }
    }

    fn push32(v: &mut Vec<u8>, x: u32, le: bool) {
        if le {
            v.extend_from_slice(&x.to_le_bytes());
        } else {
            v.extend_from_slice(&x.to_be_bytes());
        }
    }

    /// What an entry's value field holds once offsets are known.
    enum D {
        Byte(Vec<u8>),
        /// The offset of another IFD in the same file, by index.
        Ifd(usize),
        /// The offset of the trailer appended after everything else — the thumbnail bytes.
        Trailer,
    }

    struct E {
        tag: u16,
        kind: u16,
        count: u32,
        data: D,
    }

    struct Dir {
        entry: Vec<E>,
        next: Option<usize>,
    }

    fn ascii(tag: u16, s: &str) -> E {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        E { tag, kind: 2, count: b.len() as u32, data: D::Byte(b) }
    }

    fn short(tag: u16, x: u16, le: bool) -> E {
        let mut b = Vec::new();
        push16(&mut b, x, le);
        E { tag, kind: 3, count: 1, data: D::Byte(b) }
    }

    fn long(tag: u16, x: u32, le: bool) -> E {
        let mut b = Vec::new();
        push32(&mut b, x, le);
        E { tag, kind: 4, count: 1, data: D::Byte(b) }
    }

    fn byte_tag(tag: u16, x: u8) -> E {
        E { tag, kind: 1, count: 1, data: D::Byte(vec![x]) }
    }

    fn rational(tag: u16, r: &[(u32, u32)], le: bool) -> E {
        let mut b = Vec::new();
        for &(n, d) in r {
            push32(&mut b, n, le);
            push32(&mut b, d, le);
        }
        E { tag, kind: 5, count: r.len() as u32, data: D::Byte(b) }
    }

    fn ifd_ptr(tag: u16, i: usize) -> E {
        E { tag, kind: 4, count: 1, data: D::Ifd(i) }
    }

    /// Lay out a TIFF block: header, then the directories in order, then the heap, then a trailer.
    ///
    /// The builder applies the real spec rule that a value of four bytes or fewer is stored INLINE
    /// in the offset field, so the fixtures exercise both the inline and the pointer path (`"N\0"`
    /// is inline, a three-rational latitude is not).
    fn build_tiff(le: bool, dir: &[Dir], trailer: &[u8]) -> Vec<u8> {
        let mut off = Vec::new();
        let mut cur = 8usize;
        for d in dir {
            off.push(cur);
            cur += 2 + 12 * d.entry.len() + 4;
        }
        let heap_at = cur;

        let mut body = Vec::new();
        let mut heap = Vec::new();
        // The trailer offset is only knowable after the heap is built, so directories that need it
        // are patched afterwards; here we record where to patch.
        let mut trailer_patch = Vec::new();
        for d in dir {
            push16(&mut body, d.entry.len() as u16, le);
            for e in &d.entry {
                push16(&mut body, e.tag, le);
                push16(&mut body, e.kind, le);
                push32(&mut body, e.count, le);
                match &e.data {
                    D::Ifd(i) => push32(&mut body, off[*i] as u32, le),
                    D::Trailer => {
                        trailer_patch.push(body.len());
                        push32(&mut body, 0, le);
                    }
                    D::Byte(b) if b.len() <= 4 => {
                        let mut pad = b.clone();
                        pad.resize(4, 0);
                        body.extend_from_slice(&pad);
                    }
                    D::Byte(b) => {
                        push32(&mut body, (heap_at + heap.len()) as u32, le);
                        heap.extend_from_slice(b);
                        if heap.len() % 2 == 1 {
                            heap.push(0);
                        }
                    }
                }
            }
            push32(&mut body, d.next.map(|i| off[i] as u32).unwrap_or(0), le);
        }

        let trailer_at = (heap_at + heap.len()) as u32;
        for p in trailer_patch {
            let mut b = Vec::new();
            push32(&mut b, trailer_at, le);
            body[p..p + 4].copy_from_slice(&b);
        }

        let mut out = Vec::new();
        out.extend_from_slice(if le { b"II" } else { b"MM" });
        push16(&mut out, 42, le);
        push32(&mut out, 8, le);
        out.extend_from_slice(&body);
        out.extend_from_slice(&heap);
        out.extend_from_slice(trailer);
        out
    }

    const THUMB: &[u8] = b"\xFF\xD8\xFF\xD9thumbnail-bytes";

    /// A complete, realistic EXIF block: IFD0 → IFD1, plus the Exif and GPS sub-IFDs.
    fn sample_tiff(le: bool, lat_ref: &str, lon_ref: &str) -> Vec<u8> {
        let dir = vec![
            // 0: IFD0
            Dir {
                entry: vec![
                    ascii(0x010F, "Apple"),
                    ascii(0x0110, "iPhone 15"),
                    short(0x0112, 6, le),
                    ascii(0x0132, "2001:01:01 00:00:00"),
                    ifd_ptr(0x8769, 2),
                    ifd_ptr(0x8825, 3),
                ],
                next: Some(1),
            },
            // 1: IFD1, the thumbnail directory
            Dir {
                entry: vec![
                    E { tag: 0x0201, kind: 4, count: 1, data: D::Trailer },
                    long(0x0202, THUMB.len() as u32, le),
                    // Present on purpose: IFD1's own orientation must NOT win over IFD0's.
                    short(0x0112, 1, le),
                ],
                next: None,
            },
            // 2: the Exif sub-IFD
            Dir {
                entry: vec![
                    ascii(0x9003, "2019:06:12 10:11:12"),
                    rational(0x829D, &[(28, 10)], le),
                    rational(0x829A, &[(1, 250)], le),
                    rational(0x920A, &[(23, 1)], le),
                    short(0x8827, 400, le),
                    ascii(0xA434, "Wide Camera"),
                ],
                next: None,
            },
            // 3: the GPS sub-IFD
            Dir {
                entry: vec![
                    ascii(1, lat_ref),
                    rational(2, &[(52, 1), (12, 1), (30, 1)], le),
                    ascii(3, lon_ref),
                    rational(4, &[(4, 1), (53, 1), (0, 1)], le),
                    byte_tag(5, 1),
                    rational(6, &[(1234, 100)], le),
                ],
                next: None,
            },
        ];
        build_tiff(le, &dir, THUMB)
    }

    fn seg(out: &mut Vec<u8>, marker: u8, payload: &[u8]) {
        out.push(0xFF);
        out.push(marker);
        push16(out, (payload.len() + 2) as u16, false);
        out.extend_from_slice(payload);
    }

    fn qtable_payload(id: u8, fill: u8) -> Vec<u8> {
        let mut p = vec![id];
        for i in 0..64u8 {
            p.push(fill.wrapping_add(i));
        }
        p
    }

    fn sample_jpeg(tiff: Option<&[u8]>) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        if let Some(t) = tiff {
            let mut app1 = EXIF_ID.to_vec();
            app1.extend_from_slice(t);
            seg(&mut out, 0xE1, &app1);
        }
        seg(&mut out, 0xDB, &qtable_payload(0, 3));
        seg(&mut out, 0xDB, &qtable_payload(1, 9));
        // SOF0: precision 8, height 480, width 640, three components.
        let mut sof = vec![8u8];
        push16(&mut sof, 480, false);
        push16(&mut sof, 640, false);
        sof.push(3);
        sof.extend_from_slice(&[1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        seg(&mut out, 0xC0, &sof);
        seg(&mut out, 0xDA, &[3, 1, 0, 2, 0x11, 3, 0x11, 0, 63, 0]);
        out.extend_from_slice(&[0x12, 0x34, 0x56]); // entropy-coded stand-in
        out.extend_from_slice(&[0xFF, 0xD9]);
        out
    }

    fn png_chunk_out(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        push32(out, data.len() as u32, false);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        // The CRC is written as zero: this parser deliberately does not verify it, and a fixture
        // that had to compute one would be testing the fixture builder rather than the parser.
        push32(out, 0, false);
    }

    fn sample_png(tiff: Option<&[u8]>) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut ihdr = Vec::new();
        push32(&mut ihdr, 1024, false);
        push32(&mut ihdr, 768, false);
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        png_chunk_out(&mut out, b"IHDR", &ihdr);
        if let Some(t) = tiff {
            png_chunk_out(&mut out, b"eXIf", t);
        }
        png_chunk_out(&mut out, b"IDAT", &[0, 1, 2, 3, 4]);
        png_chunk_out(&mut out, b"IEND", &[]);
        out
    }

    fn riff_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(kind);
        push32(out, data.len() as u32, true);
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
    }

    fn sample_webp(tiff: Option<&[u8]>) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        let mut vp8x = vec![0x08, 0, 0, 0];
        // Canvas 300 x 200, stored as width-1 and height-1 in 24-bit little-endian.
        for v in [299u32, 199u32] {
            vp8x.push((v & 0xFF) as u8);
            vp8x.push(((v >> 8) & 0xFF) as u8);
            vp8x.push(((v >> 16) & 0xFF) as u8);
        }
        riff_chunk(&mut body, b"VP8X", &vp8x);
        // An odd-length chunk on purpose: the RIFF pad byte is a classic desynchronisation bug.
        riff_chunk(&mut body, b"ICCP", &[1, 2, 3]);
        if let Some(t) = tiff {
            riff_chunk(&mut body, b"EXIF", t);
        }
        let mut out = b"RIFF".to_vec();
        push32(&mut out, body.len() as u32, true);
        out.extend_from_slice(&body);
        out
    }

    fn sample_gif() -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        push16(&mut out, 64, true);
        push16(&mut out, 48, true);
        out.extend_from_slice(&[0xF7, 0, 0]);
        out
    }

    fn sample_heif() -> Vec<u8> {
        fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
            let mut v = Vec::new();
            push32(&mut v, (body.len() + 8) as u32, false);
            v.extend_from_slice(kind);
            v.extend_from_slice(body);
            v
        }
        let mut ispe = vec![0u8, 0, 0, 0];
        push32(&mut ispe, 4032, false);
        push32(&mut ispe, 3024, false);
        let ipco = boxed(b"ipco", &boxed(b"ispe", &ispe));
        let iprp = boxed(b"iprp", &ipco);
        let mut meta_body = vec![0u8, 0, 0, 0]; // FullBox version + flags
        meta_body.extend_from_slice(&iprp);
        let meta = boxed(b"meta", &meta_body);

        let mut out = boxed(b"ftyp", b"heic\0\0\0\0mif1heic");
        out.extend_from_slice(&meta);
        out
    }

    // -----------------------------------------------------------------------------------------
    // format and dimension
    // -----------------------------------------------------------------------------------------

    #[test]
    fn each_container_is_identified_by_its_magic() {
        assert_eq!(sniff(&sample_jpeg(None)), Format::Jpeg);
        assert_eq!(sniff(&sample_png(None)), Format::Png);
        assert_eq!(sniff(&sample_webp(None)), Format::Webp);
        assert_eq!(sniff(&sample_gif()), Format::Gif);
        assert_eq!(sniff(&sample_heif()), Format::Heif);
        assert_eq!(sniff(b""), Format::Unknown);
        assert_eq!(sniff(b"<!DOCTYPE html><html>error</html>"), Format::Unknown);
        // `ftyp` with an unrecognised brand is an MP4, not an image.
        let mut mp4 = vec![0, 0, 0, 0x18];
        mp4.extend_from_slice(b"ftypisom");
        assert_eq!(sniff(&mp4), Format::Unknown);
    }

    #[test]
    fn dimension_comes_out_of_every_container() {
        let p = probe(&sample_jpeg(None));
        assert_eq!((p.format, p.width, p.height), (Format::Jpeg, Some(640), Some(480)));
        let p = probe(&sample_png(None));
        assert_eq!((p.width, p.height), (Some(1024), Some(768)));
        let p = probe(&sample_webp(None));
        assert_eq!((p.width, p.height), (Some(300), Some(200)));
        let p = probe(&sample_gif());
        assert_eq!((p.width, p.height), (Some(64), Some(48)));
        let p = probe(&sample_heif());
        assert_eq!((p.width, p.height), (Some(4032), Some(3024)));
    }

    #[test]
    fn webp_lossy_and_lossless_frame_headers_both_decode() {
        let mut body = b"WEBP".to_vec();
        let mut vp8 = vec![0x30, 0x01, 0x00, 0x9D, 0x01, 0x2A];
        push16(&mut vp8, 320, true);
        push16(&mut vp8, 240, true);
        riff_chunk(&mut body, b"VP8 ", &vp8);
        let mut out = b"RIFF".to_vec();
        push32(&mut out, body.len() as u32, true);
        out.extend_from_slice(&body);
        assert_eq!(webp_dimension(&out), Some((320, 240)));

        let mut body = b"WEBP".to_vec();
        let mut vp8l = vec![0x2F];
        // width-1 = 99 in the low 14 bits, height-1 = 49 in the next 14.
        push32(&mut vp8l, 99u32 | (49u32 << 14), true);
        riff_chunk(&mut body, b"VP8L", &vp8l);
        let mut out = b"RIFF".to_vec();
        push32(&mut out, body.len() as u32, true);
        out.extend_from_slice(&body);
        assert_eq!(webp_dimension(&out), Some((100, 50)));
    }

    #[test]
    fn a_huffman_table_is_not_mistaken_for_a_frame_header() {
        // FFC4 (DHT) sits in the SOFn numeric range and is the classic false positive.
        let mut out = vec![0xFF, 0xD8];
        seg(&mut out, 0xC4, &[0x00, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3]);
        let mut sof = vec![8u8];
        push16(&mut sof, 100, false);
        push16(&mut sof, 200, false);
        sof.push(1);
        sof.extend_from_slice(&[1, 0x11, 0]);
        seg(&mut out, 0xC2, &sof); // progressive SOF2 is still a frame header
        assert_eq!(jpeg_dimension(&out), Some((200, 100)));
    }

    // -----------------------------------------------------------------------------------------
    // EXIF
    // -----------------------------------------------------------------------------------------

    #[test]
    fn every_tag_round_trips_in_both_byte_order() {
        for le in [true, false] {
            let tiff = sample_tiff(le, "N", "E");
            let Some(e) = parse_tiff(&tiff) else { panic!("byte order le={le} rejected") };
            assert_eq!(e.make.as_deref(), Some("Apple"), "le={le}");
            assert_eq!(e.model.as_deref(), Some("iPhone 15"));
            assert_eq!(e.lens.as_deref(), Some("Wide Camera"));
            // DateTimeOriginal from the Exif IFD must beat DateTime from IFD0.
            assert_eq!(e.datetime.as_deref(), Some("2019:06:12 10:11:12"));
            // IFD0's orientation 6, not IFD1's thumbnail orientation 1.
            assert_eq!(e.orientation, Some(6));
            assert_eq!(e.iso, Some(400));
            assert_eq!(e.f_number, Some(2.8));
            assert_eq!(e.exposure_second, Some(1.0 / 250.0));
            assert_eq!(e.focal_length_mm, Some(23.0));
            assert!(!e.is_empty());
        }
    }

    #[test]
    fn gps_sign_follows_the_hemisphere_reference() {
        // 52° 12' 30" and 4° 53' 00", the magnitudes stored positive in every case.
        let lat = 52.0 + 12.0 / 60.0 + 30.0 / 3600.0;
        let lon = 4.0 + 53.0 / 60.0;
        let case = [
            ("N", "E", lat, lon),
            ("S", "E", -lat, lon),
            ("N", "W", lat, -lon),
            ("S", "W", -lat, -lon),
            // Lowercase refs occur; hemisphere is a letter, not a case-sensitive token.
            ("s", "w", -lat, -lon),
        ];
        for (lat_ref, lon_ref, want_lat, want_lon) in case {
            for le in [true, false] {
                let tiff = sample_tiff(le, lat_ref, lon_ref);
                let Some(e) = parse_tiff(&tiff) else { panic!("rejected {lat_ref}{lon_ref}") };
                let Some(g) = e.geo else { panic!("no gps for {lat_ref}{lon_ref} le={le}") };
                assert!(
                    (g.lat - want_lat).abs() < 1e-9 && (g.lon - want_lon).abs() < 1e-9,
                    "{lat_ref}/{lon_ref} le={le}: got {:?}, want ({want_lat}, {want_lon})",
                    g
                );
                // GPSAltitudeRef 1 means BELOW sea level, so 12.34 m becomes -12.34.
                let Some(alt) = e.altitude_m else { panic!("no altitude") };
                assert!((alt + 12.34).abs() < 1e-9, "altitude sign: {alt}");
            }
        }
    }

    #[test]
    fn an_out_of_range_coordinate_is_dropped_rather_than_indexed() {
        let dir = vec![
            Dir { entry: vec![ifd_ptr(0x8825, 1)], next: None },
            Dir {
                entry: vec![
                    ascii(1, "N"),
                    rational(2, &[(900, 1), (0, 1), (0, 1)], true),
                    ascii(3, "E"),
                    rational(4, &[(4, 1), (0, 1), (0, 1)], true),
                ],
                next: None,
            },
        ];
        let tiff = build_tiff(true, &dir, &[]);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.geo, None);
    }

    #[test]
    fn exif_is_found_in_jpeg_png_and_webp_alike() {
        let tiff = sample_tiff(true, "N", "E");
        for (name, file) in [
            ("jpeg", sample_jpeg(Some(&tiff))),
            ("png", sample_png(Some(&tiff))),
            ("webp", sample_webp(Some(&tiff))),
        ] {
            let Some(e) = parse(&file) else { panic!("{name}: no exif block found") };
            assert_eq!(e.model.as_deref(), Some("iPhone 15"), "{name}");
        }
        // WebP written with the JPEG-style `Exif\0\0` prefix, as several encoders do.
        let mut prefixed = EXIF_ID.to_vec();
        prefixed.extend_from_slice(&tiff);
        let file = sample_webp(Some(&prefixed));
        let Some(e) = parse(&file) else { panic!("prefixed webp exif not found") };
        assert_eq!(e.model.as_deref(), Some("iPhone 15"));
        // GIF has no EXIF container, and asking must be a `None` rather than a wrong answer.
        assert_eq!(parse(&sample_gif()), None);
    }

    #[test]
    fn the_thumbnail_is_a_slice_of_the_input_not_a_copy() {
        let tiff = sample_tiff(true, "N", "E");
        let file = sample_jpeg(Some(&tiff));
        let Some(t) = embedded_thumbnail(&file) else { panic!("no thumbnail") };
        assert_eq!(t, THUMB);
        // Borrowed from the caller's buffer: the slice lives inside `file`, it was not allocated.
        let base = file.as_ptr() as usize;
        let got = t.as_ptr() as usize;
        assert!(got >= base && got < base + file.len(), "thumbnail was copied, not borrowed");
        // A thumbnail range must not be trusted against the wrong buffer.
        let Some(e) = parse_tiff(&tiff) else { panic!("parse") };
        assert_eq!(e.thumb_slice(&[0u8; 4]), None);
    }

    #[test]
    fn a_thumbnail_range_past_the_block_is_refused() {
        let dir = vec![
            Dir { entry: vec![ascii(0x010F, "Apple")], next: Some(1) },
            Dir {
                entry: vec![long(0x0201, 0xFFFF_0000, true), long(0x0202, 4096, true)],
                next: None,
            },
        ];
        let tiff = build_tiff(true, &dir, &[]);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.thumb, None);
    }

    // -----------------------------------------------------------------------------------------
    // quantisation tables
    // -----------------------------------------------------------------------------------------

    #[test]
    fn quantisation_tables_are_extracted_and_fingerprinted() {
        let file = sample_jpeg(None);
        let t = jpeg_qtable(&file);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].id, 0);
        assert_eq!(t[0].precision, 0);
        assert_eq!(t[0].value.len(), 64);
        assert_eq!(t[0].value[0], 3);
        assert_eq!(t[1].id, 1);
        assert_eq!(t[1].value[0], 9);

        // Deterministic, and sensitive to a single coefficient — the whole point of using it as a
        // narrowing signal is that a different encoder gives a different class.
        assert_eq!(qtable_fingerprint(&t), qtable_fingerprint(&jpeg_qtable(&file)));
        let mut other = t.clone();
        other[0].value[17] += 1;
        assert_ne!(qtable_fingerprint(&t), qtable_fingerprint(&other));
        // Destination order is part of the identity.
        other = t.clone();
        other.reverse();
        assert_ne!(qtable_fingerprint(&t), qtable_fingerprint(&other));

        // A JPEG carrying no DQT, and a non-JPEG, both give an empty vector rather than an error.
        assert!(jpeg_qtable(&sample_png(None)).is_empty());
        assert!(jpeg_qtable(&[0xFF, 0xD8, 0xFF, 0xD9]).is_empty());
    }

    #[test]
    fn a_sixteen_bit_quantisation_table_reads_big_endian() {
        let mut payload = vec![0x10u8]; // precision 1, destination 0
        for i in 0..64u16 {
            payload.extend_from_slice(&(1000 + i).to_be_bytes());
        }
        let mut file = vec![0xFF, 0xD8];
        seg(&mut file, 0xDB, &payload);
        let t = jpeg_qtable(&file);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].precision, 1);
        assert_eq!(t[0].value[0], 1000);
        assert_eq!(t[0].value[63], 1063);
    }

    // -----------------------------------------------------------------------------------------
    // the flattening into index columns
    // -----------------------------------------------------------------------------------------

    #[test]
    fn term_and_numeric_flatten_into_index_columns() {
        let tiff = sample_tiff(true, "N", "E");
        let Some(e) = parse_tiff(&tiff) else { panic!("parse") };
        let term = e.term();
        let has = |k: &str, v: &str| term.iter().any(|t| t.0 == k && t.1 == v);
        assert!(has("exif.make", "Apple"));
        assert!(has("exif.model", "iPhone 15"));
        assert!(has("exif.camera", "Apple iPhone 15"));
        assert!(has("exif.lens", "Wide Camera"));
        assert!(has("exif.date", "2019-06-12"));

        let num = e.numeric();
        let get = |k: &str| num.iter().find(|n| n.0 == k).map(|n| n.1);
        assert_eq!(get("exif.year"), Some(2019.0));
        assert_eq!(get("exif.month"), Some(6.0));
        assert_eq!(get("exif.day"), Some(12.0));
        assert_eq!(get("exif.iso"), Some(400.0));
        assert_eq!(get("exif.fnumber"), Some(2.8));
        assert_eq!(get("exif.orientation"), Some(6.0));
        assert_eq!(get("exif.focal"), Some(23.0));
        assert!(num.iter().all(|n| n.1.is_finite()));

        // An empty EXIF yields empty columns, not a row of zeros that would match a range query.
        assert!(Exif::default().term().is_empty());
        assert!(Exif::default().numeric().is_empty());
    }

    #[test]
    fn a_maker_prefixed_model_is_not_repeated_in_the_camera_term() {
        let e = Exif {
            make: Some("NIKON CORPORATION".into()),
            model: Some("NIKON D750".into()),
            ..Default::default()
        };
        assert!(e.term().iter().any(|t| t.0 == "exif.camera" && t.1 == "NIKON D750"));
    }

    #[test]
    fn a_dead_camera_clock_does_not_enter_the_date_column() {
        for bad in ["0000:00:00 00:00:00", "    :  :     :  :  ", "1970:13:45 00:00:00", "x"] {
            let e = Exif { datetime: Some(bad.into()), ..Default::default() };
            assert!(e.numeric().is_empty(), "{bad} produced a numeric date");
            assert!(e.term().is_empty(), "{bad} produced a date term");
        }
    }

    // -----------------------------------------------------------------------------------------
    // the security requirement: hostile input must return, never panic
    // -----------------------------------------------------------------------------------------

    /// Everything a caller can reach, run over one buffer. Used by every hostile-input test, so
    /// that a new entry point cannot be added without also being fuzzed.
    fn exercise(byte: &[u8]) {
        let p = probe(byte);
        let _ = p.format.name();
        let _ = sniff(byte);
        let _ = exif_block(byte);
        let _ = parse(byte);
        let _ = parse_tiff(byte);
        let _ = embedded_thumbnail(byte);
        let t = jpeg_qtable(byte);
        let _ = qtable_fingerprint(&t);
        if let Some(e) = parse_tiff(byte) {
            let _ = e.term();
            let _ = e.numeric();
            let _ = e.thumb_slice(byte);
            let _ = e.is_empty();
        }
    }

    #[test]
    fn truncation_at_every_length_returns() {
        let tiff = sample_tiff(true, "S", "W");
        let corpus = [
            sample_jpeg(Some(&tiff)),
            sample_png(Some(&tiff)),
            sample_webp(Some(&tiff)),
            sample_gif(),
            sample_heif(),
            tiff.clone(),
            sample_tiff(false, "N", "E"),
        ];
        for file in &corpus {
            // Every length from empty to 64 bytes, plus every length of the whole file: a header
            // that is cut mid-field is the commonest shape of a real scraped-corpus failure.
            for n in 0..file.len().min(64) + 1 {
                exercise(&file[..n]);
            }
            for n in 0..file.len() {
                exercise(&file[..n]);
            }
        }
    }

    #[test]
    fn a_cyclic_ifd_pointer_terminates() {
        // IFD0's Exif pointer points at IFD0, and its next-IFD pointer points at IFD0 too. Without
        // the visited set this walks forever; with it, the second visit is dropped on the pop.
        let dir = vec![Dir {
            entry: vec![ascii(0x010F, "Loop"), ifd_ptr(0x8769, 0), ifd_ptr(0x8825, 0)],
            next: Some(0),
        }];
        let tiff = build_tiff(true, &dir, &[]);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.make.as_deref(), Some("Loop"));

        // A two-node cycle: IFD0's Exif pointer is IFD1, IFD1's is IFD0.
        let dir = vec![
            Dir { entry: vec![ifd_ptr(0x8769, 1), ascii(0x010F, "A")], next: None },
            Dir { entry: vec![ifd_ptr(0x8769, 0), ascii(0x0110, "B")], next: None },
        ];
        let tiff = build_tiff(true, &dir, &[]);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.make.as_deref(), Some("A"));
        assert_eq!(e.model.as_deref(), Some("B"));
    }

    #[test]
    fn an_offset_past_the_end_of_the_buffer_is_ignored() {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        push16(&mut tiff, 42, true);
        push32(&mut tiff, 0xFFFF_FFF0, true); // IFD0 far past the end
        exercise(&tiff);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert!(e.is_empty());

        // A valid directory whose single entry points its value past the end.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        push16(&mut tiff, 42, true);
        push32(&mut tiff, 8, true);
        push16(&mut tiff, 1, true);
        push16(&mut tiff, 0x010F, true); // Make
        push16(&mut tiff, 2, true); // ASCII
        push32(&mut tiff, 40, true); // 40 bytes, so the value is a pointer
        push32(&mut tiff, 0xFFFF_0000, true);
        push32(&mut tiff, 0, true);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.make, None);
    }

    #[test]
    fn an_absurd_component_count_allocates_nothing() {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        push16(&mut tiff, 42, true);
        push32(&mut tiff, 8, true);
        push16(&mut tiff, u16::MAX, true); // 65 535 entries claimed, none present
        push16(&mut tiff, 0x010F, true);
        push16(&mut tiff, 2, true);
        push32(&mut tiff, u32::MAX, true); // 4 GiB of ASCII claimed
        push32(&mut tiff, 8, true);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.make, None);
        assert!(e.is_empty());

        // The same claim on a RATIONAL, where count * 8 would overflow a 32-bit multiply.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM");
        push16(&mut tiff, 42, false);
        push32(&mut tiff, 8, false);
        push16(&mut tiff, 1, false);
        push16(&mut tiff, 0x829D, false);
        push16(&mut tiff, 5, false);
        push32(&mut tiff, u32::MAX, false);
        push32(&mut tiff, 0, false);
        let Some(e) = parse_tiff(&tiff) else { panic!("header rejected") };
        assert_eq!(e.f_number, None);
    }

    #[test]
    fn a_byte_order_mark_with_garbage_after_it_is_refused_or_empty() {
        // The magic 42 is what separates a TIFF header from two bytes that happen to spell `II`.
        assert_eq!(parse_tiff(b"II\xff\xffgarbage-follows-here"), None);
        assert_eq!(parse_tiff(b"MM\x00\x2a"), None); // header cut before the IFD offset
        assert_eq!(parse_tiff(b"XX\x2a\x00\x08\x00\x00\x00"), None); // no byte-order mark
        for tail in 0..40usize {
            let mut v = b"II\x2a\x00".to_vec();
            v.extend(std::iter::repeat(0xAB).take(tail));
            exercise(&v);
            let mut v = b"MM\x00\x2a".to_vec();
            v.extend(std::iter::repeat(0x7F).take(tail));
            exercise(&v);
        }
    }

    #[test]
    fn a_jpeg_of_nothing_but_filler_bytes_terminates() {
        let mut v = vec![0xFF, 0xD8];
        v.extend(std::iter::repeat(0xFF).take(20_000));
        exercise(&v);
        let mut v = vec![0xFF, 0xD8];
        v.extend(std::iter::repeat(0x00).take(20_000));
        exercise(&v);
        // A segment declaring a length of zero must not make the walk stand still.
        let mut v = vec![0xFF, 0xD8];
        for _ in 0..500 {
            v.extend_from_slice(&[0xFF, 0xE1, 0x00, 0x00]);
        }
        exercise(&v);
    }

    #[test]
    fn a_png_or_riff_chunk_length_of_zero_or_absurd_terminates() {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        for _ in 0..500 {
            push32(&mut v, 0, false);
            v.extend_from_slice(b"junk");
            push32(&mut v, 0, false);
        }
        exercise(&v);

        let mut v = b"RIFFxxxxWEBP".to_vec();
        push32(&mut v, 0, true);
        v.extend_from_slice(b"VP8L");
        push32(&mut v, u32::MAX, true);
        exercise(&v);

        // A HEIF box declaring size 8 (header only) repeated, and one declaring size 1 with no
        // largesize behind it.
        let mut v = vec![0, 0, 0, 0x10];
        v.extend_from_slice(b"ftypheic\0\0\0\0mif1");
        for _ in 0..300 {
            push32(&mut v, 8, false);
            v.extend_from_slice(b"meta");
        }
        exercise(&v);
        let mut v = vec![0, 0, 0, 0x10];
        v.extend_from_slice(b"ftypavif\0\0\0\0avif");
        push32(&mut v, 1, false);
        v.extend_from_slice(b"meta");
        exercise(&v);
    }

    /// A 64-bit LCG. Seeded and pure, so a failure is reproducible from the seed printed in the
    /// assertion — and no `rand` dependency, which this crate does not have and will not gain.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.0 >> 11
        }
        fn byte(&mut self) -> u8 {
            (self.next() & 0xFF) as u8
        }
        fn upto(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }
    }

    #[test]
    fn a_fuzz_shaped_sweep_of_random_buffers_always_returns() {
        // Not a substitute for a real fuzzer, and not claimed to be one: it is a cheap, hermetic,
        // deterministic sweep that runs on every `cargo test` and would have caught each of the
        // bounds bugs this parser is written to make impossible. Purely random bytes almost never
        // sniff as anything, so three quarters of the buffers are seeded with a real magic — the
        // interesting failures are past the header, not at it.
        const MAGIC: [&[u8]; 6] = [
            b"\xFF\xD8\xFF\xE1",
            b"\x89PNG\r\n\x1a\n",
            b"RIFF\x00\x00\x00\x00WEBP",
            b"II\x2a\x00\x08\x00\x00\x00",
            b"MM\x00\x2a\x00\x00\x00\x08",
            b"\x00\x00\x00\x18ftypavif",
        ];
        let mut rng = Lcg(0x5EED_1234_ABCD_0001);
        for _ in 0..4000 {
            let len = rng.upto(200);
            let mut v = Vec::with_capacity(len + 16);
            if rng.upto(4) != 0 {
                v.extend_from_slice(MAGIC[rng.upto(MAGIC.len())]);
            }
            for _ in 0..len {
                v.push(rng.byte());
            }
            exercise(&v);
        }

        // The other half of the sweep: valid files with bytes flipped. This finds the offsets that
        // are only reachable once the surrounding structure parses.
        let tiff = sample_tiff(true, "S", "W");
        let seed = [
            sample_jpeg(Some(&tiff)),
            sample_png(Some(&tiff)),
            sample_webp(Some(&tiff)),
            sample_heif(),
            tiff,
        ];
        for base in &seed {
            for _ in 0..400 {
                let mut v = base.clone();
                for _ in 0..1 + rng.upto(6) {
                    let i = rng.upto(v.len());
                    if let Some(b) = v.get_mut(i) {
                        *b = rng.byte();
                    }
                }
                exercise(&v);
            }
        }
    }

    #[test]
    fn a_stripped_file_is_empty_rather_than_an_error() {
        // The common case in an OSINT corpus, and the distinction the whole API rests on.
        let stripped = sample_jpeg(None);
        assert_eq!(parse(&stripped), None, "no EXIF block at all");
        assert_eq!(embedded_thumbnail(&stripped), None);
        // ... but the file is still fully usable as a document: format, size and the quantisation
        // tables all survive stripping, which is exactly why they are indexed.
        let p = probe(&stripped);
        assert_eq!(p.format, Format::Jpeg);
        assert_eq!((p.width, p.height), (Some(640), Some(480)));
        assert_eq!(jpeg_qtable(&stripped).len(), 2);

        // An EXIF block that is present and carries nothing: read, and empty.
        let empty = build_tiff(true, &[Dir { entry: vec![], next: None }], &[]);
        let file = sample_jpeg(Some(&empty));
        let Some(e) = parse(&file) else { panic!("an empty block is still a block") };
        assert!(e.is_empty());
        assert!(e.term().is_empty() && e.numeric().is_empty());
    }
}
