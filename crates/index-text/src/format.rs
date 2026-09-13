//! The on-disk index format — one file, designed to be read four ways.
//!
//! `docs/research/portability.md` §1 establishes the constraint that decides this module:
//!
//! > **wasi-libc's `mmap` is a fake** — its own source comment says it *"just allocates memory with
//! > malloc and reads and writes data with pread and pwrite"*. So an mmap-based index does not fail
//! > to port to WASM; it **silently reads the entire index into linear memory with no way to opt
//! > out.** That is the worst failure mode available, because it looks like it works.
//!
//! So the format is **range-readable first**, and mmap is an optimization a native reader may layer
//! on. Two properties make that possible and both are load-bearing:
//!
//! 1. **Every offset is `u64`.** 32-bit `usize` on wasm32 was the named blocker that killed
//!    Tantivy's browser PR, and it is unfixable after a format ships.
//! 2. **A section table at a fixed head offset**, so a reader can fetch the term dictionary alone,
//!    or one posting list alone, with a byte range — without parsing anything before it. This is
//!    the property Pagefind and DuckDB-WASM both rely on (10,000 pages in under 300 kB; a Parquet
//!    join reading 3.3 % of the file).
//!
//! Little-endian throughout, because every target that matters is.

use crate::analyze::AliasTable;
use crate::index::{Field, Index, Schema};

/// `"IDXTEXT11"` — magic plus format version.
///
/// The trailing number has moved 1 -> 12 as sections were added: priors (`2`), facets (`3`),
/// multi-slot facets (`4`, same span count but a different encoding), numeric columns (`5`),
/// token positions (`6`), document keys (`7`), unscored columns (`8`), delta-varint position
/// sections (`9`), delta-varint posting lists (`10`), mode-coded posting lists (`11`),
/// checkpointed posting offsets, columnar doc lengths and width-coded facet ids (`12`).
/// Every bump is forced by the `TABLE_BYTE` assertion in the tests rather than remembered.
///
/// **`10` is nine bytes, not eight.** The version outgrew the digit the magic reserved for it,
/// which widens the head by one and moves the section table with it -- unavoidable without
/// abandoning the version number, and every offset in the file is absolute from the start of it,
/// so nothing else shifts. Version 11 folded the name back to eight bytes: `IDXTXT` + two digits.
///
/// The last three bumps did **not** widen the section table — they changed the ENCODING of a
/// section that was already there, which is the same incompatibility in fewer bytes. A `10`
/// reader decoding an `11` posting section would read a varint list count correctly and then
/// misparse every posting after it, and every answer it returned would be a confident wrong one.
/// That is exactly the "plausible garbage" this format bumps to avoid, so the magic moved even
/// though the table did not.
///
/// **Eight bytes, always.** The `E` is dropped at version 10 rather than letting the magic grow to
/// nine, because a RANGE reader has to know how many bytes the head is BEFORE it can know the
/// version — read 297 and you mis-parse a v9 file, read 296 and you mis-parse a v10 one. A
/// fixed-width magic keeps `MAGIC.len() + TABLE_BYTE` a constant a browser can fetch blind, which
/// is what `js/opfs-worker.mjs` does and what `p68` publishes as "296 bytes to open any file".
pub const MAGIC: [u8; 8] = *b"IDXTXT12";

/// The magic of every format version this crate has ever written, oldest first, so a reader can
/// say *"that is an `IDXTEXT9` file, this build reads `IDXTEXT11`"* instead of *"bad magic"*.
///
/// An index is a file a consumer keeps. Telling them their file is a stale version they must
/// rebuild is a different instruction from telling them it is not an index at all, and only one of
/// those is true when the digit moves.
///
/// All of these are matched as exact-width prefixes, so the eight-byte `IDXTXT10` sits beside the
/// nine-byte `IDXTEXT1`–`9` names — see `MAGIC` for why the width is held constant instead of
/// growing at version 10.
const KNOWN_MAGIC: [&[u8]; 11] = [
    b"IDXTEXT1", b"IDXTEXT2", b"IDXTEXT3", b"IDXTEXT4", b"IDXTEXT5", b"IDXTEXT6", b"IDXTEXT7",
    b"IDXTEXT8", b"IDXTEXT9", b"IDXTXT10", b"IDXTXT11",
];

/// Explain a magic mismatch: an older (or newer) `IDXTEXT` version, or not an index at all.
fn magic_error(head: &[u8]) -> String {
    let this = String::from_utf8_lossy(&MAGIC).into_owned();
    if head.len() < MAGIC.len() {
        return format!("need {} bytes of magic, got {}", MAGIC.len(), head.len());
    }
    let got = &head[..MAGIC.len()];
    // Newest first, so that once a nine-byte version is known it is named rather than the
    // eight-byte prefix it starts with.
    for known in KNOWN_MAGIC.iter().rev() {
        if !got.starts_with(known) {
            continue;
        }
        let seen = String::from_utf8_lossy(known).into_owned();
        return format!(
            "{seen} file, but this build reads {this} — an older index-text format version,              rebuild the index"
        );
    }
    if got.starts_with(b"IDXTEXT") {
        let seen = String::from_utf8_lossy(got).into_owned();
        return format!("{seen} file, but this build reads {this} — unknown format version");
    }
    format!("bad magic — not an index-text file (this build reads {this})")
}

/// Byte offset and length of one section. All `u64`, all absolute from the start of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub offset: u64,
    pub len: u64,
}

impl Span {
    fn range(&self) -> std::ops::Range<usize> {
        self.offset as usize..(self.offset + self.len) as usize
    }
}

/// The section table. Fixed size, immediately after the magic, so a range reader can fetch
/// `MAGIC.len() + size_of::<SectionTable>()` bytes and then seek anywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionTable {
    pub meta: Span,
    pub schema: Span,
    pub alias: Span,
    /// The serialized FST.
    pub dict: Span,
    /// `term_count + 1` `u64` offsets into `posting`, cumulative. Entry `i..i+1` bounds term `i`.
    ///
    /// Still fixed width, deliberately: this is the array a range reader fetches to turn a term
    /// into a byte range, so it has to be indexable without decoding anything before it.
    pub posting_offset: Span,
    /// Mode-coded posting lists, one per term, in the order [`SectionTable::posting_offset`]
    /// bounds. Each list is self-describing — a varint **posting count** chooses the mode by its
    /// own magnitude, then the postings — so one list still decodes on its own byte range.
    ///
    /// **Short lists (`POST_BLOCKED_MIN` or fewer postings) are sparse varint.** The measured
    /// shape of every real corpus so far is a long tail of distinctive terms whose median list
    /// holds ONE posting, where per-block framing costs more than it saves. Per posting: a varint
    /// **document-id delta** (first written whole, rest as gaps — ids ascend within a list), then
    /// one **mask byte** whose bit `i` says field `i`'s term frequency is nonzero, then a varint
    /// for each SET bit in field order. Two thirds of frequency slots are zero on the presyo
    /// catalogue; before `11` every one of those zeros cost a whole byte.
    ///
    /// **Long lists are block-FOR columnar.** The head terms carry 72 % of posting bytes, and a
    /// varint pays 2–3 bytes where a frame of reference pays one. The five columns — document-id
    /// deltas, then the `MAX_FIELD` term-frequency columns — are each cut into `POST_BLOCK`
    /// blocks: a column whose every value is zero is a single `0xFF` kind byte; otherwise each
    /// block is a u8 **bit width** followed by that many values packed LSB-first. A width of zero
    /// is itself legal (all-zero block, no payload), so an outlier only widens its own block.
    ///
    /// Fixed width through `IDXTEXT9` (`u32` doc + `MAX_FIELD` × `u16`), and `p54` is the
    /// precedent: it measured the position sections paying eight bytes to say *"+1"*. The same
    /// was true here -- postings are the largest section in the file, and most of the bytes in
    /// one were a four-byte document id whose delta from the previous posting is usually 1, plus
    /// an array of term frequencies that are usually 0 and 1.
    ///
    /// The count is written because a variable-width section has no stride to check a length
    /// against, and a posting list is a place where a wrong length is a wrong ANSWER: reading
    /// one posting too few silently drops a document from a term, and one too many steals the
    /// next term's first posting.
    pub posting: Span,
    pub doc_len: Span,
    /// `doc_count` × `f32` static priors, or a **zero-length span** when the index has none.
    ///
    /// Added in `IDXTEXT2`. The magic was bumped rather than the span appended silently: a reader
    /// expecting seven spans would mis-parse an eight-span table as posting data, and a wrong
    /// offset into a posting section fails as garbage results rather than as an error.
    pub prior: Span,
    /// `doc_count` × `u32` first-token term ids, or a **zero-length span** when absent.
    pub first_term: Span,
    /// `ceil(doc_count / 64)` × `u64` deleted bits, or a **zero-length span** when nothing is
    /// deleted.
    pub deleted: Span,
    /// Learned query expansion, or a **zero-length span** when none was learned.
    pub expansion: Span,
    /// Interned facet labels, per **slot**: `u64` slot count, then per slot a `u32` schema field
    /// index, a `u64` label count, and that many length-prefixed strings, sorted.
    /// **Zero-length span** when the index has no facet field.
    ///
    /// Added in `IDXTEXT3`, and the magic was bumped for the same reason `IDXTEXT2` bumped it: a
    /// reader expecting eleven spans would read a thirteen-span table's first two facet offsets as
    /// section data. A wrong offset into a posting section fails as plausible garbage results, not
    /// as an error, which is the failure mode this format spends bytes to avoid.
    pub facet_label: Span,
    /// `slot count` × `doc_count` × `u32` label ids, slot-major, `u32::MAX` for "no value".
    /// Zero-length span when absent.
    ///
    /// The span count did not change from `IDXTEXT3` to `IDXTEXT4` — the *encoding* of these two
    /// spans did, when faceting grew from one field to many. `ABI_VERSION` and `MAGIC` both moved
    /// anyway: the rule this format follows is "bump on any incompatible change", not "bump when
    /// the table gets wider", and a reader that parsed the old single-slot layout would read a slot
    /// count as a label count and produce plausible garbage.
    pub facet_id: Span,
    /// Numeric column fields: `u64` slot count then that many `u32` schema field indices.
    /// **Zero-length span** when the index has no numeric column.
    pub numeric_field: Span,
    /// `slot count` x `doc_count` x `f64` values, slot-major, `NaN` for absent.
    /// Zero-length span when absent.
    pub numeric_value: Span,
    /// `total posting count + 1` `u64` cumulative offsets into `position`, in POSITION units, not
    /// bytes. **Zero-length span** when the index was built without positions, which is the
    /// default -- an index that answers no phrase query stores none.
    ///
    /// Added in `IDXTEXT6`. Two spans rather than one because the offsets are what make the
    /// variable-length position runs addressable, and a reader must be able to reject a
    /// `position_at` that does not have exactly one entry per posting plus a terminator: a
    /// short one would hand the phrase verifier another posting's positions, and it would agree
    /// with them. That is a wrong answer that looks like a working phrase search.
    pub position_at: Span,
    /// One `u32` per token OCCURRENCE, `field << 16 | token_index`, ascending within each posting
    /// run. Zero-length span when positions are off.
    pub position: Span,
    /// The application's primary key per document: a `u32` schema field index, then `doc_count`
    /// length-prefixed strings in document order. An empty string means that row has no key.
    /// **Zero-length span** when the index was built without one.
    ///
    /// Added in `IDXTEXT7`. Length-prefixed rather than NUL-separated for the same reason the facet
    /// tally is: a primary key may legitimately contain any byte, and this format need not assume
    /// even that it contains no NUL.
    pub doc_key: Span,
}

/// Section table is 18 spans × 2 × u64.
const TABLE_BYTE: usize = 18 * 16;

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Writer { buf: Vec::new() }
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// LEB128. Seven bits per byte, high bit continues.
    ///
    /// Used only where the values are known to be small and numerous — the position sections,
    /// where `p45` measured the fixed-width form costing **2.2x the data it addressed**, because
    /// most (term, document) pairs carry exactly one position and each was paying an eight-byte
    /// offset to say so.
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }
    fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u64(v.len() as u64);
        self.buf.extend_from_slice(v);
    }
    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }
    fn here(&self) -> u64 {
        self.buf.len() as u64
    }
    fn span_from(&self, start: u64) -> Span {
        Span { offset: start, len: self.here() - start }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Reader { b, p: 0 }
    }
    /// `checked_add`, not `+`: a corrupt length prefix arrives here as a near-`usize::MAX` `n`,
    /// and `self.p + n` then overflows and PANICS -- which this format explicitly promises not to
    /// do. Found by `corrupt_input_errors_rather_than_panics` when the facet sections gave it a
    /// second length-prefixed section to reach; `expansion` had the same latent path.
    fn u8(&mut self) -> Result<u8, String> {
        self.need(1)?;
        let v = self.b[self.p];
        self.p += 1;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, String> {
        self.need(2)?;
        let v = u16::from_le_bytes(self.b[self.p..self.p + 2].try_into().unwrap());
        self.p += 2;
        Ok(v)
    }

    fn need(&self, n: usize) -> Result<(), String> {
        match self.p.checked_add(n) {
            Some(end) if end <= self.b.len() => Ok(()),
            _ => Err(format!("truncated: want {n} bytes at {} of {}", self.p, self.b.len())),
        }
    }
    fn u32(&mut self) -> Result<u32, String> {
        self.need(4)?;
        let v = u32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        Ok(v)
    }
    fn u64(&mut self) -> Result<u64, String> {
        self.need(8)?;
        let v = u64::from_le_bytes(self.b[self.p..self.p + 8].try_into().unwrap());
        self.p += 8;
        Ok(v)
    }
    /// The explicit element count that prefixes each varint-encoded section.
    fn varint_u64_count(&mut self) -> Result<usize, String> {
        let n = self.u64()? as usize;
        // A count larger than the section could possibly hold is corrupt; bound it before
        // allocating, the same guard the expansion and facet sections use.
        if n > self.b.len().saturating_sub(self.p).saturating_add(1) * 8 {
            return Err(format!("section claims {n} entries, span cannot hold them"));
        }
        Ok(n)
    }

    /// LEB128, refusing an over-long encoding rather than silently wrapping.
    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            self.need(1)?;
            let b = self.b[self.p];
            self.p += 1;
            // 10 groups of 7 bits is the most a u64 can hold; an 11th means corrupt input.
            if shift >= 64 {
                return Err("varint is longer than 64 bits".into());
            }
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }
    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn f32(&mut self) -> Result<f32, String> {
        self.need(4)?;
        let v = f32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        Ok(v)
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.u64()? as usize;
        self.need(n)?;
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn str(&mut self) -> Result<String, String> {
        let b = self.bytes()?;
        String::from_utf8(b.to_vec()).map_err(|e| format!("bad utf8: {e}"))
    }
}

/// Read the section table from the head of a serialized index.
///
/// **This is the range-read entry point**: a browser or edge reader fetches only
/// `MAGIC.len() + 288` bytes to learn where everything else lives.
pub fn read_section_table(head: &[u8]) -> Result<SectionTable, String> {
    if head.len() < MAGIC.len() + TABLE_BYTE {
        return Err(format!("need {} head bytes, got {}", MAGIC.len() + TABLE_BYTE, head.len()));
    }
    if head[..MAGIC.len()] != MAGIC {
        return Err(magic_error(head));
    }
    let mut r = Reader::new(&head[MAGIC.len()..]);
    let mut span = || -> Result<Span, String> {
        Ok(Span { offset: r.u64()?, len: r.u64()? })
    };
    Ok(SectionTable {
        meta: span()?,
        schema: span()?,
        alias: span()?,
        dict: span()?,
        posting_offset: span()?,
        posting: span()?,
        doc_len: span()?,
        prior: span()?,
        first_term: span()?,
        deleted: span()?,
        expansion: span()?,
        facet_label: span()?,
        facet_id: span()?,
        numeric_field: span()?,
        numeric_value: span()?,
        position_at: span()?,
        position: span()?,
        doc_key: span()?,
    })
}

/// The byte range holding term `term_id`'s posting list, given only the `posting_offset` section.
///
/// This is what makes the format range-readable: a reader that has fetched the section table and
/// the offset array can pull **one posting list** with a single ranged request, touching none of
/// the rest of the file.
pub fn posting_span(
    table: &SectionTable,
    posting_offset_bytes: &[u8],
    term_id: u32,
) -> Result<Span, String> {
    posting_span_from_bytes(posting_offset_bytes, term_id)
        .map(|(a, b)| Span { offset: table.posting.offset + a, len: b - a })
}

/// Entries `i` and `i+1` of the block-directory posting-offset array, decoded from the
/// section's own bytes: entry `i`'s block directory record sits at a COMPUTABLE byte position,
/// so the decode is the directory read plus at most `OFFSET_CHECKPOINT` varints. Returns
/// `(offset of term i, offset of term i+1)`, in posting-section units.
fn posting_span_from_bytes(b: &[u8], term_id: u32) -> Result<(u64, u64), String> {
    if b.len() < 8 {
        return Err("posting offset section is truncated".into());
    }
    let entries = u64::from_le_bytes(b[0..8].try_into().unwrap()) as usize;
    if entries == 0 {
        return Err("posting offset section claims no entries".into());
    }
    if term_id as usize + 1 >= entries {
        return Err(format!(
            "term {term_id} out of range of the offset array ({entries} entries)"
        ));
    }
    let dir = |block: usize| -> Result<(u64, u64), String> {
        let at = 8 + block * 16;
        if at + 16 > b.len() {
            return Err("posting offset directory is truncated".into());
        }
        Ok((
            u64::from_le_bytes(b[at..at + 8].try_into().unwrap()),
            u64::from_le_bytes(b[at + 8..at + 16].try_into().unwrap()),
        ))
    };
    // Decode `(start_value, deltas 1..=r)` for the block a given entry lives in.
    let entry = |j: usize| -> Result<u64, String> {
        let block = j / OFFSET_CHECKPOINT;
        let (mut val, start_byte) = dir(block)?;
        let mut at = start_byte as usize;
        for _ in 0..(j % OFFSET_CHECKPOINT) {
            let mut r = Reader::new(&b[at..]);
            val += r.varint()?;
            at += r.p;
        }
        Ok(val)
    };
    Ok((entry(term_id as usize)?, entry(term_id as usize + 1)?))
}

/// The `12` posting-offset section, from each term's posting-list byte length in term-id order.
///
/// The one encoder for this section: the file writer calls it, and so does the range tier when it
/// assembles an image around the lists it fetched. Offsets are relative to the start of the posting
/// section; directory byte positions are relative to the start of THIS section, so the bytes are
/// position-independent and the caller may place them anywhere.
pub fn encode_posting_offset(list_byte: &[u64]) -> Vec<u8> {
    let mut w = Writer::new();
    let entries = list_byte.len() + 1;
    let blocks = entries.div_ceil(OFFSET_CHECKPOINT);
    w.u64(entries as u64);
    let dir_at = w.here() as usize;
    w.buf.extend_from_slice(&vec![0u8; blocks * 16]);
    let deltas_at = w.here();
    let set_dir = |buf: &mut Vec<u8>, block: usize, value: u64, byte: u64| {
        let at = dir_at + block * 16;
        buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
        buf[at + 8..at + 16].copy_from_slice(&byte.to_le_bytes());
    };
    set_dir(&mut w.buf, 0, 0, deltas_at);
    let mut acc = 0u64;
    let mut block = 0usize;
    for (i, byte) in list_byte.iter().enumerate() {
        let j = i + 1;
        acc += byte;
        if j % OFFSET_CHECKPOINT == 0 {
            block += 1;
            let byte_at = w.here();
            set_dir(&mut w.buf, block, acc, byte_at);
        } else {
            w.varint(*byte);
        }
    }
    debug_assert_eq!(block + 1, blocks);
    w.buf
}

/// Lists at least this long use the block-FOR columnar mode; shorter ones use sparse varint.
/// The threshold is measured, not aesthetic: on the presyo catalogue lists past this size carry
/// the strong majority of posting bytes, and below it the median list holds one posting, where
/// every framing byte costs more than the compression it enables. Both the writer and the reader
/// branch on the SAME count, so no mode byte exists to disagree about.
const POST_BLOCKED_MIN: usize = 64;

/// Values per block in the columnar mode. Small enough that the last partial block of a
/// 128-posting list is half the list; large enough that one width byte amortizes to under a
/// hundredth of a bit per value.
const POST_BLOCK: usize = 128;

/// Whole-`u64` checkpoints between varint deltas in the posting-offset section: term `i`'s
/// span is decoded by walking at most `OFFSET_CHECKPOINT - 1` varints from its checkpoint.
const OFFSET_CHECKPOINT: usize = 64;

/// Column kind byte: every value in the column is zero, and no blocks follow.
const COL_ALL_ZERO: u8 = 0xFF;
/// Column kind byte: packed blocks follow, each a width byte plus its payload.
const COL_PACKED: u8 = 0x00;

/// Bit-pack `vals` at `bits` width each, LSB-first, appending whole bytes to `out`.
/// `bits` is 0..=32; zero-width callers emit nothing.
fn block_pack(out: &mut Vec<u8>, vals: &[u64], bits: u8) {
    if bits == 0 {
        return;
    }
    let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
    let mut acc: u64 = 0;
    let mut used: u32 = 0;
    for &v in vals {
        acc |= (v & mask) << used;
        used += bits as u32;
        while used >= 8 {
            out.push((acc & 0xFF) as u8);
            acc >>= 8;
            used -= 8;
        }
    }
    if used > 0 {
        out.push((acc & 0xFF) as u8);
    }
}

/// The width that holds the largest value in `vals` — 0 when they all are, which packs as a
/// payload-free all-zero block.
fn column_bits(vals: &[u64]) -> u8 {
    let max = vals.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return 0;
    }
    (u64::BITS - max.leading_zeros()) as u8
}

/// Unpack `block_len` values of `bits` width each from `b[*p..]`, advancing `*p` past the block's
/// payload. LSB-first, the exact inverse of [`block_pack`].
fn block_unpack(b: &[u8], p: &mut usize, block_len: usize, bits: u8) -> Result<[u64; POST_BLOCK], String> {
    let mut out = [0u64; POST_BLOCK];
    if bits == 0 {
        return Ok(out);
    }
    if bits as usize > 32 {
        return Err(format!("block width {bits} exceeds the 32-bit maximum"));
    }
    let nbytes = (block_len * bits as usize).div_ceil(8);
    if p.checked_add(nbytes).map_or(true, |end| end > b.len()) {
        return Err(format!(
            "block of {block_len} values at {bits} bits needs {nbytes} bytes, {} left",
            b.len() - *p
        ));
    }
    let mask = (1u64 << bits) - 1;
    let mut acc: u64 = 0;
    let mut used: u32 = 0;
    let mut src = *p;
    for v in out.iter_mut().take(block_len) {
        while used < bits as u32 {
            acc |= (b[src] as u64) << used;
            src += 1;
            used += 8;
        }
        *v = acc & mask;
        acc >>= bits;
        used -= bits as u32;
    }
    *p = src;
    Ok(out)
}

/// Decode one term's posting list from its own byte range.
///
/// This is the range-read unit -- the decode a browser performs on the bytes [`posting_span`]
/// hands back -- and it is also the decode [`Index::from_bytes`] performs on every span of the
/// posting section, so the two cannot drift apart.
///
/// Strict by the standard `p54` set for the position sections, because the failure here is a
/// wrong ANSWER rather than a failed load: a count is written explicitly and bounded by the span
/// before it is trusted, every accumulation is `checked_add`, document ids must strictly ascend
/// (which the delta encoding depends on), and the span must be consumed EXACTLY -- the check
/// fixed width used to give for free, since `len % POSTING_BYTE == 0` no longer exists.
fn read_posting_list(b: &[u8]) -> Result<Vec<(u32, [u16; crate::index::MAX_FIELD])>, String> {
    let mut r = Reader::new(b);
    let n = r.varint()?;
    let n = usize::try_from(n).map_err(|_| "posting count exceeds usize".to_string())?;
    // Bound the claimed count before allocating. The floor is mode-aware: a columnar list packs
    // `POST_BLOCK` deltas into as few as n/8 bytes (an ascending delta is at least one BIT), plus
    // one kind byte per column, so the old one-byte-per-posting floor rejected legitimate lists —
    // caught by the reload of a real 20,000-row build, not by a unit test.
    let room = r.b.len().saturating_sub(r.p);
    let floor = if n >= POST_BLOCKED_MIN {
        n.div_ceil(8) + crate::index::MAX_FIELD + 1
    } else {
        // Sparse: every posting costs a document delta and a mask byte at minimum.
        n * 2
    };
    if floor > room {
        return Err(format!("posting list claims {n} postings, {room} bytes left in its span"));
    }
    let mut list = Vec::with_capacity(n);
    if n >= POST_BLOCKED_MIN {
        read_blocked_columns(&mut r, n, &mut list)?;
    } else {
        let mut prev = 0u32;
        for i in 0..n {
            let delta = u32::try_from(r.varint()?).map_err(|_| "document delta exceeds u32")?;
            // Strictly ascending, so a zero delta is one document posted twice. Refused rather
            // than tolerated: two entries for one document double-count it, and BM25 would score
            // it twice.
            if i > 0 && delta == 0 {
                return Err(format!("posting {i} repeats document {prev}"));
            }
            let doc = prev.checked_add(delta).ok_or("document id overflowed u32")?;
            prev = doc;
            // One mask byte names the nonzero frequency slots; only those cost a varint.
            let mask = r.b.get(r.p).copied().ok_or("posting list ends inside a frequency mask")?;
            r.p += 1;
            let mut tf = [0u16; crate::index::MAX_FIELD];
            for (f, t) in tf.iter_mut().enumerate() {
                if mask & (1 << f) == 0 {
                    continue;
                }
                *t = u16::try_from(r.varint()?).map_err(|_| "term frequency exceeds u16")?;
            }
            list.push((doc, tf));
        }
    }
    if r.p != r.b.len() {
        return Err(format!(
            "posting list has {} bytes left after its {n} postings",
            r.b.len() - r.p
        ));
    }
    Ok(list)
}

/// The block-FOR columnar arm of [`read_posting_list`]: `n` postings as five columns —
/// document-id deltas, then `MAX_FIELD` frequency columns — each all-zero-flagged or cut into
/// bit-packed blocks. Fills `list` in document order.
fn read_blocked_columns(
    r: &mut Reader,
    n: usize,
    list: &mut Vec<(u32, [u16; crate::index::MAX_FIELD])>,
) -> Result<(), String> {
    // Column payloads are consumed with a cursor that shares the reader's span, so the
    // exact-consume check at the end still governs every byte.
    list.resize(n, (0, [0; crate::index::MAX_FIELD]));
    for col in 0..=crate::index::MAX_FIELD {
        let kind = r.b.get(r.p).copied().ok_or("posting list ends inside a column header")?;
        r.p += 1;
        match kind {
            COL_ALL_ZERO => continue, // every value is 0, which is what the list was resized to
            COL_PACKED => {}
            other => {
                return Err(format!("column kind {other:#04x} is neither 0x00 nor 0xFF"));
            }
        }
        let mut prev = 0u32;
        let mut done = 0usize;
        while done < n {
            let len = POST_BLOCK.min(n - done);
            let bits = r.b.get(r.p).copied().ok_or("posting list ends inside a block header")?;
            r.p += 1;
            let vals = block_unpack(r.b, &mut r.p, len, bits)?;
            for (i, &v) in vals.iter().take(len).enumerate() {
                let at = done + i;
                if col == 0 {
                    let delta =
                        u32::try_from(v).map_err(|_| "document delta exceeds u32".to_string())?;
                    if at > 0 && delta == 0 {
                        return Err(format!("posting {at} repeats document {prev}"));
                    }
                    prev = if at == 0 {
                        delta
                    } else {
                        prev.checked_add(delta).ok_or("document id overflowed u32")?
                    };
                    list[at].0 = prev;
                } else {
                    let t = u16::try_from(v).map_err(|_| "term frequency exceeds u16".to_string())?;
                    list[at].1[col - 1] = t;
                }
            }
            done += len;
        }
    }
    Ok(())
}

impl Index {
    /// Serialize to the portable format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.buf.extend_from_slice(&MAGIC);
        // Reserve the section table; it is backfilled once the spans are known.
        let table_at = w.here() as usize;
        w.buf.extend_from_slice(&[0u8; TABLE_BYTE]);

        let s = self.snapshot();

        let start = w.here();
        w.u64(s.doc_count as u64);
        w.u32(s.field.len() as u32);
        w.f32(s.k1);
        w.f32(s.typo_penalty);
        for v in s.avg_len.iter() {
            w.f32(*v);
        }
        // Appended in `IDXTEXT8` rather than placed beside `field.len()`, so the bytes an
        // `IDXTEXT7` reader would have read are still in the same places -- the version digit is
        // what rejects the file, not a shifted offset producing a plausible wrong number.
        w.u32(s.unscored.len() as u32);
        let meta = w.span_from(start);

        let start = w.here();
        for f in s.field.iter() {
            w.str(&f.name);
            w.f32(f.boost);
            w.f32(f.b);
        }
        // Unscored columns carry a name and nothing else: no boost and no `b`, because nothing
        // scores them. See `Schema::with_column`.
        for name in s.unscored.iter() {
            w.str(name);
        }
        let schema = w.span_from(start);

        let start = w.here();
        w.u64(s.alias.len() as u64);
        for (k, v) in s.alias.iter() {
            w.str(k);
            w.str(v);
        }
        let alias = w.span_from(start);

        let start = w.here();
        w.buf.extend_from_slice(&s.dict_bytes);
        let dict = w.span_from(start);

        // The postings are encoded FIRST, into their own buffer: the offset array that addresses
        // them is written before them in the file, and a variable-width encoding has no stride
        // to compute those offsets from -- they can only be measured.
        let mut post = Writer::new();
        let mut list_byte: Vec<u64> = Vec::with_capacity(s.posting.len());
        // Scratch for the columnar mode: five columns of at most POST_BLOCK values, reused.
        let mut cols: Vec<[u64; POST_BLOCK]> =
            vec![[0u64; POST_BLOCK]; crate::index::MAX_FIELD + 1];
        for list in s.posting.iter() {
            let at = post.here();
            // The count is per LIST rather than once for the section, so one list still decodes
            // on its own: that is the range-read property this format exists to provide. It also
            // CHOOSES the mode — the reader branches on the same magnitude.
            post.varint(list.len() as u64);
            if list.len() >= POST_BLOCKED_MIN {
                // Columnar, COLUMN-MAJOR to match the reader: every block of the document-id
                // column, then every block of each frequency column. An earlier draft interleaved
                // the columns block by block; the two layouts agree on every single-block list and
                // diverge silently on the first multi-block one, which is why only the reload
                // check at scale caught it.
                let nblocks = list.len().div_ceil(POST_BLOCK);
                for (c, buf) in cols.iter_mut().enumerate() {
                    // Column header: an all-zero column — a name-only corpus leaves the other
                    // fields empty for most terms — is one byte instead of a block per 128
                    // postings.
                    if c > 0 && list.iter().all(|p| p.1[c - 1] == 0) {
                        post.buf.push(COL_ALL_ZERO);
                        continue;
                    }
                    post.buf.push(COL_PACKED);
                    for blk in 0..nblocks {
                        let done = blk * POST_BLOCK;
                        let len = POST_BLOCK.min(list.len() - done);
                        for (i, v) in buf.iter_mut().take(len).enumerate() {
                            let p = &list[done + i];
                            *v = if c == 0 {
                                // Deltas from the previous posting — the previous BLOCK's last
                                // doc for position 0, the previous posting within the block
                                // otherwise. The reader accumulates exactly this: it carries
                                // `prev` across blocks and adds every gap.
                                let from = if i == 0 {
                                    if done > 0 {
                                        list[done - 1].0
                                    } else {
                                        0
                                    }
                                } else {
                                    list[done + i - 1].0
                                };
                                (p.0 - from) as u64
                            } else {
                                p.1[c - 1] as u64
                            };
                        }
                        let bits = column_bits(&buf[..len]);
                        post.buf.push(bits);
                        if bits > 0 {
                            block_pack(&mut post.buf, &buf[..len], bits);
                        }
                    }
                }
            } else {
                // Sparse varint: distinctive terms, most with one posting and mostly-zero
                // frequencies — a mask byte instead of `MAX_FIELD` mostly-zero varints.
                let mut prev = 0u32;
                for p in list.iter() {
                    post.varint((p.0 - prev) as u64);
                    prev = p.0;
                    let mut mask = 0u8;
                    for (f, v) in p.1.iter().enumerate() {
                        if *v != 0 {
                            mask |= 1 << f;
                        }
                    }
                    post.buf.push(mask);
                    for (f, v) in p.1.iter().enumerate() {
                        if mask & (1 << f) != 0 {
                            post.varint(*v as u64);
                        }
                    }
                }
            }
            list_byte.push(post.here() - at);
        }

        // Offsets are relative to the start of the posting section, so the section can be
        // relocated or fetched independently. `12` re-laid it out as a directory of blocks:
        // every `OFFSET_CHECKPOINT` entries gets a fixed 16-byte `(cumulative value, byte
        // where this block's deltas start)` record at a COMPUTABLE position, and the entries
        // inside a block are varint deltas from their predecessor. Fixed-width `u64` spent
        // eight bytes on a list length that is usually one or two; the directory keeps any
        // single entry decodable in at most `OFFSET_CHECKPOINT` varints, which a flat varint
        // array cannot — variable widths make byte positions of later blocks unknowable
        // without decoding everything before them.
        let start = w.here();
        w.buf.extend_from_slice(&encode_posting_offset(&list_byte));
        let posting_offset = w.span_from(start);

        let start = w.here();
        w.buf.extend_from_slice(&post.buf);
        let posting = w.span_from(start);

        // `12` columnar: one varint column per field, an all-zero field (a name-only corpus
        // leaves fields 1.. empty for EVERY document) a single 0xFF flag byte instead of two
        // bytes per document. The old fixed `u16` stride was the third-largest section.
        let start = w.here();
        for f in 0..crate::index::MAX_FIELD {
            if s.doc_len.iter().all(|l| l[f] == 0) {
                w.buf.push(0xFF);
            } else {
                w.buf.push(0x00);
                for l in s.doc_len.iter() {
                    w.varint(l[f] as u64);
                }
            }
        }
        let doc_len = w.span_from(start);

        // Written only when it carries information; a uniform prior is the absence of a prior, and
        // an empty span says so without costing `doc_count` floats of ones.
        let start = w.here();
        for v in s.prior.iter() {
            w.f32(*v);
        }
        let prior = w.span_from(start);

        // Anchoring is a ranking signal, so an artifact that dropped it would silently rank worse
        // than the index it was built from — the failure mode would be "search got a bit worse
        // after deploying", which is close to undebuggable.
        let start = w.here();
        for v in s.first_term.iter() {
            w.u32(*v);
        }
        let first_term = w.span_from(start);

        // Deletions MUST persist. An artifact that dropped them would resurrect every deleted
        // document on reload — for presyo that means merged duplicates reappearing in results,
        // which is the exact bug deletion exists to prevent.
        let start = w.here();
        for v in s.deleted.iter() {
            w.u64(*v);
        }
        let deleted = w.span_from(start);

        // A learned expansion is a ranking signal like anchoring, so an artifact that dropped it
        // would rank worse than the index it was built from — and the symptom ("browse got worse
        // after we deployed") is close to undebuggable. Fourth time this rule has applied; it is
        // written once here and in the round-trip test.
        let start = w.here();
        w.u64(s.expansion.len() as u64);
        for (value, term) in s.expansion.iter() {
            w.str(value);
            w.u32(term.len() as u32);
            for t in term {
                w.u32(*t);
            }
        }
        let expansion = w.span_from(start);

        // Facets are a query capability, not a ranking signal, so dropping them fails LOUDLY --
        // a filtered search returns nothing and a tally returns empty. That is the one optional
        // section whose absence is obvious, and it still round-trips for the same reason as the
        // rest: an artifact that answers differently from the index it was built from is worse
        // than one that fails to load.
        let (facet_label_src, facet_id_src) = (s.facet_label, s.facet_id);
        let start = w.here();
        if !facet_label_src.is_empty() {
            w.u64(facet_label_src.len() as u64);
            for (slot, label) in facet_label_src.iter().enumerate() {
                w.u32(s.facet_field.get(slot).copied().unwrap_or(0) as u32);
                w.u64(label.len() as u64);
                for v in label.iter() {
                    w.str(v);
                }
            }
        }
        let facet_label = w.span_from(start);

        // `12` width-coded: a slot whose label count fits u8 costs one byte per document, not
        // four. The all-values-absent column is a 0 width code and nothing else. The sentinel
        // for "no value" is the width's maximum (0xFF / 0xFFFF / u32::MAX), which is why the
        // writer picks a width the label ids actually fit UNDER.
        let start = w.here();
        if !facet_label_src.is_empty() {
            for column in facet_id_src.iter() {
                let max = column
                    .iter()
                    .copied()
                    .filter(|v| *v != u32::MAX)
                    .max()
                    .unwrap_or(0);
                let (code, width): (u8, usize) = if column.iter().all(|v| *v == u32::MAX) {
                    (0, 0)
                } else if max < 0xFF {
                    (1, 1)
                } else if max < 0xFFFF {
                    (2, 2)
                } else {
                    (4, 4)
                };
                w.buf.push(code);
                for v in column.iter() {
                    match width {
                        0 => {}
                        1 => {
                            let b = if *v == u32::MAX { 0xFF } else { *v as u8 };
                            w.buf.push(b);
                        }
                        2 => {
                            let x = if *v == u32::MAX { 0xFFFFu16 } else { *v as u16 };
                            w.u16(x);
                        }
                        _ => w.u32(*v),
                    }
                }
            }
        }
        let facet_id = w.span_from(start);

        // Numeric columns. Written as raw IEEE-754 bits so a NaN -- the marker for "this document
        // has no value here" -- survives verbatim rather than being normalised by a text or decimal
        // round trip.
        let start = w.here();
        if !s.numeric_value.is_empty() {
            w.u64(s.numeric_value.len() as u64);
            for slot in 0..s.numeric_value.len() {
                w.u32(s.numeric_field.get(slot).copied().unwrap_or(0) as u32);
            }
        }
        let numeric_field = w.span_from(start);

        let start = w.here();
        for column in s.numeric_value.iter() {
            for v in column.iter() {
                w.f64(*v);
            }
        }
        let numeric_value = w.span_from(start);

        // Both spans are empty unless the index carries positions, and they are written together:
        // offsets without data, or data without offsets, is not a state a reader should have to
        // have an opinion about.
        // Delta-varint, and `p45` is why: the offsets are monotone and almost always advance by
        // ONE, because most (term, document) pairs carry a single position. Fixed-width `u64` spent
        // eight bytes to say "+1" and made the offset array 2.2x the positions it addressed.
        // Nothing at all when positions are off, so the default still costs ZERO bytes -- pinned
        // by `an_index_without_positions_costs_no_position_bytes`.
        let start = w.here();
        if !s.position_at.is_empty() {
            w.u64(s.position_at.len() as u64);
            let mut prev = 0u64;
            for v in s.position_at.iter() {
                w.varint(v - prev);
                prev = *v;
            }
        }
        let position_at = w.span_from(start);

        // Positions delta-varint WITHIN each posting run. They ascend inside a run and reset at the
        // next one, so a global delta would go negative -- the run boundaries come from
        // `position_at`, which is why that section is written first and read first.
        let start = w.here();
        if !s.position_at.is_empty() {
            w.u64(s.position.len() as u64);
        }
        for run in s.position_at.windows(2) {
            let (lo, hi) = (run[0] as usize, run[1] as usize);
            let mut prev = 0u32;
            for &v in &s.position[lo..hi] {
                // First of a run is written whole; the rest as gaps from the previous.
                w.varint((v - prev) as u64);
                prev = v;
            }
        }
        let position = w.span_from(start);

        // Keys. The field index is written even though nothing reads it at query time, because a
        // host rebuilding the index from its source rows needs to know WHICH column was the key.
        let start = w.here();
        if !s.doc_key.is_empty() {
            w.u32(s.key_field as u32);
            for k in s.doc_key.iter() {
                w.str(k);
            }
        }
        let doc_key = w.span_from(start);

        let mut t = Vec::with_capacity(TABLE_BYTE);
        for sp in [
            meta,
            schema,
            alias,
            dict,
            posting_offset,
            posting,
            doc_len,
            prior,
            first_term,
            deleted,
            expansion,
            facet_label,
            facet_id,
            numeric_field,
            numeric_value,
            position_at,
            position,
            doc_key,
        ] {
            t.extend_from_slice(&sp.offset.to_le_bytes());
            t.extend_from_slice(&sp.len.to_le_bytes());
        }
        w.buf[table_at..table_at + TABLE_BYTE].copy_from_slice(&t);
        w.buf
    }

    /// Load an index previously produced by [`Index::to_bytes`].
    ///
    /// Every length is validated against the buffer, so a truncated or corrupt file returns an
    /// error rather than panicking — this parses untrusted input the moment it is served over HTTP.
    pub fn from_bytes(buf: &[u8]) -> Result<Index, String> {
        let table = read_section_table(buf)?;
        let end = |s: &Span| (s.offset + s.len) as usize;
        for s in [
            &table.meta,
            &table.schema,
            &table.alias,
            &table.dict,
            &table.posting_offset,
            &table.posting,
            &table.doc_len,
            &table.prior,
            &table.first_term,
            &table.deleted,
            &table.expansion,
        ] {
            if end(s) > buf.len() {
                return Err(format!("section {s:?} runs past the end of a {}-byte buffer", buf.len()));
            }
        }

        let mut r = Reader::new(&buf[table.meta.range()]);
        let doc_count = r.u64()? as usize;
        let field_count = r.u32()? as usize;
        if field_count == 0 || field_count > crate::index::MAX_FIELD {
            return Err(format!("field_count {field_count} out of range"));
        }
        let k1 = r.f32()?;
        let typo_penalty = r.f32()?;
        let mut avg_len = [1.0f32; crate::index::MAX_FIELD];
        for v in avg_len.iter_mut() {
            *v = r.f32()?;
        }
        let unscored_count = r.u32()? as usize;
        if field_count + unscored_count > crate::index::MAX_COLUMN {
            return Err(format!(
                "{field_count} fields + {unscored_count} unscored columns exceeds                  MAX_COLUMN {}",
                crate::index::MAX_COLUMN
            ));
        }

        let mut r = Reader::new(&buf[table.schema.range()]);
        let mut field = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let name = r.str()?;
            let boost = r.f32()?;
            let b = r.f32()?;
            field.push(Field { name, boost, b });
        }
        let mut unscored = Vec::with_capacity(unscored_count);
        for _ in 0..unscored_count {
            unscored.push(r.str()?);
        }
        let mut schema = Schema::new(field);
        schema.unscored = unscored;
        schema.k1 = k1;
        schema.typo_penalty = typo_penalty;

        let mut r = Reader::new(&buf[table.alias.range()]);
        let n = r.u64()? as usize;
        let mut alias = AliasTable::new();
        for _ in 0..n {
            let k = r.str()?;
            let v = r.str()?;
            alias.insert(&k, &v);
        }

        let dict_bytes = buf[table.dict.range()].to_vec();

        let off = &buf[table.posting_offset.range()];
        if off.len() < 8 {
            return Err("posting offset array is malformed".into());
        }
        let entries = u64::from_le_bytes(off[0..8].try_into().unwrap()) as usize;
        // Both checks turn what used to be an ABORT into an Err. An entry count of 0 made
        // `term_count` wrap to usize::MAX (release builds do not trap on underflow) and the
        // `with_capacity` below died with "capacity overflow" -- inside wasm, `unreachable`, with
        // no message for the host. A huge count could also overflow `blocks * 16` back into range.
        if entries == 0 {
            return Err("posting offset section claims no entries".into());
        }
        let blocks = entries.div_ceil(OFFSET_CHECKPOINT);
        match blocks.checked_mul(16).and_then(|d| d.checked_add(8)) {
            Some(need) if off.len() >= need => {}
            _ => return Err("posting offset directory is truncated".into()),
        }
        let term_count = entries - 1;
        let post_bytes = &buf[table.posting.range()];
        let mut posting = Vec::with_capacity(term_count);
        // Total postings across every term -- the count `position_at` must have one entry for,
        // plus a terminator. Summed from what was actually decoded, so it cannot drift from it.
        let mut posting_count = 0usize;
        // Full sequential decode of the block directory: walk every block, accumulate deltas,
        // resetting to each block's own start value so a corrupt block cannot poison the next.
        let mut all_offsets = vec![0u64; entries];
        for block in 0..blocks {
            let at = 8 + block * 16;
            let val = u64::from_le_bytes(off[at..at + 8].try_into().unwrap());
            all_offsets[block * OFFSET_CHECKPOINT] = val;
            let byte_at = u64::from_le_bytes(off[at + 8..at + 16].try_into().unwrap()) as usize;
            if byte_at > off.len() {
                return Err(format!("posting offset block {block} points past the section"));
            }
            let mut r = Reader::new(&off[byte_at..]);
            let mut acc = val;
            let last = ((block + 1) * OFFSET_CHECKPOINT).min(entries);
            for slot in all_offsets[(block * OFFSET_CHECKPOINT + 1)..last].iter_mut() {
                acc += r.varint()?;
                *slot = acc;
            }
        }
        let get = { let get = &all_offsets; move |k: usize| get[k] };
        for i in 0..term_count {
            let (a, b) = (get(i), get(i + 1));
            // Checked as `u64` before narrowing: on a 32-bit target a cast would silently
            // truncate an offset back into range and read the wrong bytes as a posting list.
            if b < a || b > post_bytes.len() as u64 {
                return Err(format!("posting list {i} has a malformed span {a}..{b}"));
            }
            let list = read_posting_list(&post_bytes[a as usize..b as usize])
                .map_err(|e| format!("posting list {i}: {e}"))?;
            posting_count = posting_count.checked_add(list.len()).ok_or("posting count overflowed")?;
            posting.push(list);
        }

        // `12` columnar decode, exactly as the range path does it: one flag-and-varint column
        // per field, an all-zero field one flag byte, exact-consume at the end.
        let mut doc_len = vec![[0u16; crate::index::MAX_FIELD]; doc_count];
        {
            let mut r = Reader::new(&buf[table.doc_len.range()]);
            for f in 0..crate::index::MAX_FIELD {
                let flag = r.b.get(r.p).copied().ok_or("doc_len section ends inside a flag")?;
                r.p += 1;
                if flag == 0xFF {
                    continue;
                }
                for l in doc_len.iter_mut() {
                    *l.get_mut(f).expect("field in range") =
                        u16::try_from(r.varint()?).map_err(|_| "field length exceeds u16")?;
                }
            }
            if r.p != r.b.len() {
                return Err(format!("doc_len section has {} bytes left over", r.b.len() - r.p));
            }
        }

        let mut ix =
            Index::from_parts(schema, alias, &dict_bytes, posting, doc_len, avg_len, doc_count)?;

        // Priors are optional: a zero-length span means "uniform", which is the same thing an
        // index built without them has.
        if table.prior.len > 0 {
            if table.prior.len as usize != doc_count * 4 {
                return Err(format!(
                    "prior section is {} bytes, expected {} for {doc_count} documents",
                    table.prior.len,
                    doc_count * 4
                ));
            }
            let mut r = Reader::new(&buf[table.prior.range()]);
            let mut prior = Vec::with_capacity(doc_count);
            for _ in 0..doc_count {
                prior.push(r.f32()?);
            }
            ix.set_prior(prior);
        }
        if table.first_term.len > 0 {
            if table.first_term.len as usize != doc_count * 4 {
                return Err(format!(
                    "first_term section is {} bytes, expected {} for {doc_count} documents",
                    table.first_term.len,
                    doc_count * 4
                ));
            }
            let mut r = Reader::new(&buf[table.first_term.range()]);
            let mut ft = Vec::with_capacity(doc_count);
            for _ in 0..doc_count {
                ft.push(r.u32()?);
            }
            ix.set_first_term(ft);
        }
        if table.deleted.len > 0 {
            let want = doc_count.div_ceil(64);
            if table.deleted.len as usize != want * 8 {
                return Err(format!(
                    "deleted section is {} bytes, expected {} for {doc_count} documents",
                    table.deleted.len,
                    want * 8
                ));
            }
            let mut r = Reader::new(&buf[table.deleted.range()]);
            let mut d = Vec::with_capacity(want);
            for _ in 0..want {
                d.push(r.u64()?);
            }
            ix.set_deleted(d);
        }
        if table.expansion.len > 0 {
            let mut r = Reader::new(&buf[table.expansion.range()]);
            let n = r.u64()? as usize;
            // Bound the count by what the span can physically hold before allocating for it:
            // each entry writes at least a 4-byte string length and a 4-byte term count.
            if n > table.expansion.len as usize / 8 {
                return Err(format!("expansion claims {n} entries, span holds at most {}", table.expansion.len / 8));
            }
            let mut e = Vec::with_capacity(n);
            for _ in 0..n {
                let value = r.str()?;
                let k = r.u32()? as usize;
                let mut term = Vec::with_capacity(k);
                for _ in 0..k {
                    term.push(r.u32()?);
                }
                e.push((value, term));
            }
            ix.set_expansion(e);
        }
        // Positions. Validated hard, because every failure here is silent: a `position_at` one
        // entry short makes `position_of` return the NEXT posting's run, and the phrase verifier
        // agrees with whatever it is handed. A phrase search that answers confidently from the
        // wrong document is worse than one that refuses to load.
        if table.position_at.len > 0 || table.position.len > 0 {
            if table.position_at.len == 0 {
                return Err("position data with no offset array".into());
            }
            let want = posting_count.checked_add(1).ok_or("posting count overflowed")?;
            // Delta-varint since IDXTEXT9, so the section is no longer a fixed multiple of 8 and
            // the count is written explicitly. Everything else is validated exactly as before:
            // this is the section whose every failure is SILENT, because a short offset array hands
            // the phrase verifier another posting's positions and it agrees with them.
            let mut r = Reader::new(&buf[table.position_at.range()]);
            let n_at = r.varint_u64_count()?;
            if n_at != want {
                return Err(format!(
                    "position_at holds {n_at} offsets, expected {want} for {posting_count} postings"
                ));
            }
            let mut at = Vec::with_capacity(want);
            let mut acc = 0u64;
            for i in 0..want {
                acc = acc.checked_add(r.varint()?).ok_or("position_at overflowed")?;
                // Non-decreasing by construction now (deltas are unsigned), so the remaining
                // check is the upper bound, which a corrupt delta can still violate.
                if i == 0 && acc != 0 {
                    return Err(format!("position_at must start at 0, got {acc}"));
                }
                at.push(acc);
            }

            let mut r = Reader::new(&buf[table.position.range()]);
            let entry = r.varint_u64_count()?;
            if at[want - 1] != entry as u64 {
                return Err(format!(
                    "position_at ends at {} but {entry} positions were written",
                    at[want - 1]
                ));
            }
            let mut pos = Vec::with_capacity(entry);
            for run in at.windows(2) {
                let (lo, hi) = (run[0], run[1]);
                if hi < lo || hi > entry as u64 {
                    return Err(format!("position run {lo}..{hi} is out of order or past {entry}"));
                }
                let mut prev = 0u32;
                for _ in lo..hi {
                    let d = u32::try_from(r.varint()?).map_err(|_| "position delta exceeds u32")?;
                    prev = prev.checked_add(d).ok_or("position overflowed u32")?;
                    pos.push(prev);
                }
            }
            ix.set_position(pos, at);
        }
        // Keys. Validated against `doc_count` rather than trusted, because a short key array would
        // silently shift every key onto the wrong document: `doc_of_key` would then resolve a real
        // key to a real-but-wrong row, and a change stream would update the wrong record. That is a
        // corruption that looks exactly like working software.
        if table.doc_key.len > 0 {
            let mut r = Reader::new(&buf[table.doc_key.range()]);
            let field = r.u32()? as usize;
            if field >= crate::index::MAX_FIELD {
                return Err(format!("key field {field} out of range"));
            }
            let mut key = Vec::with_capacity(doc_count);
            for _ in 0..doc_count {
                key.push(r.str()?);
            }
            ix.set_key(field, key);
        }
        if table.facet_label.len > 0 {
            let mut r = Reader::new(&buf[table.facet_label.range()]);
            // A slot costs at least a u32 field index and a u64 label count on the wire.
            let slot_n = r.u64()? as usize;
            if slot_n > table.facet_label.len as usize / 12 {
                return Err(format!(
                    "facet claims {slot_n} slots, span holds at most {}",
                    table.facet_label.len / 12
                ));
            }
            let mut field = Vec::with_capacity(slot_n);
            let mut label = Vec::with_capacity(slot_n);
            for _ in 0..slot_n {
                field.push(r.u32()? as usize);
                let n = r.u64()? as usize;
                if n > table.facet_label.len as usize / 4 {
                    return Err(format!(
                        "facet slot claims {n} labels, span holds at most {}",
                        table.facet_label.len / 4
                    ));
                }
                let mut l = Vec::with_capacity(n);
                for _ in 0..n {
                    l.push(r.str()?);
                }
                label.push(l);
            }
            // `12` width-coded decode, exactly as the range path does it: each slot declares a
            // byte width whose maximum value is the "no value" sentinel; 0 means every
            // document is absent in that slot.
            let mut r = Reader::new(&buf[table.facet_id.range()]);
            let mut id = Vec::with_capacity(slot_n);
            for _ in 0..slot_n {
                let code = r.u8()?;
                let per_doc = match code {
                    0 => 0usize,
                    1 => 1,
                    2 => 2,
                    4 => 4,
                    other => {
                        return Err(format!("facet slot width {other} is not 0, 1, 2 or 4"));
                    }
                };
                if per_doc as u64 * doc_count as u64 > r.b.len() as u64 - r.p as u64 {
                    return Err(format!(
                        "facet slot needs {} x {doc_count} values, {} bytes left",
                        per_doc,
                        r.b.len() - r.p
                    ));
                }
                let mut column = Vec::with_capacity(doc_count);
                for _ in 0..doc_count {
                    column.push(match per_doc {
                        0 => u32::MAX,
                        1 => {
                            let b = r.u8()?;
                            if b == 0xFF {
                                u32::MAX
                            } else {
                                b as u32
                            }
                        }
                        2 => {
                            let x = r.u16()?;
                            if x == 0xFFFF {
                                u32::MAX
                            } else {
                                x as u32
                            }
                        }
                        _ => r.u32()?,
                    });
                }
                id.push(column);
            }
            ix.set_facet(field, label, id);
        }
        if table.numeric_field.len > 0 {
            let mut r = Reader::new(&buf[table.numeric_field.range()]);
            let slot_n = r.u64()? as usize;
            if slot_n > table.numeric_field.len as usize / 4 {
                return Err(format!(
                    "numeric claims {slot_n} slots, span holds at most {}",
                    table.numeric_field.len / 4
                ));
            }
            let mut field = Vec::with_capacity(slot_n);
            for _ in 0..slot_n {
                field.push(r.u32()? as usize);
            }
            if table.numeric_value.len as usize != slot_n * doc_count * 8 {
                return Err(format!(
                    "numeric value section is {} bytes, expected {} for {slot_n} slots x {doc_count} documents",
                    table.numeric_value.len,
                    slot_n * doc_count * 8
                ));
            }
            let mut r = Reader::new(&buf[table.numeric_value.range()]);
            let mut value = Vec::with_capacity(slot_n);
            for _ in 0..slot_n {
                let mut column = Vec::with_capacity(doc_count);
                for _ in 0..doc_count {
                    column.push(r.f64()?);
                }
                value.push(column);
            }
            ix.set_numeric(field, value);
        }
        Ok(ix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Doc, FacetClause, Hit, IndexBuilder};

    fn built() -> Index {
        let schema = Schema::new(vec![
            Field::new("brand", 3.0, 0.4),
            Field::new("title", 2.0, 0.45),
            Field::new("category", 1.0, 0.75),
        ]);
        let mut b = IndexBuilder::new(schema).with_alias(AliasTable::philippine_grocery());
        for (br, t, c) in [
            ("Coca Cola", "Coca Cola Regular 1.5L", "Inumin"),
            ("Bear Brand", "Bear Brand Powdered Milk 300g", "Gatas"),
            ("Bear Brand", "Bear Brand Powdered Milk 900g", "Gatas"),
            ("Colgate", "Colgate Total Charcoal Deep Clean 80g", "Toothpaste"),
            ("Nescafe", "Nescafe Classic Reseal 200g", "Kape"),
        ] {
            b.add(&Doc::new([br, t, c]));
        }
        b.build().unwrap()
    }

    /// The property that matters: a loaded index answers *identically*, not approximately.
    #[test]
    fn round_trip_preserves_every_answer() {
        let a = built();
        let bytes = a.to_bytes();
        let b = Index::from_bytes(&bytes).expect("load");

        assert_eq!(a.doc_count(), b.doc_count());
        assert_eq!(a.term_count(), b.term_count());
        for q in [
            "coca cola 1.5l",
            "bear brand milk 300g",
            "colgaye",
            "nescaffe classic",
            "gatas",
            "kape",
            "qzxwv nothing",
        ] {
            assert_eq!(a.search(q, 10), b.search(q, 10), "query {q:?} differs after round trip");
        }
    }

    /// Both posting modes round-trip EXACTLY, including a MULTI-BLOCK columnar list. The two
    /// modes branch on list length, and the columnar layout is column-major — an earlier draft
    /// wrote it block-major, which agrees on every single-block list (≤ `POST_BLOCK` postings)
    /// and silently corrupts the first multi-block one. The smallest corpus in the suite could
    /// not build a list that long, so this one synthesizes the boundary sizes: 63 (sparse),
    /// 64 (columnar, one block), 129 and 300 (columnar, multi-block).
    #[test]
    fn posting_round_trip_at_both_mode_boundaries_and_across_blocks() {
        for shared in [63usize, 64, 129, 300] {
            let schema = Schema::new(vec![Field::new("name", 1.0, 0.4)]);
            let mut b = IndexBuilder::new(schema);
            for d in 0..shared {
                // A token unique to each document, plus "common" in every one — the latter
                // builds the posting list whose length selects and stresses the mode.
                b.add(&Doc::new([format!("unique{d} common")]));
            }
            let a = b.build().unwrap();
            let bytes = a.to_bytes();
            let loaded = Index::from_bytes(&bytes)
                .unwrap_or_else(|e| panic!("reload at shared={shared}: {e}"));
            assert_eq!(loaded.search("common", shared), a.search("common", shared));
            for d in 0..shared {
                let q = format!("unique{d}");
                assert_eq!(loaded.search(&q, 5), a.search(&q, 5));
            }
        }
    }

    #[test]
    fn schema_and_alias_survive() {
        let a = built();
        let b = Index::from_bytes(&a.to_bytes()).unwrap();
        assert_eq!(a.schema().field.len(), b.schema().field.len());
        for (x, y) in a.schema().field.iter().zip(b.schema().field.iter()) {
            assert_eq!(x.name, y.name);
            assert_eq!(x.boost, y.boost);
            assert_eq!(x.b, y.b);
        }
        assert_eq!(a.schema().k1, b.schema().k1);
        // The alias table must survive, or Filipino queries silently stop resolving.
        assert!(!b.search("gatas", 5).is_empty(), "alias table did not survive serialization");
    }

    /// Serialization is byte-for-byte stable, so an index can be content-hashed and cached
    /// immutably — the fix profstopick needed after serving a stale shard to returning students.
    #[test]
    fn serialization_is_deterministic() {
        assert_eq!(built().to_bytes(), built().to_bytes());
    }

    /// The range-read property, demonstrated rather than asserted in prose: one posting list is
    /// located and decoded from its own byte range, touching none of the rest of the file.
    #[test]
    fn a_single_posting_list_is_range_readable() {
        let ix = built();
        let bytes = ix.to_bytes();
        let head = &bytes[..MAGIC.len() + TABLE_BYTE];
        let table = read_section_table(head).unwrap();

        // A reader fetches only the offset array...
        let offsets = &bytes[table.posting_offset.range()];
        let term_id = ix.term_id_of("colgate").expect("term present");
        let span = posting_span(&table, offsets, term_id).unwrap();

        // ...then only that one list. It carries its own count, so it decodes standalone --
        // which is the property a fixed-width posting list used to get from its stride.
        assert!(span.len > 0, "colgate must have a non-empty posting list");
        let slice = &bytes[span.range()];
        let list = read_posting_list(slice).expect("a fetched list decodes on its own");

        assert_eq!(list.len(), 1, "colgate appears in one document");
        assert_eq!(list[0].0, 3, "colgate appears in document 3");
        assert_eq!(ix.search("colgate", 1)[0].doc, 3, "and the engine agrees");

        // And the bytes actually touched are a small fraction of the file.
        let touched = head.len() + offsets.len() + slice.len();
        assert!(
            touched < bytes.len(),
            "a single-term read touched {touched} of {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn corrupt_input_errors_rather_than_panics() {
        let good = built().to_bytes();
        assert!(Index::from_bytes(&[]).is_err());
        assert!(Index::from_bytes(b"NOTANIDX").is_err());
        // Truncation at every 1/8th of the file must be handled, not panic.
        for frac in 1..8 {
            let cut = good.len() * frac / 8;
            let _ = Index::from_bytes(&good[..cut]);
        }
        // A valid header with a garbage body.
        let mut bad = good.clone();
        let n = bad.len();
        for b in bad[n / 2..].iter_mut() {
            *b = 0xFF;
        }
        let _ = Index::from_bytes(&bad);
    }

    /// The columnar mode's own failure modes, each of which would otherwise be a confident wrong
    /// answer inside one term's list: a column kind the writer never emits, a block wider than
    /// any value it could legally hold, a block whose payload runs past its span, and a count
    /// the span is too small to hold even at the columnar floor. Built by hand at the byte level
    /// so each case is exact rather than lucky.
    #[test]
    fn columnar_posting_corruption_is_rejected_loudly() {
        // A legal 64-posting list, encoded the way the writer does, as the base to corrupt.
        let encode = |n: u32| -> Vec<u8> {
            let mut w = Writer::new();
            w.varint(n as u64);
            w.buf.push(COL_PACKED);
            let deltas: Vec<u64> = (0..n as u64).map(|i| if i == 0 { 7 } else { 1 }).collect();
            w.buf.push(column_bits(&deltas));
            block_pack(&mut w.buf, &deltas, column_bits(&deltas));
            for _ in 0..crate::index::MAX_FIELD {
                w.buf.push(COL_ALL_ZERO);
            }
            w.buf
        };
        let good = encode(64);
        assert!(read_posting_list(&good).is_ok(), "the hand-encoded list must itself be valid");

        // A column kind that is neither 0x00 nor 0xFF.
        let mut bad = good.clone();
        bad[1] = 0x42;
        assert!(read_posting_list(&bad).is_err(), "unknown column kind must be refused");

        // A block width of 33 bits: more than any u32 delta needs, so always corrupt.
        let mut bad = good.clone();
        bad[2] = 33;
        assert!(read_posting_list(&bad).is_err(), "33-bit block must be refused");

        // A truncated block payload: the width says 8 bits for 64 values, one byte is missing.
        let mut bad = good.clone();
        bad.pop();
        assert!(read_posting_list(&bad).is_err(), "short block payload must be refused");

        // A count the span cannot hold even at the columnar floor (ceil(n/8) + columns): 64
        // postings need at least 13 bytes of columns, and this span has 10.
        let mut short = encode(64);
        short.truncate(10);
        assert!(read_posting_list(&short).is_err(), "count below the columnar floor must be refused");
    }

    #[test]
    fn every_offset_in_the_table_is_u64() {
        // The section table is 18 spans of two u64s. If this size ever changes, the format version
        // in MAGIC must change with it — it went 7 -> 8 and `IDXTEXT1` -> `IDXTEXT2` when static
        // priors were added, 11 -> 13 and `IDXTEXT2` -> `IDXTEXT3` when facets were, and 15 -> 17
        // and `IDXTEXT5` -> `IDXTEXT6` when token positions were, and 17 -> 18 and `IDXTEXT6` ->
        // `IDXTEXT7` when document keys were, because a reader expecting the old count mis-parses
        // the extra offsets as posting data and fails as garbage results rather than as an error.
        //
        // **This assertion is the mechanism.** It failed on the facet change and again on the
        // position change, and that is what forced each magic bump; without it the format would
        // have grown silently.
        assert_eq!(TABLE_BYTE, 288);
        let bytes = built().to_bytes();
        let t = read_section_table(&bytes).unwrap();
        // Sections appear in ascending order and tile the file without gaps after the head.
        let mut at = (MAGIC.len() + TABLE_BYTE) as u64;
        for s in [
            t.meta,
            t.schema,
            t.alias,
            t.dict,
            t.posting_offset,
            t.posting,
            t.doc_len,
            t.prior,
            t.first_term,
            t.deleted,
            t.expansion,
            t.facet_label,
            t.facet_id,
            t.numeric_field,
            t.numeric_value,
            t.position_at,
            t.position,
            t.doc_key,
        ] {
            assert_eq!(s.offset, at, "sections must tile contiguously");
            at += s.len;
        }
        assert_eq!(at as usize, bytes.len(), "the last section must end at EOF");
    }

    #[test]
    fn prefix_anchoring_survives_a_round_trip() {
        // Anchoring is a RANKING signal, so an artifact that dropped it would rank worse than the
        // index it was built from -- and the symptom would be "search got slightly worse after we
        // deployed", which is close to undebuggable. Hence a round-trip assertion, not a size one.
        let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
        let mut b = IndexBuilder::new(schema);
        b.add(&Doc::new(["del carmen"]));
        b.add(&Doc::new(["san carlos del norte"]));
        b.add(&Doc::new(["carmen del sur"]));
        let ix = b.build().unwrap();

        // "del c" anchors on `del`, so the document that STARTS with it must win.
        let before: Vec<u32> = ix.search_prefix("del c", 3).iter().map(|h| h.doc).collect();
        assert_eq!(before[0], 0, "the document beginning with the query must rank first");

        let round = Index::from_bytes(&ix.to_bytes()).unwrap();
        let after: Vec<u32> = round.search_prefix("del c", 3).iter().map(|h| h.doc).collect();
        assert_eq!(after, before, "serialization must not change the order");
    }

    #[test]
    fn a_learned_expansion_survives_a_round_trip() {
        let schema = Schema::new(vec![
            Field::new("name", 3.0, 0.5),
            Field::new("cat", 0.0, 0.75),
        ]);
        let mut b = IndexBuilder::new(schema).learn_expansion(1, 8);
        for n in ["camote powder", "sago tapioca", "cream tartar", "hotcake mix", "mung beans"] {
            b.add(&Doc::new([n, "baking needs"]));
        }
        for n in ["baking tray steel", "baking sheet paper"] {
            b.add(&Doc::new([n, "kitchen tools"]));
        }
        let ix = b.build().unwrap();
        assert!(ix.expansion_count() > 0);
        let before: Vec<u32> = ix.search("baking needs", 5).iter().map(|h| h.doc).collect();

        let round = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert_eq!(round.expansion_count(), ix.expansion_count());
        assert_eq!(round.expansion_of("baking needs"), ix.expansion_of("baking needs"));
        let after: Vec<u32> = round.search("baking needs", 5).iter().map(|h| h.doc).collect();
        assert_eq!(after, before, "serialization must not change what a facet query returns");
    }

    #[test]
    fn facets_survive_a_round_trip() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
        ]))
        .with_facet(1);
        for (name, brand) in [
            ("Colgate Total Toothpaste 150g", "Colgate"),
            ("Colgate Fresh Gel 100g", "Colgate"),
            ("Safeguard Pure White Soap 135g", "Safeguard"),
            ("Lucky Me Pancit Canton 60g", "Lucky Me"),
        ] {
            b.add(&Doc::new(vec![name, brand]));
        }
        let ix = b.build().unwrap();

        // Values are stored VERBATIM: "Lucky Me" is one facet, not two terms.
        assert_eq!(ix.facet_label(), ["Colgate", "Lucky Me", "Safeguard"]);
        assert_eq!(ix.facet_of(0), Some("Colgate"));
        assert_eq!(ix.facet_of(3), Some("Lucky Me"));

        let before_tally = ix.facet_tally("Colgate");
        let before_filtered: Vec<u32> =
            ix.search_facet("Colgate", 10, "Colgate").iter().map(|h| h.doc).collect();
        assert_eq!(before_tally.first().map(|x| x.1), Some(2), "both Colgate rows counted");
        assert_eq!(before_filtered.len(), 2);

        let back = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert_eq!(back.facet_label(), ix.facet_label(), "labels survive");
        for d in 0..ix.doc_count() as u32 {
            assert_eq!(back.facet_of(d), ix.facet_of(d), "doc {d} facet survives");
        }
        assert_eq!(back.facet_tally("Colgate"), before_tally, "tally survives");
        let after: Vec<u32> =
            back.search_facet("Colgate", 10, "Colgate").iter().map(|h| h.doc).collect();
        assert_eq!(after, before_filtered, "filtered search survives");

        // An unknown value is empty, not everything -- the failure mode that matters for a filter.
        assert!(back.search_facet("Colgate", 10, "Nestle").is_empty());
    }

    #[test]
    fn two_facet_slots_filter_conjunctively_and_survive_a_round_trip() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
            Field::new("category", 0.0, 0.6),
        ]))
        .with_facet(1)
        .with_facet(2);
        for (name, brand, cat) in [
            ("Colgate Total Toothpaste 150g", "Colgate", "Oral Care"),
            ("Colgate Mouthwash 500ml", "Colgate", "Oral Care"),
            ("Colgate Toothbrush Soft", "Colgate", "Accessory"),
            ("Oral B Toothbrush Medium", "Oral B", "Accessory"),
        ] {
            b.add(&Doc::new(vec![name, brand, cat]));
        }
        let ix = b.build().unwrap();

        assert_eq!(ix.facet_slot_count(), 2);
        assert_eq!(ix.facet_field(), [1, 2]);
        assert_eq!(ix.facet_label_at(0), ["Colgate", "Oral B"]);
        assert_eq!(ix.facet_label_at(1), ["Accessory", "Oral Care"]);
        assert_eq!(ix.facet_of_at(0, 1), Some("Oral Care"));

        // A query that reaches BOTH brands, so each filter alone is strictly larger than the
        // conjunction. "Colgate" alone never matches the Oral B row, which would make the
        // conjunction equal to one of its own operands and demonstrate nothing.
        let q = "Colgate Toothbrush";

        // The point of the feature: brand AND category, in one pass.
        let both: Vec<u32> =
            ix.search_facet_all(q, 10, &[(0, "Colgate"), (1, "Accessory")]).iter().map(|h| h.doc).collect();
        assert_eq!(both, vec![2], "only the Colgate accessory satisfies both");

        // Each filter alone is strictly larger, which is what makes the conjunction meaningful.
        assert_eq!(ix.search_facet_at(q, 10, 0, "Colgate").len(), 3);
        assert_eq!(ix.search_facet_at(q, 10, 1, "Accessory").len(), 2);

        // An unsatisfiable pair is empty, NOT everything -- the dangerous reading of a filter.
        assert!(ix.search_facet_all(q, 10, &[(0, "Oral B"), (1, "Oral Care")]).is_empty());
        assert!(ix.search_facet_all(q, 10, &[(0, "Nestle")]).is_empty());
        assert!(ix.search_facet_all(q, 10, &[(9, "Colgate")]).is_empty(), "bad slot");

        let t0 = ix.facet_tally_at(q, 0);
        let t1 = ix.facet_tally_at(q, 1);
        assert_eq!(t0, vec![("Colgate", 3), ("Oral B", 1)]);
        assert_eq!(t1, vec![("Accessory", 2), ("Oral Care", 2)]);

        let back = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert_eq!(back.facet_field(), ix.facet_field(), "slot -> field mapping survives");
        assert_eq!(back.facet_slot_count(), 2);
        for slot in 0..2 {
            assert_eq!(back.facet_label_at(slot), ix.facet_label_at(slot));
            for d in 0..ix.doc_count() as u32 {
                assert_eq!(back.facet_of_at(d, slot), ix.facet_of_at(d, slot));
            }
            assert_eq!(back.facet_tally_at(q, slot), ix.facet_tally_at(q, slot));
        }
        let after: Vec<u32> = back
            .search_facet_all(q, 10, &[(0, "Colgate"), (1, "Accessory")])
            .iter()
            .map(|h| h.doc)
            .collect();
        assert_eq!(after, both, "the conjunction survives a round trip");
    }

    #[test]
    fn a_numeric_range_filters_tallies_and_survives_a_round_trip() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
            Field::new("size", 0.0, 0.6),
        ]))
        .with_facet(1)
        .with_numeric(2);
        for (name, brand, size) in [
            ("Colgate Toothpaste Small", "Colgate", "50"),
            ("Colgate Toothpaste Medium", "Colgate", "100"),
            ("Colgate Toothpaste Large", "Colgate", "150"),
            ("Colgate Toothpaste Family", "Colgate", "300"),
            ("Colgate Toothpaste Sample", "Colgate", "not a number"),
        ] {
            b.add(&Doc::new(vec![name, brand, size]));
        }
        let ix = b.build().unwrap();
        let q = "Colgate Toothpaste";

        assert_eq!(ix.numeric_slot_count(), 1);
        assert_eq!(ix.numeric_field(), [2]);
        assert_eq!(ix.numeric_of(0, 0), Some(50.0));
        assert_eq!(ix.numeric_of(4, 0), None, "unparseable text has no value");

        // Half-open: [100, 300) is docs 1 and 2, and NOT doc 3 at exactly 300.
        let mid: Vec<u32> =
            ix.search_range(q, 10, 0, 100.0, 300.0).iter().map(|h| h.doc).collect();
        let mut sorted = mid.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2], "300 is excluded by the open upper bound");

        // The document with no value is in no range at all, including a maximal one.
        let all: Vec<u32> =
            ix.search_range(q, 10, 0, f64::MIN, f64::MAX).iter().map(|h| h.doc).collect();
        assert!(!all.contains(&4), "an absent value is excluded, not treated as zero");
        assert_eq!(all.len(), 4);

        // A histogram must partition: adjacent buckets share a boundary and must not double count.
        let edge = [0.0, 100.0, 200.0, 400.0];
        let hist = ix.range_tally(q, 0, &edge);
        assert_eq!(hist, vec![1, 2, 1], "50 | 100,150 | 300");
        assert_eq!(hist.iter().sum::<usize>(), 4, "the absent value is counted nowhere");

        // Facet AND range in one pass.
        let both: Vec<u32> = ix
            .search_filtered(q, 10, &[(0, "Colgate")], &[(0, 100.0, 300.0)])
            .iter()
            .map(|h| h.doc)
            .collect();
        let mut bs = both.clone();
        bs.sort_unstable();
        assert_eq!(bs, vec![1, 2]);
        assert!(
            ix.search_filtered(q, 10, &[(0, "Nestle")], &[(0, 0.0, 999.0)]).is_empty(),
            "an unsatisfiable facet still empties the whole filter"
        );
        assert!(ix.search_range(q, 10, 9, 0.0, 1.0).is_empty(), "unknown numeric slot is empty");

        let back = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert_eq!(back.numeric_field(), ix.numeric_field());
        for d in 0..ix.doc_count() as u32 {
            assert_eq!(back.numeric_of(d, 0), ix.numeric_of(d, 0), "doc {d} value survives");
        }
        assert_eq!(back.range_tally(q, 0, &edge), hist, "the histogram survives");
        let after: Vec<u32> =
            back.search_range(q, 10, 0, 100.0, 300.0).iter().map(|h| h.doc).collect();
        assert_eq!(after, mid, "the range filter survives, in the same order");
    }

    #[test]
    fn sorting_by_a_numeric_column_orders_filters_and_excludes_absent_values() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
            Field::new("size", 0.0, 0.6),
        ]))
        .with_facet(1)
        .with_numeric(2);
        for (name, brand, size) in [
            ("Colgate Toothpaste Large", "Colgate", "150"),
            ("Colgate Toothpaste Small", "Colgate", "50"),
            ("Colgate Toothpaste Family", "Colgate", "300"),
            ("Oral B Toothpaste Medium", "Oral B", "100"),
            ("Colgate Toothpaste Sample", "Colgate", ""),
        ] {
            b.add(&Doc::new(vec![name, brand, size]));
        }
        let ix = b.build().unwrap();
        let q = "Toothpaste";

        // Ascending is by VALUE, not by relevance: doc 1 (50) first even though the corpus and the
        // relevance order both start elsewhere.
        let asc: Vec<u32> = ix.search_sorted(q, 10, 0, true).iter().map(|h| h.doc).collect();
        assert_eq!(asc, vec![1, 3, 0, 2], "50, 100, 150, 300");

        let desc: Vec<u32> = ix.search_sorted(q, 10, 0, false).iter().map(|h| h.doc).collect();
        let mut reversed = asc.clone();
        reversed.reverse();
        assert_eq!(desc, reversed, "descending is the exact reverse here, all values distinct");

        // The row with no size has no position in a price order.
        assert!(!asc.contains(&4), "an absent value is excluded, not sorted as zero");

        // `k` truncates AFTER ordering, so it returns the k cheapest, not k arbitrary matches.
        assert_eq!(
            ix.search_sorted(q, 2, 0, true).iter().map(|h| h.doc).collect::<Vec<_>>(),
            vec![1, 3]
        );

        // Score and bucket are still populated honestly even though they do not order the result.
        assert!(ix.search_sorted(q, 10, 0, true).iter().all(|h| h.score > 0.0));

        // The filter bar applies first, then the sort.
        let filtered: Vec<u32> = ix
            .search_sorted_filtered(q, 10, 0, true, &[(0, "Colgate")], &[(0, 0.0, 200.0)])
            .iter()
            .map(|h| h.doc)
            .collect();
        assert_eq!(filtered, vec![1, 0], "Colgate only, under 200, cheapest first");
        assert!(
            ix.search_sorted_filtered(q, 10, 0, true, &[(0, "Nestle")], &[]).is_empty(),
            "an unsatisfiable filter is empty, not unsorted-everything"
        );
        assert!(ix.search_sorted(q, 10, 9, true).is_empty(), "unknown slot is empty");
        assert!(ix.search_sorted(q, 0, 0, true).is_empty(), "k = 0 is empty");

        // Survives a round trip like every other query capability.
        let back = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert_eq!(
            back.search_sorted(q, 10, 0, true).iter().map(|h| h.doc).collect::<Vec<_>>(),
            asc
        );
    }

    #[test]
    fn sorting_ties_break_by_rank_then_doc() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("size", 0.0, 0.6),
        ]))
        .with_numeric(1);
        // Three documents at the SAME value: the order among them must be deterministic and must
        // be the relevance order, not insertion order.
        b.add(&Doc::new(vec!["Toothpaste Colgate Mint Fresh Clean", "100"]));
        b.add(&Doc::new(vec!["Toothpaste", "100"]));
        b.add(&Doc::new(vec!["Toothpaste Colgate", "100"]));
        let ix = b.build().unwrap();

        let a: Vec<u32> = ix.search_sorted("Toothpaste", 10, 0, true).iter().map(|h| h.doc).collect();
        let b2: Vec<u32> = ix.search_sorted("Toothpaste", 10, 0, true).iter().map(|h| h.doc).collect();
        assert_eq!(a, b2, "a tie order is stable across runs");
        assert_eq!(a.len(), 3);
        // The shortest field scores highest under BM25 length normalisation, so doc 1 leads.
        assert_eq!(a[0], 1, "among equal values, the best match comes first");
    }

    #[test]
    fn an_index_without_numeric_columns_costs_no_numeric_bytes() {
        let ix = built();
        let t = read_section_table(&ix.to_bytes()).unwrap();
        assert_eq!(t.numeric_field.len, 0);
        assert_eq!(t.numeric_value.len, 0);
        assert_eq!(ix.numeric_slot_count(), 0);
        assert!(ix.range_tally("anything", 0, &[0.0, 1.0]).is_empty());
    }

    /// OR inside a clause, AND across clauses, NOT, and the two opposite unknown-value rules.
    ///
    /// The unknown-value rules are the dangerous part: getting them backwards makes a filter
    /// silently return the whole corpus, which looks like a working search.
    #[test]
    fn facet_clauses_or_within_and_across_and_negate() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
            Field::new("kind", 0.0, 0.6),
        ]))
        .with_facet(1)
        .with_facet(2);
        for (n, brand, kind) in [
            ("Colgate Toothpaste Large", "Colgate", "paste"),
            ("Oral B Toothpaste Mini", "Oral B", "paste"),
            ("Aquafresh Toothpaste Twin", "Aquafresh", "paste"),
            ("Colgate Toothbrush Soft", "Colgate", "brush"),
        ] {
            b.add(&Doc::new(vec![n, brand, kind]));
        }
        let ix = b.build().unwrap();
        let q = "Toothpaste Toothbrush";
        let docs = |h: Vec<Hit>| {
            let mut d: Vec<u32> = h.iter().map(|x| x.doc).collect();
            d.sort_unstable();
            d
        };

        // OR within a clause widens.
        assert_eq!(
            docs(ix.search_any(q, 10, &[FacetClause::any(0, &["Colgate", "Oral B"])])),
            vec![0, 1, 3]
        );
        // AND across clauses narrows.
        assert_eq!(
            docs(ix.search_any(
                q,
                10,
                &[FacetClause::any(0, &["Colgate", "Oral B"]), FacetClause::any(1, &["paste"])]
            )),
            vec![0, 1]
        );
        // NOT.
        assert_eq!(docs(ix.search_any(q, 10, &[FacetClause::none(0, &["Colgate"])])), vec![1, 2]);
        // NOT combined with OR inside the negated clause.
        assert_eq!(
            docs(ix.search_any(q, 10, &[FacetClause::none(0, &["Colgate", "Oral B"])])),
            vec![2]
        );

        // The two opposite rules for unknown values.
        assert!(
            ix.search_any(q, 10, &[FacetClause::any(0, &["Nestle"])]).is_empty(),
            "include with every value unknown matches NOTHING"
        );
        assert_eq!(
            docs(ix.search_any(q, 10, &[FacetClause::none(0, &["Nestle"])])),
            vec![0, 1, 2, 3],
            "exclude with every value unknown excludes NOTHING"
        );
        // A partially-known include keeps the known part rather than failing.
        assert_eq!(
            docs(ix.search_any(q, 10, &[FacetClause::any(0, &["Nestle", "Colgate"])])),
            vec![0, 3]
        );
        // An unknown SLOT is empty, not unfiltered -- the dangerous reading of a bad filter.
        // A slot the index does not have takes the SAME two answers as an unknown value, not a
        // blanket empty -- otherwise "not discontinued" deletes every row from a segment that was
        // built before the slot existed.
        assert!(ix.search_any(q, 10, &[FacetClause::any(9, &["Colgate"])]).is_empty());
        assert_eq!(
            docs(ix.search_any(q, 10, &[FacetClause::none(9, &["Colgate"])])),
            docs(ix.search(q, 10)),
            "an exclude on a slot that does not exist removes nothing"
        );
    }

    /// A document with no value in the slot is kept by an exclude and dropped by an include.
    #[test]
    fn a_missing_facet_value_is_kept_by_not_and_dropped_by_any() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
        ]))
        .with_facet(1);
        b.add(&Doc::new(vec!["Colgate Toothpaste", "Colgate"]));
        b.add(&Doc::new(vec!["Generic Toothpaste", ""])); // no brand at all
        let ix = b.build().unwrap();

        let d = |h: Vec<Hit>| h.iter().map(|x| x.doc).collect::<Vec<_>>();
        assert_eq!(d(ix.search_any("Toothpaste", 10, &[FacetClause::any(0, &["Colgate"])])), vec![0]);
        assert_eq!(
            d(ix.search_any("Toothpaste", 10, &[FacetClause::none(0, &["Colgate"])])),
            vec![1],
            "an unbranded row survives \"not Colgate\""
        );
    }

    /// Paging must partition the ranking: page 0 then page 1 equals a single longer request.
    /// Phrase queries: the words consecutive, in order, in ONE field.
    ///
    /// The corpus is chosen so a bag-of-words search cannot tell the cases apart -- every document
    /// contains every word of the phrase. Only adjacency separates them, which is the whole claim.
    #[test]
    fn a_phrase_matches_only_consecutive_tokens_in_one_field() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("brand", 1.0, 0.6),
        ]))
        .with_position();
        b.add(&Doc::new(["Vanilla Ice Cream Tub", "Selecta"])); // 0: the phrase
        b.add(&Doc::new(["Ice Crushed Cream Soda", "Selecta"])); // 1: both words, not adjacent
        b.add(&Doc::new(["Cream Ice Bar", "Selecta"])); // 2: both words, wrong order
        b.add(&Doc::new(["Chocolate Ice", "Cream Co"])); // 3: adjacent ACROSS a field boundary
        let ix = b.build().unwrap();

        let docs = |h: Vec<Hit>| h.iter().map(|x| x.doc).collect::<Vec<_>>();

        // Every one of the four contains both words, so the plain search returns all of them.
        assert_eq!(docs(ix.search("Ice Cream", 10)).len(), 4, "all four match as a bag of words");

        assert_eq!(
            docs(ix.search_phrase("Ice Cream", 10)),
            vec![0],
            "only the document with the words adjacent and in order matches"
        );
        assert!(
            docs(ix.search_phrase("Cream Ice", 10)).contains(&2),
            "the reversed phrase matches the reversed document"
        );
        assert!(
            !docs(ix.search_phrase("Ice Cream", 10)).contains(&3),
            "a phrase must not span a field boundary: name ending 'Ice' plus brand starting 'Cream'"
        );

        // A word no document has cannot be part of any phrase, and the safety direction is the
        // same as an all-unknown include: nothing, never everything.
        assert!(ix.search_phrase("Ice Sorbet", 10).is_empty(), "an unknown word matches nothing");
        assert!(ix.search_phrase("", 10).is_empty(), "an empty phrase is not a match-everything");

        // A single-token phrase is an ordinary term query, not a special case.
        assert_eq!(
            docs(ix.search_phrase("Vanilla", 10)),
            docs(ix.search("Vanilla", 10)),
            "a one-word phrase agrees with a one-word search"
        );

        // The phrase constrains but does not re-rank: it is a filter, like a facet clause.
        let phrase = ix.search_phrase("Ice Cream", 10);
        let plain: Vec<Hit> = ix.search("Ice Cream", 10).into_iter().filter(|h| h.doc == 0).collect();
        assert_eq!(phrase, plain, "a phrase hit keeps the score the same query gave it");
    }

    /// Positions must survive a round trip, and an index WITHOUT them must keep working.
    #[test]
    fn positions_round_trip_and_are_optional() {
        let row = ["Vanilla Ice Cream Tub", "Ice Crushed Cream Soda", "Cream Ice Bar"];

        let mut with = IndexBuilder::new(Schema::new(vec![Field::new("name", 3.0, 0.4)]))
            .with_position();
        let mut without = IndexBuilder::new(Schema::new(vec![Field::new("name", 3.0, 0.4)]));
        for r in row {
            with.add(&Doc::new([r]));
            without.add(&Doc::new([r]));
        }
        let with = with.build().unwrap();
        let without = without.build().unwrap();

        assert!(with.has_position());
        assert!(!without.has_position());
        // An index built without positions REFUSES the query rather than degrading to a
        // bag-of-words match that would look like a working phrase search.
        assert!(without.search_phrase("Ice Cream", 10).is_empty());
        assert_eq!(without.search("Ice Cream", 10).len(), 3, "ordinary search is unaffected");

        let bytes = with.to_bytes();
        let back = Index::from_bytes(&bytes).unwrap();
        assert!(back.has_position(), "the position spans survive serialization");
        assert_eq!(
            back.search_phrase("Ice Cream", 10),
            with.search_phrase("Ice Cream", 10),
            "the reopened index answers the phrase identically"
        );

        // The cost is stated rather than assumed: positions are the only build option that grows
        // with token occurrences instead of with documents.
        let plain = without.to_bytes();
        assert!(
            bytes.len() > plain.len(),
            "positions cost bytes: {} with, {} without",
            bytes.len(),
            plain.len()
        );
    }

    /// **`eff` must BE the final comparator, not merely resemble it.**
    ///
    /// `p47` sized the scoring pool down to `k` and ungated the block skip, and both are sound only
    /// because the ranking pool provably holds the top `k` by the final ordering. That proof rests
    /// entirely on `eff = canon_score(score) - bucket * bucket_scale` inducing exactly the order
    /// `rank_cmp` does.
    ///
    /// It did not, before `p47`: `eff` used the RAW score while `rank_cmp` compares the quantized
    /// one, so two documents whose scores differ but quantize equal were ordered by score in the
    /// pool and by document id in the answer. This pins the property so that gap cannot reopen
    /// silently -- if it does, a pruning bound stops being an upper bound.
    #[test]
    fn eff_induces_exactly_the_final_ranking() {
        use crate::index::canon_score;

        // Scores deliberately include pairs that differ by less than the quantization grid, which
        // is the only case where the two orders could ever disagree.
        let base = 3.25f32;
        let mut score = vec![0.0f32, 0.5, 1.0, base, base + f32::EPSILON, base + 2.0 * f32::EPSILON];
        score.push(base * (1.0 + 1e-7));
        let bucket = [0u32, 1, 3, 7];
        // Larger than any score here, which is the condition the real `bucket_scale` satisfies.
        let bucket_scale = 100.0f32;
        let eff = |h: &Hit| canon_score(h.score) - h.typo_bucket as f32 * bucket_scale;

        let mut hit: Vec<Hit> = Vec::new();
        for (i, &s) in score.iter().enumerate() {
            for (j, &b) in bucket.iter().enumerate() {
                hit.push(Hit { doc: (i * bucket.len() + j) as u32, score: s, typo_bucket: b });
            }
        }

        for a in &hit {
            for b in &hit {
                if a.doc == b.doc {
                    continue;
                }
                let by_rank = crate::index::rank_cmp(a, b);
                let by_eff = eff(b).total_cmp(&eff(a)).then(a.doc.cmp(&b.doc));
                assert_eq!(
                    by_rank, by_eff,
                    "eff and rank_cmp disagree on {a:?} vs {b:?} (eff {} vs {})",
                    eff(a),
                    eff(b)
                );
            }
        }
    }

    /// **The differential test that makes two sort arms safe to ship.**
    ///
    /// `search_sorted` now picks between a posting scan and a value-order walk on a cost estimate.
    /// Two implementations of one answer is a bug factory unless they are held to being the SAME
    /// answer, so both are forced over the same queries and required to agree document for
    /// document, score for score, bucket for bucket.
    ///
    /// The corpus is built for the case that separates them: heavy ties in the numeric column, so
    /// a walk that stopped at exactly `k` would return an arbitrary subset of the boundary group
    /// and still look plausible.
    #[test]
    fn sorted_arms_agree_document_for_document() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("price", 0.0, 0.6),
        ]))
        .with_numeric(1);
        // Three price levels over sixty rows: every k lands inside a tie group.
        for i in 0..60 {
            let price = [10.0, 20.0, 30.0][i % 3];
            let name = match i % 4 {
                0 => format!("Colgate Toothpaste Variant {i}"),
                1 => format!("Colgate Total Toothpaste {i}"),
                2 => format!("Oral B Toothbrush {i}"),
                _ => format!("Colgate Toothpaste Gel Extra Long Name Number {i}"),
            };
            b.add(&Doc::new([name, format!("{price}")]));
        }
        // A row with no price at all: it has no position in a price order, in either arm.
        b.add(&Doc::new(["Colgate Toothpaste Unpriced", "not a number"]));
        let ix = b.build().unwrap();

        for q in ["Colgate", "Toothpaste", "Colgate Toothpaste", "Colgte", "Oral B", "qzxwv"] {
            for k in [1usize, 2, 3, 5, 10, 25, 100] {
                for asc in [true, false] {
                    let walk = ix.search_sorted_arm(q, k, 0, asc, true);
                    let scan = ix.search_sorted_arm(q, k, 0, asc, false);
                    assert_eq!(
                        walk, scan,
                        "arms disagree on q={q:?} k={k} ascending={asc}\n walk={walk:?}\n scan={scan:?}"
                    );
                    // And the public entry point must return whichever it chose, unchanged.
                    assert_eq!(ix.search_sorted(q, k, 0, asc), scan, "the chooser changed the answer");
                }
            }
        }

        // The stopping rule is the point: the walk must not return more than k, and must return
        // exactly k when that many priced matches exist.
        assert_eq!(ix.search_sorted_arm("Colgate", 5, 0, true, true).len(), 5);

        // The filtered entry point takes the same fork, so the filter must survive it. Checked
        // against the unfiltered answer narrowed by hand rather than against the other arm, so
        // this cannot pass by both arms being wrong the same way.
        let ranged = ix.search_sorted_filtered("Colgate", 50, 0, true, &[], &[(0, 10.0, 25.0)]);
        let by_hand: Vec<Hit> = ix
            .search_sorted("Colgate", 1000, 0, true)
            .into_iter()
            .filter(|h| ix.numeric_of(h.doc, 0).is_some_and(|v| (10.0..25.0).contains(&v)))
            .collect();
        assert_eq!(ranged, by_hand, "the range filter survives whichever arm was chosen");
        assert!(
            ix.search_sorted("Colgate Toothpaste Unpriced", 10, 0, true)
                .iter()
                .all(|h| h.doc != 60),
            "a row with no value is excluded from the order by both arms"
        );
    }

    /// Keys must survive a round trip, and an index without them must cost nothing.
    #[test]
    fn keys_round_trip_and_are_optional() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("sku", 0.0, 0.6),
            Field::new("name", 3.0, 0.4),
        ]))
        .with_key(0);
        for (sku, name) in [
            ("sku-1", "Colgate Total Toothpaste 150g"),
            ("sku-2", "Aquafresh Mini Toothpaste 50g"),
            ("", "Unkeyed Toothpaste"),
        ] {
            b.add(&Doc::new([sku, name]));
        }
        let ix = b.build().unwrap();
        let back = Index::from_bytes(&ix.to_bytes()).unwrap();

        assert!(back.has_key());
        assert_eq!(back.key_field(), Some(0));
        assert_eq!(back.keyed_count(), 2);
        assert_eq!(back.doc_of_key("sku-2"), Some(1), "the key lookup is rebuilt on load");
        assert_eq!(back.key_of(0), Some("sku-1"));
        assert_eq!(back.key_of(2), None, "a blank key stays no key across the round trip");
        assert_eq!(back.search("Toothpaste", 10).len(), ix.search("Toothpaste", 10).len());

        // The default costs nothing: no key field, no key section.
        let plain = built().to_bytes();
        let t = read_section_table(&plain).unwrap();
        assert_eq!(t.doc_key.len, 0, "an index without keys stores no key bytes");
        assert!(!Index::from_bytes(&plain).unwrap().has_key());
    }

    /// A key array shorter than the corpus must be REFUSED.
    ///
    /// Keys are positional, so one short entry shifts every later key onto the wrong document:
    /// `doc_of_key` then resolves a real key to a real-but-wrong row, and a change stream updates
    /// the wrong record. That is corruption that looks exactly like working software, which is why
    /// the reader counts rather than trusts.
    #[test]
    fn a_short_key_array_is_refused() {
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("sku", 0.0, 0.6),
            Field::new("name", 3.0, 0.4),
        ]))
        .with_key(0);
        b.add(&Doc::new(["sku-1", "Colgate Total Toothpaste"]));
        b.add(&Doc::new(["sku-2", "Aquafresh Mini Toothpaste"]));
        let bytes = b.build().unwrap().to_bytes();
        assert!(Index::from_bytes(&bytes).is_ok());

        // Shrink the doc_key span so the last key is unreadable. It is the 18th (last) span.
        let t = read_section_table(&bytes).unwrap();
        let mut bad = bytes.clone();
        let at = MAGIC.len() + 17 * 16;
        bad[at + 8..at + 16].copy_from_slice(&(t.doc_key.len - 4).to_le_bytes());
        assert!(
            Index::from_bytes(&bad).is_err(),
            "a truncated key array must error, not shift keys onto the wrong rows"
        );
    }

    /// The default must be free. Positions are the one option whose cost scales with token
    /// occurrences, so "off by default" has to mean zero bytes, not a small header.
    #[test]
    fn an_index_without_positions_costs_no_position_bytes() {
        let bytes = built().to_bytes();
        let t = read_section_table(&bytes).unwrap();
        assert_eq!(t.position_at.len, 0, "no offsets");
        assert_eq!(t.position.len, 0, "no positions");
    }

    /// A truncated offset array must be REFUSED, not tolerated. One entry short and `position_of`
    /// returns the next posting's run; the verifier then agrees with positions belonging to
    /// another term, which is a wrong phrase answer that looks exactly like a right one.
    #[test]
    fn a_truncated_position_offset_array_is_refused() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 3.0, 0.4)]))
            .with_position();
        b.add(&Doc::new(["Vanilla Ice Cream Tub"]));
        b.add(&Doc::new(["Ice Crushed Cream Soda"]));
        let bytes = b.build().unwrap().to_bytes();
        assert!(Index::from_bytes(&bytes).is_ok(), "the unmodified bytes must load");

        // Shorten `position_at` by exactly one u64 and fix up the table so nothing else notices.
        let t = read_section_table(&bytes).unwrap();
        let mut bad = bytes.clone();
        let at = MAGIC.len() + 15 * 16; // the position_at span is the 16th
        let len = (t.position_at.len - 8).to_le_bytes();
        bad[at + 8..at + 16].copy_from_slice(&len);
        assert!(
            Index::from_bytes(&bad).is_err(),
            "an offset array one entry short must be an error, not a silent aliasing"
        );
    }

    #[test]
    fn pages_partition_the_ranking_without_gaps_or_repeats() {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("name", 3.0, 0.4)]));
        for i in 0..40 {
            b.add(&Doc::new(vec![format!("Toothpaste variant {i}")]));
        }
        let ix = b.build().unwrap();
        let q = "Toothpaste";

        let all: Vec<u32> = ix.search(q, 30).iter().map(|h| h.doc).collect();
        let mut paged: Vec<u32> = Vec::new();
        for page in 0..3 {
            paged.extend(ix.search_page(q, page * 10, 10).iter().map(|h| h.doc));
        }
        assert_eq!(paged, all, "three pages of 10 must equal one request for 30");

        let mut seen = paged.clone();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), paged.len(), "no document appears on two pages");

        // Past the end is empty, not an error, and not a wrapped page.
        assert!(ix.search_page(q, 10_000, 10).is_empty());
        assert_eq!(ix.search_page(q, 0, 5).len(), 5);
    }

    #[test]
    fn an_index_without_facets_costs_no_facet_bytes() {
        let ix = built();
        let t = read_section_table(&ix.to_bytes()).unwrap();
        assert_eq!(t.facet_label.len, 0, "no facet field means no facet label bytes");
        assert_eq!(t.facet_id.len, 0, "and no per-document ids");
        assert!(ix.facet_label().is_empty());
        assert!(ix.facet_tally("anything").is_empty());
    }

    #[test]
    fn deletions_survive_a_round_trip() {
        // An artifact that dropped deletions would resurrect them on reload, which for presyo means
        // merged duplicates reappearing — the exact bug deletion exists to prevent.
        let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
        let mut b = IndexBuilder::new(schema);
        b.add(&Doc::new(["alpha widget"]));
        b.add(&Doc::new(["alpha gadget"]));
        let mut ix = b.build().unwrap();
        assert!(ix.delete(0));
        assert_eq!(ix.live_count(), 1);

        let round = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert!(round.is_deleted(0));
        assert_eq!(round.deleted_count(), 1);
        assert_eq!(round.live_count(), 1);
        let hit: Vec<u32> = round.search("alpha", 10).iter().map(|h| h.doc).collect();
        assert_eq!(hit, vec![1], "a deleted document must not come back from disk");
    }

    #[test]
    fn a_uniform_prior_costs_no_bytes() {
        // The absence of a prior and a uniform prior are the same thing, and neither should pay
        // doc_count floats for saying nothing.
        let bytes = built().to_bytes();
        let t = read_section_table(&bytes).unwrap();
        assert_eq!(t.prior.len, 0);
        assert!(!Index::from_bytes(&bytes).unwrap().has_prior());
    }

    #[test]
    fn priors_survive_a_round_trip_and_keep_their_ranking() {
        let schema = Schema::new(vec![Field::new("name", 1.0, 0.5)]);
        let mut b = IndexBuilder::new(schema);
        // Same text three times: ONLY the prior can separate them.
        b.add_with_prior(&Doc::new(["quezon"]), 1.0);
        b.add_with_prior(&Doc::new(["quezon"]), 9.0);
        b.add_with_prior(&Doc::new(["quezon"]), 3.0);
        let ix = b.build().unwrap();
        assert!(ix.has_prior());
        let before: Vec<u32> = ix.search("quezon", 3).iter().map(|h| h.doc).collect();
        assert_eq!(before, vec![1, 2, 0], "highest prior first");

        let round = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert!(round.has_prior());
        let after: Vec<u32> = round.search("quezon", 3).iter().map(|h| h.doc).collect();
        assert_eq!(after, before, "serialization must not change the order");
        // Normalized against the largest, so the top document sits at exactly 1.0.
        assert!((round.prior_of(1) - 1.0).abs() < 1e-6);
    }

    /// `p56`: a schema's unscored columns survive serialization, and so do the facet and numeric
    /// slots declared over them. Without the `IDXTEXT8` schema-section entries a loaded index
    /// would report a facet slot pointing at a column it can no longer name.
    #[test]
    fn unscored_columns_survive_a_round_trip() {
        let schema = Schema::new(vec![Field::new("path", 1.0, 0.75)])
            .with_column("format")
            .with_column("orientation")
            .with_column("width")
            .with_column("height")
            .with_column("byte");
        let mut b = IndexBuilder::new(schema)
            .with_facet_of("format")
            .with_facet_of("orientation")
            .with_numeric_of("width")
            .with_numeric_of("height")
            .with_numeric_of("byte");
        b.add(&Doc::new(["cdn/a/hero.jpg", "jpeg", "landscape", "1920", "1080", "204800"]));
        b.add(&Doc::new(["cdn/b/icon.png", "png", "square", "64", "64", "1024"]));
        let ix = b.build().unwrap();

        let back = Index::from_bytes(&ix.to_bytes()).unwrap();
        assert_eq!(back.schema().unscored, ix.schema().unscored, "column names survive");
        assert_eq!(back.schema().column_count(), 6, "one scored field plus five columns");
        assert_eq!(back.schema().column_of("width"), Some(3));
        assert_eq!(back.facet_field(), ix.facet_field());
        assert_eq!(back.numeric_field(), ix.numeric_field());
        assert_eq!(back.facet_label_at(1), ix.facet_label_at(1));
        assert_eq!(back.search("hero", 5), ix.search("hero", 5));
    }

    /// `p56` bumped `IDXTEXT7` -> `IDXTEXT8` by appending to two positionally-read sections. A
    /// reader must say *which* version it found rather than "bad magic", because "your file is a
    /// version old, rebuild it" and "this is not an index" are different instructions and only one
    /// of them is true.
    #[test]
    fn an_older_format_version_is_named_rather_than_called_garbage() {
        // Versions `9` and earlier are eight bytes, one shorter than `IDXTEXT10`, so the byte
        // after the magic belongs to the section table and is left alone -- that is exactly what
        // a real older file looks like.
        let mut old = built().to_bytes();
        old[..8].copy_from_slice(b"IDXTEXT9");
        let err = Index::from_bytes(&old).expect_err("an IDXTEXT9 file must be refused");
        assert!(err.contains("IDXTEXT9"), "must name the version found: {err}");
        // Compared against the CURRENT magic rather than a hard-coded one, so the next format
        // bump does not have to remember to edit this line.
        let now = std::str::from_utf8(&MAGIC).unwrap();
        assert!(err.contains(now), "must name the version read ({now}): {err}");

        // Long enough to get past the head-length check, so it is the magic that rejects it.
        let mut foreign = built().to_bytes();
        foreign[..8].copy_from_slice(b"PARQUET1");
        let err = Index::from_bytes(&foreign).expect_err("not an index");
        assert!(err.contains("not an index-text file"), "{err}");
        assert!(!err.contains("rebuild"), "a foreign file is not a stale index: {err}");
    }

    /// **The `p56` no-regression assertion.** Adding unscored columns -- and faceting and ranging
    /// over all of them -- must not move a single posting byte, because the per-posting array is
    /// `[u16; MAX_FIELD]` over *scored* fields only. Measured on the sections themselves rather
    /// than asserted in prose.
    #[test]
    fn unscored_columns_cost_no_posting_byte() {
        fn sections(ix: &Index) -> (u64, u64, Vec<u8>) {
            let bytes = ix.to_bytes();
            let t = read_section_table(&bytes[..MAGIC.len() + TABLE_BYTE]).unwrap();
            (t.posting.len, t.doc_len.len, bytes[t.posting.range()].to_vec())
        }
        let doc: [[&str; 6]; 3] = [
            ["cdn/a/hero.jpg", "jpeg", "landscape", "1920", "1080", "204800"],
            ["cdn/b/icon.png", "png", "square", "64", "64", "1024"],
            ["cdn/c/hero banner.webp", "webp", "landscape", "1600", "400", "51200"],
        ];

        // Baseline: one scored field, nothing else.
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("path", 1.0, 0.75)]));
        for d in doc.iter() {
            b.add(&Doc::new([d[0]]));
        }
        let lean = b.build().unwrap();

        // The same corpus with five unscored columns, two faceted and three numeric.
        let schema = Schema::new(vec![Field::new("path", 1.0, 0.75)])
            .with_column("format")
            .with_column("orientation")
            .with_column("width")
            .with_column("height")
            .with_column("byte");
        let mut b = IndexBuilder::new(schema)
            .with_facet_of("format")
            .with_facet_of("orientation")
            .with_numeric_of("width")
            .with_numeric_of("height")
            .with_numeric_of("byte");
        for d in doc.iter() {
            b.add(&Doc::new(*d));
        }
        let wide = b.build().unwrap();

        let (lean_post, lean_len, lean_bytes) = sections(&lean);
        let (wide_post, wide_len, wide_bytes) = sections(&wide);
        assert_eq!(wide_post, lean_post, "posting section grew by {} B", wide_post - lean_post);
        assert_eq!(wide_len, lean_len, "doc_len section grew");
        assert_eq!(wide_bytes, lean_bytes, "a posting byte moved");
        // ...and still exactly `MAX_FIELD` term frequencies per posting over the SCORED fields,
        // which is why no byte moved: five columns add no slot to that array.
        assert_eq!(
            wide.schema().field.len(),
            lean.schema().field.len(),
            "a column must not widen the per-posting array"
        );

        // ...and the columns are genuinely there, so this is not measuring two identical indexes.
        assert_eq!(wide.facet_slot_count(), 2);
        assert_eq!(wide.numeric_field(), [3, 4, 5]);
        assert_eq!(wide.facet_of_at(0, 1), Some("landscape"));
        assert_eq!(wide.search("hero", 5), lean.search("hero", 5), "and nothing scores differently");
    }
}
