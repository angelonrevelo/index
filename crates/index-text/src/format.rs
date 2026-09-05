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

/// `"IDXTEXT1"` — magic plus format version in eight bytes.
pub const MAGIC: [u8; 8] = *b"IDXTEXT2";

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
    pub posting_offset: Span,
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
}

/// Bytes on the wire for one posting: `u32` doc id + `MAX_FIELD` × `u16` term frequency.
const POSTING_BYTE: usize = 4 + 2 * crate::index::MAX_FIELD;
/// Section table is 11 spans × 2 × u64.
const TABLE_BYTE: usize = 11 * 16;

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
    fn need(&self, n: usize) -> Result<(), String> {
        if self.p + n > self.b.len() {
            Err(format!("truncated: want {n} bytes at {} of {}", self.p, self.b.len()))
        } else {
            Ok(())
        }
    }
    fn u16(&mut self) -> Result<u16, String> {
        self.need(2)?;
        let v = u16::from_le_bytes(self.b[self.p..self.p + 2].try_into().unwrap());
        self.p += 2;
        Ok(v)
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
/// `MAGIC.len() + 176` bytes to learn where everything else lives.
pub fn read_section_table(head: &[u8]) -> Result<SectionTable, String> {
    if head.len() < MAGIC.len() + TABLE_BYTE {
        return Err(format!("need {} head bytes, got {}", MAGIC.len() + TABLE_BYTE, head.len()));
    }
    if head[..MAGIC.len()] != MAGIC {
        return Err("bad magic — not an index-text file, or a different format version".into());
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
    let i = term_id as usize;
    if (i + 2) * 8 > posting_offset_bytes.len() {
        return Err(format!("term {term_id} out of range of the offset array"));
    }
    let get = |k: usize| -> u64 {
        u64::from_le_bytes(posting_offset_bytes[k * 8..k * 8 + 8].try_into().unwrap())
    };
    let (a, b) = (get(i), get(i + 1));
    Ok(Span { offset: table.posting.offset + a, len: b - a })
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
        let meta = w.span_from(start);

        let start = w.here();
        for f in s.field.iter() {
            w.str(&f.name);
            w.f32(f.boost);
            w.f32(f.b);
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

        // Offsets are relative to the start of the posting section, so the section can be
        // relocated or fetched independently.
        let start = w.here();
        let mut acc: u64 = 0;
        w.u64(0);
        for list in s.posting.iter() {
            acc += (list.len() * POSTING_BYTE) as u64;
            w.u64(acc);
        }
        let posting_offset = w.span_from(start);

        let start = w.here();
        for list in s.posting.iter() {
            for p in list.iter() {
                w.u32(p.0);
                for v in p.1.iter() {
                    w.u16(*v);
                }
            }
        }
        let posting = w.span_from(start);

        let start = w.here();
        for l in s.doc_len.iter() {
            for v in l.iter() {
                w.u16(*v);
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

        let mut r = Reader::new(&buf[table.schema.range()]);
        let mut field = Vec::with_capacity(field_count);
        for _ in 0..field_count {
            let name = r.str()?;
            let boost = r.f32()?;
            let b = r.f32()?;
            field.push(Field { name, boost, b });
        }
        let mut schema = Schema::new(field);
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
        if off.len() % 8 != 0 || off.len() < 8 {
            return Err("posting offset array is malformed".into());
        }
        let term_count = off.len() / 8 - 1;
        let get = |k: usize| -> u64 { u64::from_le_bytes(off[k * 8..k * 8 + 8].try_into().unwrap()) };
        let post_bytes = &buf[table.posting.range()];
        let mut posting = Vec::with_capacity(term_count);
        for i in 0..term_count {
            let (a, b) = (get(i) as usize, get(i + 1) as usize);
            if b < a || b > post_bytes.len() || (b - a) % POSTING_BYTE != 0 {
                return Err(format!("posting list {i} has a malformed span {a}..{b}"));
            }
            let mut list = Vec::with_capacity((b - a) / POSTING_BYTE);
            let mut pr = Reader::new(&post_bytes[a..b]);
            while pr.p < pr.b.len() {
                let doc = pr.u32()?;
                let mut tf = [0u16; crate::index::MAX_FIELD];
                for t in tf.iter_mut() {
                    *t = pr.u16()?;
                }
                list.push((doc, tf));
            }
            posting.push(list);
        }

        let dl = &buf[table.doc_len.range()];
        let stride = 2 * crate::index::MAX_FIELD;
        if dl.len() != doc_count * stride {
            return Err(format!("doc_len section is {} bytes, expected {}", dl.len(), doc_count * stride));
        }
        let mut doc_len = Vec::with_capacity(doc_count);
        for d in 0..doc_count {
            let mut l = [0u16; crate::index::MAX_FIELD];
            for (f, v) in l.iter_mut().enumerate() {
                let at = d * stride + f * 2;
                *v = u16::from_le_bytes(dl[at..at + 2].try_into().unwrap());
            }
            doc_len.push(l);
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
        Ok(ix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Doc, IndexBuilder};

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

        // ...then only that one list.
        assert!(span.len > 0, "colgate must have a non-empty posting list");
        let slice = &bytes[span.range()];
        assert_eq!(slice.len() % POSTING_BYTE, 0);

        let doc = u32::from_le_bytes(slice[0..4].try_into().unwrap());
        assert_eq!(doc, 3, "colgate appears in document 3");
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

    #[test]
    fn every_offset_in_the_table_is_u64() {
        // The section table is 11 spans of two u64s. If this size ever changes, the format version
        // in MAGIC must change with it — it went 7 -> 8 and `IDXTEXT1` -> `IDXTEXT2` when static
        // priors were added, because a reader expecting seven spans would mis-parse the eighth as
        // posting data and fail as garbage results rather than as an error.
        assert_eq!(TABLE_BYTE, 176);
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
}
