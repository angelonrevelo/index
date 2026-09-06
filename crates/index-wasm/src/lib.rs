//! `index-wasm` — the binding that makes the engine usable outside Rust.
//!
//! **One artifact, three consumers.** This crate is a `cdylib`, so the same source produces a
//! `.wasm` module for the browser and Node *and* a native shared library with a C ABI that Go,
//! Python, PHP, Ruby and anything else with an FFI can call. `docs/research/portability.md` §3:
//! *"C ABI + cbindgen is the actual lowest common denominator."*
//!
//! # Why there is no `wasm-bindgen`
//!
//! Every `String` or `Vec<u8>` crossing a `wasm-bindgen` boundary is an O(n) copy plus an
//! allocation. More importantly, a `wasm-bindgen` binding does **not** generalize to native FFI, so
//! it would be two artifacts to maintain instead of one.
//!
//! onegrid already ratified exactly this shape on this machine (`demand.md` Finding 4): a
//! hand-written raw-pointer ABI, versioned, with **JS owning the heap** through explicit alloc and
//! free calls. That design is reused here rather than reinvented, so `index` can drop into the
//! socket onegrid already cut.
//!
//! # The contract
//!
//! - The host allocates with [`idx_alloc`], writes bytes into linear memory, and passes
//!   `(ptr, len)`. The host frees with [`idx_free`]. **Nothing is freed implicitly.**
//! - `memory.grow()` detaches every JS view of the buffer — *even `grow(0)`* — so the host must
//!   re-derive its `Uint8Array` after **every** call that could allocate. The JS wrapper in
//!   `js/index.mjs` does this unconditionally.
//! - Results are written into a single owned buffer whose `(ptr, len)` the caller reads via
//!   [`idx_result_ptr`] / [`idx_result_len`]. It stays valid until the next search on that handle.
//!
//! # Result encoding
//!
//! A flat little-endian array of `hit_count` × 12-byte records: `u32 doc`, `f32 score`,
//! `u32 typo_bucket`. Little-endian because every target that matters is.
//!
//! The image tier reuses that shape rather than inventing a second one. An **image hit** is also
//! 12 bytes — `u32 doc`, `f32 score`, `u32 why` — where `why` is the signal bitmask
//! (`1` text, `2` vector, `4` hash, `8` colour) sitting in the slot `typo_bucket` occupies for a
//! text hit, so a host that already decodes text hits renames one field rather than writing a
//! second decoder. A **near-duplicate** record is 8 bytes: `u32 doc`, `u32 distance`. Image
//! results live in the image handle's own buffer, read with [`idx_image_result_ptr`] /
//! [`idx_image_result_len`].

use index_text::{
    AliasTable, Doc, FacetClause, Field, Index, IndexBuilder, Schema, SectionTable, Searcher, Span,
};
use std::alloc::{alloc, dealloc, Layout};

/// Bump this on any incompatible change to the exported signatures or the result encoding.
/// The host is expected to check it before doing anything else.
pub const ABI_VERSION: u32 = 14;

/// Bytes per hit record in the result buffer.
const HIT_BYTE: usize = 12;

/// An index under construction.
///
/// Building in-process matters as much as loading a prebuilt file: an application that can only
/// `idx_open` a blob must run a Rust build step to adopt the engine at all, which is a much larger
/// ask than `npm install`. With a builder, a Node or browser host can index its own data directly.
pub struct Builder {
    inner: IndexBuilder,
}

/// Parse the NUL-separated clause spec into borrowed pieces. `None` on a malformed clause.
///
/// One grammar, one parser: the single-index and the multi-segment entry points share this, so a
/// spec that works against `idx_search_clause` cannot mean something else against
/// [`idx_searcher_search_clause`].
///
/// The result borrows from `text` and must outlive the search, which is why the parse and the
/// [`FacetClause`] construction are two steps rather than one.
fn parse_clause_spec(text: &str) -> Option<Vec<(usize, bool, Vec<&str>)>> {
    let mut parsed: Vec<(usize, bool, Vec<&str>)> = Vec::new();
    for part in text.split('\0').filter(|p| !p.is_empty()) {
        let eq = part.find('=')?;
        let (head, value) = part.split_at(eq);
        let value = &value[1..];
        let (slot_text, exclude) = match head.strip_suffix('!') {
            Some(rest) => (rest, true),
            None => (head, false),
        };
        let slot = slot_text.parse::<usize>().ok()?;
        parsed.push((slot, exclude, value.split('|').filter(|v| !v.is_empty()).collect()));
    }
    Some(parsed)
}

/// Borrow a parsed spec as clauses. Separate from [`parse_clause_spec`] only because the borrow
/// checker needs the owned `Vec<&str>` to have a name that outlives the clause slice.
fn borrow_clause<'a>(parsed: &'a [(usize, bool, Vec<&'a str>)]) -> Vec<FacetClause<'a>> {
    parsed
        .iter()
        .map(|(slot, exclude, value)| FacetClause { slot: *slot, value, exclude: *exclude })
        .collect()
}

/// **The full filter bar, with paging**: OR within a clause, AND across clauses, NOT, and an
/// offset.
///
/// `spec` is a NUL-separated list of clauses. Each clause is `slot[!]=v1|v2|...`:
///
/// - `0=Colgate|Oral B`  — slot 0 is Colgate **or** Oral B
/// - `1!=discontinued`   — slot 1 is **not** discontinued
///
/// Clauses are AND-ed. `|` separates alternatives, so a facet value containing `|` cannot be
/// expressed — every other byte can, including `=` after the first one.
///
/// An unknown value inside a clause is ignored; an include whose values are *all* unknown matches
/// nothing, while an exclude whose values are all unknown excludes nothing. Returns `u32::MAX` on a
/// malformed spec, and 0 when the filter is unsatisfiable — never everything.
///
/// # Safety
/// `h` must be a live handle; `q` and `spec` readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_search_clause(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    k: u32,
    offset: u32,
    spec: *const u8,
    spec_len: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() || spec.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(spec, spec_len)) else {
        return u32::MAX;
    };
    // Values are borrowed from `text`, so the parse must outlive the search call.
    let Some(parsed) = parse_clause_spec(text) else { return u32::MAX };
    let clause = borrow_clause(&parsed);
    let hit = handle.index.search_clause(query, k as usize, offset as usize, &clause, &[]);
    write_hit(&mut handle.result, &hit);
    hit.len() as u32
}

/// **Phrase query**: the query's tokens, consecutive and in order, within ONE field.
///
/// Requires an index built with [`idx_build_position`]. Without positions this returns **0 hits**
/// rather than falling back to an ordinary term search — a silent fallback would return
/// bag-of-words results that look like phrase results, which is the failure this refuses to have.
///
/// **A phrase is exact.** No token is typo-corrected or prefix-expanded: quoting is the caller
/// asserting these words, and correcting inside a phrase answers a different question. A token
/// absent from the dictionary therefore matches **nothing**, the same direction as an include
/// clause whose values are all unknown.
///
/// Ranking is unchanged — the phrase is a filter applied after scoring, like a facet clause, so a
/// phrase hit carries the score the same query would have given it.
///
/// # Safety
/// `h` must be a live handle; `q` must be readable for `q_len` bytes and be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn idx_search_phrase(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    offset: u32,
    k: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = handle.index.search_phrase_page(query, offset as usize, k as usize);
    write_hit(&mut handle.result, &hit);
    hit.len() as u32
}

/// [`idx_search_phrase`] across every segment of a searcher.
///
/// A segment built without positions contributes nothing, rather than contributing term matches
/// that would be indistinguishable from phrase matches once merged.
///
/// # Safety
/// `s` must be a live searcher; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search_phrase(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    offset: u32,
    k: u32,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = searcher.inner.search_phrase_page(query, offset as usize, k as usize);
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// [`idx_search`] starting at `offset` — page `n` is `offset = n * k`.
///
/// **Cost grows with `offset`, not `k`**: the engine over-fetches `offset + k` and drops the
/// prefix, because rank order is only known once everything above the page is scored. For deep
/// paging, filter instead.
///
/// # Safety
/// `h` must be a live handle; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_search_page(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    offset: u32,
    k: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = handle.index.search_page(query, offset as usize, k as usize);
    write_hit(&mut handle.result, &hit);
    hit.len() as u32
}

/// Byte ranges of `text` that matched `query` — what a UI bolds.
///
/// The index does not store field text, so the host passes back the row it already has. Writes
/// `count` pairs of little-endian `u32` (`start`, `end`) into the handle's result buffer and
/// returns `count`, or `u32::MAX` on a bad pointer or invalid UTF-8.
///
/// Offsets are **byte** offsets into `text`, ascending and non-overlapping, and always land on
/// UTF-8 character boundaries — a JavaScript host slicing a `Uint8Array` and decoding gets whole
/// characters back. They are NOT UTF-16 code-unit offsets, so `String.prototype.slice` on a
/// JavaScript string is the wrong tool for text outside the BMP.
///
/// # Safety
/// `h` must be a live handle; `q` and `text` readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_highlight(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    text: *const u8,
    text_len: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() || text.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Ok(body) = std::str::from_utf8(std::slice::from_raw_parts(text, text_len)) else {
        return u32::MAX;
    };
    let span = handle.index.highlight(query, body);
    handle.result.clear();
    for (a, b) in &span {
        handle.result.extend_from_slice(&(*a as u32).to_le_bytes());
        handle.result.extend_from_slice(&(*b as u32).to_le_bytes());
    }
    span.len() as u32
}

/// A live, appendable collection: an ordered set of immutable segments searched as one.
///
/// This is the incremental-update path. Building a whole corpus is O(corpus); adding a thousand
/// rows to a live one should not be, so new rows go into a small new segment that is cheap to
/// build, and a query runs against every segment and merges.
pub struct SearcherHandle {
    inner: Searcher,
    result: Vec<u8>,
}

/// Turn an index into a searcher. **CONSUMES the handle** -- do not use or close it afterwards.
///
/// # Safety
/// `h` must be a live handle from [`idx_open`] or [`idx_build_finish`], not previously consumed.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_new(h: *mut Handle) -> *mut SearcherHandle {
    if h.is_null() {
        return std::ptr::null_mut();
    }
    let handle = Box::from_raw(h);
    Box::into_raw(Box::new(SearcherHandle {
        inner: Searcher::new(handle.index),
        result: Vec::new(),
    }))
}

/// Append a segment. **CONSUMES the handle.** Returns 1 on success, 0 if either pointer is null.
///
/// Its documents take the next global ordinals, so ordinals already handed out stay valid -- an
/// application holding a document id from before the append does not have to re-resolve it.
///
/// # Safety
/// `s` must be a live searcher; `h` a live, unconsumed handle.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_push(s: *mut SearcherHandle, h: *mut Handle) -> u32 {
    let Some(searcher) = s.as_mut() else { return 0 };
    if h.is_null() {
        return 0;
    }
    searcher.inner.push(Box::from_raw(h).index);
    1
}

/// # Safety
/// `s` must be a live searcher, not previously closed.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_close(s: *mut SearcherHandle) {
    if !s.is_null() {
        drop(Box::from_raw(s));
    }
}

/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_doc_count(s: *const SearcherHandle) -> u32 {
    s.as_ref().map_or(0, |x| x.inner.doc_count() as u32)
}

/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_segment_count(s: *const SearcherHandle) -> u32 {
    s.as_ref().map_or(0, |x| x.inner.segment_count() as u32)
}

/// Documents still live after deletions.
///
/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_live_count(s: *const SearcherHandle) -> u32 {
    s.as_ref().map_or(0, |x| x.inner.live_count() as u32)
}

/// Non-zero when too much of the collection lives outside the largest segment, or too much of it is
/// deleted. A host is expected to rebuild from its source rows when this turns on -- compaction
/// here is a rebuild, not a merge, because the rows are the application's, not the index's.
///
/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_needs_compaction(s: *const SearcherHandle) -> u32 {
    s.as_ref().map_or(0, |x| u32::from(x.inner.needs_compaction()))
}

/// Tombstone a document by GLOBAL ordinal. Returns 1 if it was live and is now deleted.
///
/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_delete(s: *mut SearcherHandle, global: u32) -> u32 {
    let Some(searcher) = s.as_mut() else { return 0 };
    u32::from(searcher.inner.delete(global))
}

/// Search every segment. `prefix` non-zero runs typeahead semantics on the last token.
/// Hits use GLOBAL ordinals and the usual `IDX_HIT_BYTE` encoding.
///
/// # Safety
/// `s` must be a live searcher; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    k: u32,
    prefix: u32,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = if prefix != 0 {
        searcher.inner.search_prefix(query, k as usize)
    } else {
        searcher.inner.search(query, k as usize)
    };
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// Conjunctive faceted search across every segment. `spec` is the NUL-separated `slot=value` form
/// used by [`idx_search_facet_all`].
///
/// # Safety
/// `s` must be a live searcher; `q` and `spec` readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search_facet_all(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    k: u32,
    spec: *const u8,
    spec_len: usize,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() || spec.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(spec, spec_len)) else {
        return u32::MAX;
    };
    let mut want: Vec<(usize, &str)> = Vec::new();
    for part in text.split('\0').filter(|p| !p.is_empty()) {
        let Some((slot, value)) = part.split_once('=') else { return u32::MAX };
        let Ok(slot) = slot.parse::<usize>() else { return u32::MAX };
        want.push((slot, value));
    }
    let hit = searcher.inner.search_facet_all(query, k as usize, &want);
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// **The full filter bar, with paging, across every segment.** `spec` is the same
/// `slot[!]=v1|v2|...` grammar [`idx_search_clause`] takes, parsed by the same code.
///
/// The safety rule is per SEGMENT and per clause: a value unknown to one segment is ignored there,
/// so an include whose values are all unknown *to that segment* contributes nothing from it, and an
/// exclude whose values are all unknown removes nothing from it. A filter bar built from one
/// segment's labels therefore stays correct against a searcher holding several.
///
/// Every segment must yield `offset + k` before the merge, because the page boundary is global: a
/// segment's third-best hit can be the page's first.
///
/// # Safety
/// `s` must be a live searcher; `q` and `spec` readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search_clause(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    k: u32,
    offset: u32,
    spec: *const u8,
    spec_len: usize,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() || spec.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(spec, spec_len)) else {
        return u32::MAX;
    };
    let Some(parsed) = parse_clause_spec(text) else { return u32::MAX };
    let clause = borrow_clause(&parsed);
    let hit = searcher.inner.search_clause(query, k as usize, offset as usize, &clause, &[]);
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// [`idx_searcher_search`] starting at `offset` — page `n` is `offset = n * k`.
///
/// **Cost grows with `offset` and with segment count together**, not with `k`: every segment has to
/// produce `offset + k` hits before the merge can decide which of them the page contains. For deep
/// paging, filter instead.
///
/// # Safety
/// `s` must be a live searcher; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search_page(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    offset: u32,
    k: u32,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = searcher.inner.search_page(query, offset as usize, k as usize);
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// Search a half-open numeric range across every segment.
///
/// # Safety
/// `s` must be a live searcher; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search_range(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    k: u32,
    slot: u32,
    lo: f64,
    hi: f64,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = searcher.inner.search_range(query, k as usize, slot as usize, lo, hi);
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// Sort by a numeric column across every segment.
///
/// # Safety
/// `s` must be a live searcher; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_search_sorted(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    k: u32,
    slot: u32,
    ascending: u32,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = searcher.inner.search_sorted(query, k as usize, slot as usize, ascending != 0);
    write_hit(&mut searcher.result, &hit);
    hit.len() as u32
}

/// Facet tally across every segment, merged by VALUE. Same length-prefixed encoding as
/// [`idx_facet_tally`]: `u32 label_len`, bytes, `u32 count`.
///
/// # Safety
/// `s` must be a live searcher; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_facet_tally(
    s: *mut SearcherHandle,
    q: *const u8,
    q_len: usize,
    slot: u32,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let tally = searcher.inner.facet_tally_at(query, slot as usize);
    searcher.result.clear();
    for (label, count) in &tally {
        searcher.result.extend_from_slice(&(label.len() as u32).to_le_bytes());
        searcher.result.extend_from_slice(label.as_bytes());
        searcher.result.extend_from_slice(&(*count as u32).to_le_bytes());
    }
    tally.len() as u32
}

/// Pointer to the last searcher result. Invalidated by the next call on the same searcher.
///
/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_result_ptr(s: *const SearcherHandle) -> *const u8 {
    s.as_ref().map_or(std::ptr::null(), |x| x.result.as_ptr())
}

/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_result_len(s: *const SearcherHandle) -> usize {
    s.as_ref().map_or(0, |x| x.result.len())
}

/// Shared hit encoder: doc `u32`, score `f32`, typo_bucket `u32`, little-endian, packed.
fn write_hit(out: &mut Vec<u8>, hit: &[index_text::Hit]) {
    out.clear();
    out.reserve(hit.len() * HIT_BYTE);
    for x in hit {
        out.extend_from_slice(&x.doc.to_le_bytes());
        out.extend_from_slice(&x.score.to_le_bytes());
        out.extend_from_slice(&x.typo_bucket.to_le_bytes());
    }
}

/// Create a builder. `field_spec` is a NUL-separated list of `name:boost:b` triples, e.g.
/// `"label:3:0.4\0sublabel:1:0.6"`. Returns null on a malformed spec.
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_build_new(ptr: *const u8, len: usize) -> *mut Builder {
    if ptr.is_null() || len == 0 {
        return std::ptr::null_mut();
    }
    let Ok(spec) = std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) else {
        return std::ptr::null_mut();
    };
    let mut field = Vec::new();
    for part in spec.split('\0').filter(|s| !s.is_empty()) {
        let mut it = part.split(':');
        let (Some(name), Some(boost), Some(b)) = (it.next(), it.next(), it.next()) else {
            return std::ptr::null_mut();
        };
        let (Ok(boost), Ok(b)) = (boost.parse::<f32>(), b.parse::<f32>()) else {
            return std::ptr::null_mut();
        };
        field.push(Field::new(name, boost, b));
    }
    if field.is_empty() || field.len() > index_text::MAX_FIELD {
        return std::ptr::null_mut();
    }
    let inner = IndexBuilder::new(Schema::new(field))
        .with_alias(AliasTable::philippine_grocery());
    Box::into_raw(Box::new(Builder { inner }))
}

/// Designate field `field` as the stored facet, before any document is added.
///
/// Returns 1 on success, 0 if the builder is null or `field` is out of range. Faceting is what
/// makes shopping search work -- filter by category and count per category -- and it is stored, so
/// it must be chosen at build time rather than at query time.
///
/// # Safety
/// `b` must be a live builder from [`idx_build_new`] with no documents added yet.
#[no_mangle]
pub unsafe extern "C" fn idx_build_facet(b: *mut Builder, field: u32) -> u32 {
    let Some(builder) = b.as_mut() else { return 0 };
    // `set_facet_field` rather than `with_facet`: the consuming form would need a placeholder
    // builder to swap through, and constructing one asserts on an empty schema -- a panic here is
    // a trap that kills the instance. Caught by `the_abi_facets`.
    u32::from(builder.inner.set_facet_field(field as usize))
}

/// Designate field `field` as the document's **primary key**, before any document is added.
///
/// Returns 1 on success, 0 if the builder is null, the field is out of range, or a document has
/// already been added — a key cannot be recovered afterwards.
///
/// Without one, a host can search a collection but can never say *which row* changed: every
/// incremental operation an application performs is keyed on ITS id, and [`idx_searcher_delete`]
/// takes a dense ordinal no database row carries.
///
/// # Safety
/// `b` must be a live builder from [`idx_build_new`].
#[no_mangle]
pub unsafe extern "C" fn idx_build_key(b: *mut Builder, field: u32) -> u32 {
    let Some(builder) = b.as_mut() else { return 0 };
    u32::from(builder.inner.set_key_field(field as usize))
}

/// The document carrying `key`, or `u32::MAX` if there is none.
///
/// **A deleted document is still found.** This answers "which ordinal is this row", which a caller
/// needs precisely in order to delete it; filtering here would make deleting an already-deleted row
/// indistinguishable from deleting a row that never existed.
///
/// # Safety
/// `h` must be a live handle; `key` readable for `key_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_doc_of_key(h: *const Handle, key: *const u8, key_len: usize) -> u32 {
    let Some(handle) = h.as_ref() else { return u32::MAX };
    if key.is_null() {
        return u32::MAX;
    }
    let Ok(k) = std::str::from_utf8(std::slice::from_raw_parts(key, key_len)) else {
        return u32::MAX;
    };
    handle.index.doc_of_key(k).unwrap_or(u32::MAX)
}

/// Write the application key of `doc` into the handle's result buffer; returns its byte length, or
/// 0 when the row has no key.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_key_of(h: *mut Handle, doc: u32) -> usize {
    let Some(handle) = h.as_mut() else { return 0 };
    handle.result = handle.index.key_of(doc).unwrap_or("").as_bytes().to_vec();
    handle.result.len()
}

/// How many documents carry a key. Below `idx_doc_count` when some key fields were blank — worth
/// surfacing, because those rows can never be addressed by a change stream.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_keyed_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.keyed_count() as u32)
}

/// The LIVE document carrying `key` across every segment, as a global ordinal, or `u32::MAX`.
///
/// Segments are searched newest first, because a key in more than one segment means the row was
/// updated and the newest version is the live one.
///
/// # Safety
/// `s` must be a live searcher; `key` readable for `key_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_doc_of_key(
    s: *const SearcherHandle,
    key: *const u8,
    key_len: usize,
) -> u32 {
    let Some(searcher) = s.as_ref() else { return u32::MAX };
    if key.is_null() {
        return u32::MAX;
    }
    let Ok(k) = std::str::from_utf8(std::slice::from_raw_parts(key, key_len)) else {
        return u32::MAX;
    };
    searcher.inner.doc_of_key(k).unwrap_or(u32::MAX)
}

/// Tombstone the row carrying `key`. Returns 1 if a live row was found and retired.
///
/// **This is the operation a change stream's delete becomes**, and the reason keys exist:
/// [`idx_searcher_delete`] takes a dense global ordinal, which is assigned at insertion and is not
/// something a database row carries.
///
/// Deleting a key that is already gone returns 0 and is not an error — a change stream replayed
/// from an earlier offset re-delivers deletes, and refusing would make replay impossible.
///
/// # Safety
/// `s` must be a live searcher; `key` readable for `key_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_delete_key(
    s: *mut SearcherHandle,
    key: *const u8,
    key_len: usize,
) -> u32 {
    let Some(searcher) = s.as_mut() else { return 0 };
    if key.is_null() {
        return 0;
    }
    let Ok(k) = std::str::from_utf8(std::slice::from_raw_parts(key, key_len)) else {
        return 0;
    };
    u32::from(searcher.inner.delete_key(k))
}

/// Write the application key of a GLOBAL ordinal into the searcher's result buffer; returns its
/// byte length, or 0 when the row has no key.
///
/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_key_of(s: *mut SearcherHandle, global: u32) -> usize {
    let Some(searcher) = s.as_mut() else { return 0 };
    searcher.result = searcher.inner.key_of(global).unwrap_or("").as_bytes().to_vec();
    searcher.result.len()
}

/// Whether every segment carries keys, so the collection can be driven by a change stream.
///
/// All-or-nothing on purpose: one unkeyed segment means some rows can never be addressed, and an
/// update that silently skipped them would drift from the source of truth with no signal.
///
/// # Safety
/// `s` must be a live searcher.
#[no_mangle]
pub unsafe extern "C" fn idx_searcher_has_key(s: *const SearcherHandle) -> u32 {
    s.as_ref().map_or(0, |x| u32::from(x.inner.has_key()))
}

/// Record **token positions**, before any document is added. Required for [`idx_search_phrase`]
/// and used by nothing else. Returns 1 on success, 0 if the builder is null or a document has
/// already been added.
///
/// Opt-in because it is the only build option that costs one `u32` per token OCCURRENCE rather
/// than per document. An index that answers no phrase query stores no positions at all.
///
/// # Safety
/// `b` must be a live builder from [`idx_build_new`].
#[no_mangle]
pub unsafe extern "C" fn idx_build_position(b: *mut Builder) -> u32 {
    let Some(builder) = b.as_mut() else { return 0 };
    u32::from(builder.inner.set_position())
}

/// Designate field `field` as a numeric column before any document is added, for range filtering
/// and histograms. Returns 1 on success, 0 if the builder is null, the field is out of range, or a
/// document has already been added.
///
/// Text that does not parse as a finite number becomes NaN, which no range contains -- an absent
/// value is excluded from every filter rather than treated as zero.
///
/// # Safety
/// `b` must be a live builder from [`idx_build_new`].
#[no_mangle]
pub unsafe extern "C" fn idx_build_numeric(b: *mut Builder, field: u32) -> u32 {
    let Some(builder) = b.as_mut() else { return 0 };
    u32::from(builder.inner.set_numeric_field(field as usize))
}

/// Add one document. `ptr`/`len` is a NUL-separated list of field values, positionally aligned
/// with the schema. Returns the document ordinal, or `u32::MAX` on error.
///
/// # Safety
/// `b` must be a live builder; `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_build_add(b: *mut Builder, ptr: *const u8, len: usize) -> u32 {
    let Some(builder) = b.as_mut() else { return u32::MAX };
    if ptr.is_null() {
        return u32::MAX;
    }
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) else {
        return u32::MAX;
    };
    let field: Vec<&str> = text.split('\0').collect();
    builder.inner.add(&Doc::new(field))
}

/// Finish building and return a searchable handle. **Consumes the builder** — it must not be used
/// again, and must not be passed to [`idx_build_free`] afterwards.
///
/// # Safety
/// `b` must be a live builder from [`idx_build_new`].
#[no_mangle]
pub unsafe extern "C" fn idx_build_finish(b: *mut Builder) -> *mut Handle {
    if b.is_null() {
        return std::ptr::null_mut();
    }
    let builder = Box::from_raw(b);
    match builder.inner.build() {
        Ok(index) => Box::into_raw(Box::new(Handle { index, result: Vec::new() })),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Discard a builder that will never be finished.
///
/// # Safety
/// `b` must be a live builder that has NOT been passed to [`idx_build_finish`].
#[no_mangle]
pub unsafe extern "C" fn idx_build_free(b: *mut Builder) {
    if !b.is_null() {
        drop(Box::from_raw(b));
    }
}

/// Serialize a live index. Returns the byte length written into the handle's result buffer
/// (read with [`idx_result_ptr`]), or 0 on error. Lets a host build once and cache the bytes.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_serialize(h: *mut Handle) -> usize {
    let Some(handle) = h.as_mut() else { return 0 };
    handle.result = handle.index.to_bytes();
    handle.result.len()
}

/// A loaded index plus the buffer its last search wrote into.
///
/// Public only because it appears in the signatures of the exported functions; it is **opaque** and
/// hosts must treat it as a token. Its layout is not part of the ABI.
pub struct Handle {
    index: Index,
    result: Vec<u8>,
}

/// ABI version. Call first; if it does not match what the host was built against, stop.
///
/// # Safety
/// None — takes no pointers.
#[no_mangle]
pub extern "C" fn idx_abi_version() -> u32 {
    ABI_VERSION
}

/// Allocate `len` bytes in the module's linear memory and return the pointer.
///
/// Returns null on a zero length or on allocation failure. **The host owns this memory** and must
/// return it with [`idx_free`] using the identical length.
///
/// # Safety
/// The returned pointer is valid for `len` bytes until passed to [`idx_free`].
#[no_mangle]
pub extern "C" fn idx_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    match Layout::from_size_align(len, 1) {
        // SAFETY: `len` is non-zero and the layout is valid, which is `alloc`'s contract.
        Ok(layout) => unsafe { alloc(layout) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free memory obtained from [`idx_alloc`].
///
/// # Safety
/// `ptr` must have come from [`idx_alloc`] with the same `len`, and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn idx_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    if let Ok(layout) = Layout::from_size_align(len, 1) {
        dealloc(ptr, layout);
    }
}

/// Open a serialized index (produced by `Index::to_bytes`).
///
/// Returns an opaque handle, or null if the bytes are not a valid index — **a corrupt or truncated
/// buffer returns null rather than trapping**, because this parses data that may have arrived over
/// HTTP.
///
/// The caller may free its copy of `(ptr, len)` immediately afterwards; the index is copied in.
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_open(ptr: *const u8, len: usize) -> *mut Handle {
    if ptr.is_null() || len == 0 {
        return std::ptr::null_mut();
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    match Index::from_bytes(bytes) {
        Ok(index) => Box::into_raw(Box::new(Handle { index, result: Vec::new() })),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Release a handle from [`idx_open`].
///
/// # Safety
/// `h` must have come from [`idx_open`] and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn idx_close(h: *mut Handle) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

/// Number of documents in the index, or 0 for a null handle.
///
/// # Safety
/// `h` must be null or a live handle from [`idx_open`].
#[no_mangle]
pub unsafe extern "C" fn idx_doc_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.doc_count() as u32)
}

/// Number of distinct terms in the dictionary, or 0 for a null handle.
///
/// # Safety
/// `h` must be null or a live handle from [`idx_open`].
#[no_mangle]
pub unsafe extern "C" fn idx_term_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.term_count() as u32)
}

/// Run a query. Returns the number of hits written to the result buffer, or `u32::MAX` on error.
///
/// `prefix` non-zero enables typeahead semantics on the final token — the right default for a
/// search-as-you-type box, and Meilisearch's rule.
///
/// # Safety
/// `h` must be a live handle; `q` must be readable for `q_len` bytes and be valid UTF-8.
#[no_mangle]
pub unsafe extern "C" fn idx_search(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    k: u32,
    prefix: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = if prefix != 0 {
        handle.index.search_prefix(query, k as usize)
    } else {
        handle.index.search(query, k as usize)
    };
    handle.result.clear();
    handle.result.reserve(hit.len() * HIT_BYTE);
    for x in &hit {
        handle.result.extend_from_slice(&x.doc.to_le_bytes());
        handle.result.extend_from_slice(&x.score.to_le_bytes());
        handle.result.extend_from_slice(&x.typo_bucket.to_le_bytes());
    }
    hit.len() as u32
}

/// [`idx_search`] restricted to documents whose value in facet `slot` is exactly the UTF-8 string
/// at `v`/`v_len`. Results are written to the handle's result buffer in the same 12-byte hit format.
/// Returns the hit count, or `u32::MAX` on error. An unknown facet value returns 0, not everything.
///
/// # Safety
/// `h` must be a live handle; `q` and `v` must be readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_search_facet(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    k: u32,
    slot: u32,
    v: *const u8,
    v_len: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() || v.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Ok(value) = std::str::from_utf8(std::slice::from_raw_parts(v, v_len)) else {
        return u32::MAX;
    };
    let hit = handle.index.search_facet_at(query, k as usize, slot as usize, value);
    handle.result.clear();
    handle.result.reserve(hit.len() * HIT_BYTE);
    for x in &hit {
        handle.result.extend_from_slice(&x.doc.to_le_bytes());
        handle.result.extend_from_slice(&x.score.to_le_bytes());
        handle.result.extend_from_slice(&x.typo_bucket.to_le_bytes());
    }
    hit.len() as u32
}

/// Count how many documents matching `query` carry each value of facet `slot`.
///
/// Writes into the handle's result buffer, one entry after another:
/// `u32 label_len`, `label_len` UTF-8 bytes, `u32 count` -- all little-endian, no padding.
/// Returns the number of entries, or `u32::MAX` on error. Sorted by count descending.
///
/// Length-prefixed rather than NUL-separated because a facet value may legitimately contain
/// anything except a NUL, and this format does not have to assume even that.
///
/// # Safety
/// `h` must be a live handle; `q` must be readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_facet_tally(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    slot: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let tally = handle.index.facet_tally_at(query, slot as usize);
    handle.result.clear();
    for (label, count) in &tally {
        handle.result.extend_from_slice(&(label.len() as u32).to_le_bytes());
        handle.result.extend_from_slice(label.as_bytes());
        handle.result.extend_from_slice(&(*count as u32).to_le_bytes());
    }
    tally.len() as u32
}

/// [`idx_search`] restricted to `lo <= value < hi` on numeric column `slot`.
///
/// Half-open on purpose: adjacent buckets in a price filter must not both contain the boundary, or
/// the counts beside them add up to more than the result set. Returns the hit count, or
/// `u32::MAX` on error; an unknown slot returns 0, not everything.
///
/// # Safety
/// `h` must be a live handle; `q` must be readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_search_range(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    k: u32,
    slot: u32,
    lo: f64,
    hi: f64,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = handle.index.search_range(query, k as usize, slot as usize, lo, hi);
    handle.result.clear();
    handle.result.reserve(hit.len() * HIT_BYTE);
    for x in &hit {
        handle.result.extend_from_slice(&x.doc.to_le_bytes());
        handle.result.extend_from_slice(&x.score.to_le_bytes());
        handle.result.extend_from_slice(&x.typo_bucket.to_le_bytes());
    }
    hit.len() as u32
}

/// Histogram of numeric column `slot` over every document matching `query`.
///
/// `edge`/`edge_n` are ascending bucket boundaries; bucket `i` is `edge[i] <= v < edge[i+1]`.
/// Writes `edge_n - 1` little-endian `u32` counts into the result buffer and returns that number,
/// or `u32::MAX` on error. A document with no value in the column is counted in no bucket, so the
/// counts sum to at most the match count rather than exactly it.
///
/// # Safety
/// `h` must be a live handle; `q` and `edge` must be readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_range_tally(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    slot: u32,
    edge: *const f64,
    edge_n: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() || edge.is_null() || edge_n < 2 {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let edge = std::slice::from_raw_parts(edge, edge_n);
    let count = handle.index.range_tally(query, slot as usize, edge);
    handle.result.clear();
    for c in &count {
        handle.result.extend_from_slice(&(*c as u32).to_le_bytes());
    }
    count.len() as u32
}

/// Sort matching documents by numeric column `slot` instead of by relevance.
///
/// `ascending` non-zero gives smallest first. Documents with no value in the column are excluded --
/// there is no position in a price order for a product with no price. Returns the hit count, or
/// `u32::MAX` on error; an unknown slot returns 0.
///
/// **This cannot prune.** Relevance ordering is what lets the engine stop early; a numeric order
/// gives it nothing to stop on, because the cheapest item may match the query worst. Cost tracks
/// the query's match count, not `k`.
///
/// # Safety
/// `h` must be a live handle; `q` must be readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_search_sorted(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    k: u32,
    slot: u32,
    ascending: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let hit = handle.index.search_sorted(query, k as usize, slot as usize, ascending != 0);
    handle.result.clear();
    handle.result.reserve(hit.len() * HIT_BYTE);
    for x in &hit {
        handle.result.extend_from_slice(&x.doc.to_le_bytes());
        handle.result.extend_from_slice(&x.score.to_le_bytes());
        handle.result.extend_from_slice(&x.typo_bucket.to_le_bytes());
    }
    hit.len() as u32
}

/// How many numeric columns the index carries.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_numeric_slot_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.numeric_slot_count() as u32)
}

/// Number of distinct values in facet `slot`. 0 when the slot does not exist.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_facet_count(h: *const Handle, slot: u32) -> u32 {
    h.as_ref().map_or(0, |x| x.index.facet_label_at(slot as usize).len() as u32)
}

/// How many facet slots the index carries. 0 when it has no facet field.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_facet_slot_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.facet_slot_count() as u32)
}

/// Conjunctive faceted search: every `slot=value` pair in the NUL-separated `spec` must hold,
/// e.g. `"0=Colgate " + "1=Accessory"`. The slot is decimal, the value is everything after the
/// first `=`, so a value may contain `=`.
///
/// Returns the hit count, `u32::MAX` on a malformed spec or bad pointer, and 0 when the filter is
/// unsatisfiable -- never everything, which is the dangerous reading of a filter.
///
/// # Safety
/// `h` must be a live handle; `q` and `spec` must be readable for their lengths.
#[no_mangle]
pub unsafe extern "C" fn idx_search_facet_all(
    h: *mut Handle,
    q: *const u8,
    q_len: usize,
    k: u32,
    spec: *const u8,
    spec_len: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if q.is_null() || spec.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(spec, spec_len)) else {
        return u32::MAX;
    };
    let mut want: Vec<(usize, &str)> = Vec::new();
    for part in text.split(' ').filter(|p| !p.is_empty()) {
        let Some((slot, value)) = part.split_once('=') else { return u32::MAX };
        let Ok(slot) = slot.parse::<usize>() else { return u32::MAX };
        want.push((slot, value));
    }
    let hit = handle.index.search_facet_all(query, k as usize, &want);
    handle.result.clear();
    handle.result.reserve(hit.len() * HIT_BYTE);
    for x in &hit {
        handle.result.extend_from_slice(&x.doc.to_le_bytes());
        handle.result.extend_from_slice(&x.score.to_le_bytes());
        handle.result.extend_from_slice(&x.typo_bucket.to_le_bytes());
    }
    hit.len() as u32
}

/// Pointer to the last search's result buffer. Valid until the next `idx_search` on this handle.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_result_ptr(h: *const Handle) -> *const u8 {
    h.as_ref().map_or(std::ptr::null(), |x| x.result.as_ptr())
}

/// Byte length of the last search's result buffer.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_result_len(h: *const Handle) -> usize {
    h.as_ref().map_or(0, |x| x.result.len())
}

// ================================================================================================
// The image tier — `index-image` through the same hand-written C ABI.
// ================================================================================================
//
// An image tier only Rust can call would forfeit the thesis this crate exists to defend:
// `docs/research/image.md` §1 finds that **every FOSS incumbent needs a server** to answer a
// question about your own files, and §2 finds the browser is exactly where they fail. So the image
// surface is exported here, beside the text surface, in the same idiom — opaque handles, host-owned
// memory, flat little-endian result records, a sentinel for every bad input, and nothing freed
// implicitly.
//
// # Why the embedding crosses as raw `f32`, never as JSON
//
// A 512-d embedding is 2 KB as `f32` and roughly 6 KB as JSON text that must then be *parsed* on
// arrival. `docs/research/image.md` §8 records `rclip` ingesting 1.28 M images in 3 hours; at that
// rate the serialise/parse pair, not the search, would dominate the cost of the boundary — for no
// benefit, because both sides already agree on IEEE-754 little-endian. So the host writes `f32`
// straight into linear memory at an offset [`idx_alloc`] hands out, exactly as [`idx_range_tally`]
// already takes its `f64` edges.
//
// The corollary is a real constraint and is stated rather than hidden:
// `docs/research/portability.md` §1 records that an mmap-style format does not *fail* to port to
// WASM, it silently reads the whole index into linear memory — and a vector column is the largest
// thing this engine has ever put there. `js/image-smoke.mjs` therefore reports the linear-memory
// high-water mark, not just latency.

use index_image::{FusedQuery, Hash64, ImageDoc, ImageIndex, ImageIndexBuilder, Metric};

/// Bytes per image hit record: `u32 doc`, `f32 score`, `u32 why`. Little-endian, packed.
///
/// Deliberately the same width as [`HIT_BYTE`], with `why` occupying the slot `typo_bucket` holds
/// in a text hit — a host that already decodes text hits changes one field name, not its decoder.
const IMAGE_HIT_BYTE: usize = 12;

/// Bytes per near-duplicate record: `u32 doc`, `u32 distance`. Little-endian, packed.
const IMAGE_NEAR_BYTE: usize = 8;

/// `why` bit: the text/facet/range arm accepted this document.
const WHY_TEXT: u32 = 1;
/// `why` bit: a vector neighbour within the shortlist.
const WHY_VECTOR: u32 = 2;
/// `why` bit: a perceptual-hash near-duplicate.
const WHY_HASH: u32 = 4;
/// `why` bit: a colour-bucket match. Reserved — no arm sets it yet, and the bit is defined now so
/// that adding one later is not an ABI break.
const WHY_COLOR: u32 = 8;

/// An image index, in one of its two lifetimes: under construction, then immutable.
///
/// One handle rather than two, because the caller has one thing — an index — and a builder that
/// must be separately freed is one more object to leak from a host language with no destructors.
/// [`idx_image_build`] flips the handle over; every query before it returns a sentinel, and every
/// mutation after it does too.
///
/// Public only because it appears in the exported signatures. It is **opaque**: its layout is not
/// part of the ABI.
pub struct ImageHandle {
    builder: Option<ImageIndexBuilder>,
    index: Option<ImageIndex>,
    /// The embedding staged by [`idx_image_push_vector`], consumed by the next [`idx_image_add`].
    pending: Option<Vec<f32>>,
    /// vector slot -> document ordinal, recorded at ingest.
    ///
    /// `ImageIndex` maps slots back to documents internally for a fused query but does not expose
    /// the mapping, and [`idx_image_search_vector`] goes to the vector column directly so that the
    /// caller's `oversample` is genuinely honoured rather than quietly replaced by the default.
    /// Recording the pair here at `add` time is cheaper and more honest than reconstructing it.
    doc_of_slot: Vec<u32>,
    /// Embedding width fixed at [`idx_image_new`]. 0 for a corpus with no embeddings at all.
    dim: usize,
    result: Vec<u8>,
    /// The `why` mask of each hit in `result`, so [`idx_image_why`] need not re-decode.
    why: Vec<u32>,
}

impl ImageHandle {
    /// Write fused-shaped records and remember their `why` masks.
    fn write_image_hit(&mut self, hit: &[(u32, f32, u32)]) {
        self.result.clear();
        self.result.reserve(hit.len() * IMAGE_HIT_BYTE);
        self.why.clear();
        self.why.reserve(hit.len());
        for &(doc, score, why) in hit {
            self.result.extend_from_slice(&doc.to_le_bytes());
            self.result.extend_from_slice(&score.to_le_bytes());
            self.result.extend_from_slice(&why.to_le_bytes());
            self.why.push(why);
        }
    }
}

/// Create an image index builder.
///
/// `ptr`/`len` is a NUL-separated field spec. Each field is `name:boost:b`, exactly the grammar
/// [`idx_build_new`] takes, with **one optional fourth component** declaring the column type:
///
/// - `caption:3:0.4`      — a scored text field
/// - `camera:1:0.6:f`     — also a **facet**
/// - `year:0:0.6:n`       — also a **numeric** column
///
/// Facet and numeric slots are numbered in field order, so `"a:1:0.4\0b:1:0.4:f\0c:1:0.4:f"` gives
/// facet slot 0 to `b` and slot 1 to `c`. Declaring the column type inside the spec rather than
/// through separate `idx_image_facet` / `idx_image_numeric` calls is deliberate: on the text side
/// those calls must precede the first document and silently return 0 afterwards, which is an
/// ordering rule a host can get wrong. Here the schema is one argument and cannot be half-applied.
///
/// `dim` is the embedding width, or 0 for a corpus with no embeddings — such an index is still
/// fully searchable by text, facet, range and hash, which is what lets a first result appear before
/// an embedding pass has finished (`docs/research/image.md` §8: 15 hours for 84,725 images).
///
/// `metric` is 0 cosine, 1 dot, 2 L2. Every metric is reported **higher is better**, so one
/// k-selection ranks them all.
///
/// Returns null on a malformed spec, an empty spec, too many fields, or an unknown metric.
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_image_new(
    ptr: *const u8,
    len: usize,
    dim: usize,
    metric: u32,
) -> *mut ImageHandle {
    if ptr.is_null() || len == 0 {
        return std::ptr::null_mut();
    }
    let Ok(spec) = std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) else {
        return std::ptr::null_mut();
    };
    let metric = match metric {
        0 => Metric::Cosine,
        1 => Metric::Dot,
        2 => Metric::L2,
        _ => return std::ptr::null_mut(),
    };

    let mut field = Vec::new();
    let mut facet = Vec::new();
    let mut numeric = Vec::new();
    for part in spec.split('\0').filter(|s| !s.is_empty()) {
        let mut it = part.split(':');
        let (Some(name), Some(boost), Some(b)) = (it.next(), it.next(), it.next()) else {
            return std::ptr::null_mut();
        };
        let (Ok(boost), Ok(b)) = (boost.parse::<f32>(), b.parse::<f32>()) else {
            return std::ptr::null_mut();
        };
        match it.next() {
            None => {}
            Some("f") => facet.push(field.len()),
            Some("n") => numeric.push(field.len()),
            // An unrecognised column type is refused rather than ignored: silently dropping an `f`
            // the caller typed as `F` would build an index whose filter bar matches nothing.
            Some(_) => return std::ptr::null_mut(),
        }
        if it.next().is_some() {
            return std::ptr::null_mut();
        }
        field.push(Field::new(name, boost, b));
    }
    if field.is_empty() || field.len() > index_text::MAX_FIELD {
        return std::ptr::null_mut();
    }

    let mut builder = ImageIndexBuilder::new(Schema::new(field), dim, metric);
    for f in facet {
        builder = builder.with_facet(f);
    }
    for n in numeric {
        builder = builder.with_numeric(n);
    }
    Box::into_raw(Box::new(ImageHandle {
        builder: Some(builder),
        index: None,
        pending: None,
        doc_of_slot: Vec::new(),
        dim,
        result: Vec::new(),
        why: Vec::new(),
    }))
}

/// Stage the embedding that the **next** [`idx_image_add`] will attach.
///
/// `ptr` points at `len` **`f32`** in linear memory — not bytes, not JSON. See this section's note:
/// 2 KB raw against ~6 KB of text that must then be parsed, at an ingest rate where serialisation
/// would dominate the boundary for no benefit.
///
/// Returns 1 on success, 0 on a null handle, a null pointer, a zero length, an index already built,
/// a column created with `dim = 0`, a length that disagrees with `dim`, or a non-finite component.
/// A dimension mismatch is refused **here**, at the call that can name both numbers, rather than
/// surfacing later as a failed `add` — in a real ingest that mismatch means a model was swapped
/// mid-run, and the operator needs telling, not crashing.
///
/// Staging rather than taking a document ordinal is what keeps the pair atomic: an ordinal
/// parameter would let a host attach an embedding to a document that does not exist yet, and the
/// only honest answer to that is an error the host would then have to unwind.
///
/// # Safety
/// `h` must be a live handle; `ptr` readable for `len` **`f32`**, i.e. `len * 4` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_image_push_vector(
    h: *mut ImageHandle,
    ptr: *const f32,
    len: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return 0 };
    if ptr.is_null() || len == 0 || handle.builder.is_none() {
        return 0;
    }
    if handle.dim == 0 || len != handle.dim {
        return 0;
    }
    let v = std::slice::from_raw_parts(ptr, len);
    if v.iter().any(|x| !x.is_finite()) {
        return 0;
    }
    handle.pending = Some(v.to_vec());
    1
}

/// Add one image: its text fields, its perceptual hashes, and its content digest.
///
/// `ptr`/`len` is a NUL-separated list of field values positionally aligned with the schema, the
/// same wire form [`idx_build_add`] takes.
///
/// A 64-bit hash crosses as **two `u32` halves** rather than as one `u64`, because a WASM `i64`
/// surfaces in JavaScript as a `BigInt` — a second numeric type in the host's hot ingest loop, for
/// a value that is a bit pattern rather than a number. `present` says which hashes are real:
/// bit 0 `dhash`, bit 1 `phash`. An absent hash is genuinely absent, not zero: a zero hash is a
/// legitimate all-dark image and would otherwise become a near-duplicate of every other document
/// nobody hashed.
///
/// `digest`/`digest_len` is the SHA-256 of the ORIGINAL bytes — the dedup key. Null means "not
/// computed" and stores the zero digest; a non-null pointer whose length is not exactly 32 is an
/// error, because a truncated digest that compares equal to nothing is worse than none.
///
/// Any embedding staged by [`idx_image_push_vector`] is consumed here whether or not this call
/// succeeds — a staged vector never leaks into the following document.
///
/// Returns the document ordinal, or `u32::MAX` on any error.
///
/// # Safety
/// `h` must be a live handle; `ptr` readable for `len` bytes; `digest` null or readable for
/// `digest_len` bytes.
#[no_mangle]
// One image is genuinely this many independent columns; the alternative is a host-visible struct
// whose layout, padding and all, would then be part of the ABI.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn idx_image_add(
    h: *mut ImageHandle,
    ptr: *const u8,
    len: usize,
    present: u32,
    dhash_hi: u32,
    dhash_lo: u32,
    phash_hi: u32,
    phash_lo: u32,
    digest: *const u8,
    digest_len: usize,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    // Taken unconditionally: a staged vector must not survive a rejected document.
    let pending = handle.pending.take();
    if handle.builder.is_none() || ptr.is_null() {
        return u32::MAX;
    }
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) else {
        return u32::MAX;
    };

    let mut key = [0u8; 32];
    if !digest.is_null() {
        if digest_len != 32 {
            return u32::MAX;
        }
        key.copy_from_slice(std::slice::from_raw_parts(digest, 32));
    }

    let join = |hi: u32, lo: u32| Hash64((u64::from(hi) << 32) | u64::from(lo));
    let image = ImageDoc {
        digest: key,
        dhash: (present & 1 != 0).then(|| join(dhash_hi, dhash_lo)),
        phash: (present & 2 != 0).then(|| join(phash_hi, phash_lo)),
        ..ImageDoc::default()
    };

    let field: Vec<&str> = text.split('\0').collect();
    let builder = handle.builder.as_mut().expect("checked above");
    match builder.add(&image, &Doc::new(field), pending.as_deref()) {
        Ok(id) => {
            if pending.is_some() {
                handle.doc_of_slot.push(id);
            }
            id
        }
        Err(_) => u32::MAX,
    }
}

/// Finish building. Returns 1 on success, 0 on a null handle, an already-built index, or a build
/// failure.
///
/// The handle survives either way, so a host has nothing extra to free on the error path. After a
/// success every `add` / `push_vector` returns its refusal sentinel and every query starts working;
/// before it, the reverse.
///
/// # Safety
/// `h` must be a live handle from [`idx_image_new`].
#[no_mangle]
pub unsafe extern "C" fn idx_image_build(h: *mut ImageHandle) -> u32 {
    let Some(handle) = h.as_mut() else { return 0 };
    let Some(builder) = handle.builder.take() else { return 0 };
    match builder.build() {
        Ok(index) => {
            handle.index = Some(index);
            1
        }
        Err(_) => 0,
    }
}

/// Documents in a built image index. 0 for a null handle or one still under construction.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_image_doc_count(h: *const ImageHandle) -> u32 {
    h.as_ref().and_then(|x| x.index.as_ref()).map_or(0, |i| i.doc_count() as u32)
}

/// **Vector search alone**: the `p58` tiered pipeline — binary popcount shortlist, int8 rerank,
/// exact final pass — with no text, facet or range predicate.
///
/// `ptr`/`len` is the query embedding as `len` **`f32`**. `oversample` is the shortlist multiplier;
/// 0 is treated as 1, and 4 is the measured recommendation (Qdrant's figure: a 3–4x oversampled
/// rerank recovered recall to 0.98–0.9966). It is a real parameter here rather than a fixed default
/// because a caller trading recall against latency has nowhere else to turn the dial.
///
/// Writes `IDX_IMAGE_HIT_BYTE` records with `why = IDX_WHY_VECTOR` and returns the count, or
/// `u32::MAX` on a null handle, an index not yet built, a null pointer, or a length that disagrees
/// with the column's dimension. `k == 0` and a corpus with no embeddings return 0.
///
/// **This applies no filter**, and that is exactly why it is separate from
/// [`idx_image_search_fused`]: a host wanting a filtered vector search must ask for one, so the
/// filter cannot be lost by accident. A vector arm returning documents the filter bar excluded is
/// the defect `p59` guards, and it is not a defect this entry point can silently commit.
///
/// # Safety
/// `h` must be a live handle; `ptr` readable for `len` `f32`.
#[no_mangle]
pub unsafe extern "C" fn idx_image_search_vector(
    h: *mut ImageHandle,
    ptr: *const f32,
    len: usize,
    k: u32,
    oversample: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if ptr.is_null() || handle.index.is_none() || handle.dim == 0 || len != handle.dim {
        return u32::MAX;
    }
    if k == 0 {
        handle.result.clear();
        handle.why.clear();
        return 0;
    }
    let query = std::slice::from_raw_parts(ptr, len).to_vec();
    let index = handle.index.as_ref().expect("checked above");
    let hit: Vec<(u32, f32, u32)> = index
        .vector()
        .search(&query, k as usize, oversample as usize)
        .into_iter()
        // A slot with no document is impossible by construction, so it is dropped rather than
        // defaulted: a silent 0 here would attribute someone else's embedding to document 0.
        .filter_map(|(slot, score)| {
            handle.doc_of_slot.get(slot as usize).map(|&doc| (doc, score, WHY_VECTOR))
        })
        .collect();
    handle.write_image_hit(&hit);
    hit.len() as u32
}

/// **The fused query**: text, facets, numeric ranges, vector proximity and hash proximity, scored
/// together with top-k selected **once**.
///
/// This is `p59` made callable from JavaScript, and the property it asserts is not speed:
///
/// - **Hard predicates are hard everywhere.** Facet and range clauses are applied inside the text
///   pass *and* re-applied to the vector and hash arms. A soft arm knows nothing about facets, so
///   it must be filtered rather than trusted; skipping that step is precisely how a vector search
///   leaks a document the filter bar excluded.
/// - **No short page.** Each soft arm over-generates before fusion, so a document ranked just
///   outside every arm's top-`k` is still reachable — the failure a fan-out architecture cannot fix
///   at the intersection layer, because both subsystems discarded the document before it ran.
///
/// Every predicate is optional:
///
/// - `q`/`q_len` — free text, BM25F. Null or empty drops the text arm.
/// - `spec`/`spec_len` — the `slot[!]=v1|v2|...` filter-bar grammar [`idx_search_clause`] takes,
///   parsed by the **same** code, so one spec cannot mean two things. Null or empty means no facet
///   predicate; a malformed spec is `u32::MAX`, never a silent match-everything.
/// - `range`/`range_n` — `range_n` triples of `f64` laid out as `slot, lo, hi`, half-open
///   `lo <= v < hi`. `f64` for the slot too, so the array is one homogeneous buffer a host writes
///   with a single `Float64Array`; a whole number below 2^53 is exact.
/// - `vec`/`vec_len` — the query embedding as `f32`. A length disagreeing with `dim` is an error,
///   not a silently dropped arm.
/// - `hash_present`, `hash_hi`, `hash_lo`, `hash_max` — a dHash probe and its Hamming radius. The
///   radius is the caller's: a threshold measured for one code length does not port to another, so
///   a constant here would trap anyone switching hash widths.
/// - `alpha` — weight of the text arm; the soft arms share the remainder. Negative means "use the
///   default", which is 0.5 and asserts nothing about which signal is better — the honest position
///   before a labelled judgment set exists.
///
/// Writes `IDX_IMAGE_HIT_BYTE` records and returns the count, or `u32::MAX` on error. A query with
/// no predicate at all is a legitimate match-everything, because a corpus where most documents
/// carry almost no signal is the normal case rather than the exception.
///
/// # Safety
/// `h` must be a live handle; every non-null pointer readable for its stated length.
#[no_mangle]
// The predicate set IS the argument list; collapsing it into a host-visible struct would put that
// struct's layout, padding and all, into the ABI.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn idx_image_search_fused(
    h: *mut ImageHandle,
    q: *const u8,
    q_len: usize,
    spec: *const u8,
    spec_len: usize,
    range: *const f64,
    range_n: usize,
    vec: *const f32,
    vec_len: usize,
    hash_present: u32,
    hash_hi: u32,
    hash_lo: u32,
    hash_max: u32,
    alpha: f32,
    k: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if handle.index.is_none() {
        return u32::MAX;
    }

    let text: Option<&str> = if q.is_null() || q_len == 0 {
        None
    } else {
        match std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) {
            Ok(t) => Some(t),
            Err(_) => return u32::MAX,
        }
    };

    // Values are borrowed from `spec_text`, so the parse must outlive the search call.
    let spec_text = if spec.is_null() || spec_len == 0 {
        ""
    } else {
        match std::str::from_utf8(std::slice::from_raw_parts(spec, spec_len)) {
            Ok(t) => t,
            Err(_) => return u32::MAX,
        }
    };
    let Some(parsed) = parse_clause_spec(spec_text) else { return u32::MAX };
    let clause = borrow_clause(&parsed);

    let mut bound: Vec<(usize, f64, f64)> = Vec::new();
    if range_n > 0 {
        if range.is_null() {
            return u32::MAX;
        }
        for t in std::slice::from_raw_parts(range, range_n * 3).chunks_exact(3) {
            // A slot that is not a non-negative whole number is a host bug, not a filter: refuse it
            // rather than truncate it into some other column's range.
            if !(t[0].is_finite() && t[0] >= 0.0 && t[0].fract() == 0.0) {
                return u32::MAX;
            }
            bound.push((t[0] as usize, t[1], t[2]));
        }
    }

    let embed: Option<Vec<f32>> = if vec.is_null() || vec_len == 0 {
        None
    } else {
        if handle.dim == 0 || vec_len != handle.dim {
            return u32::MAX;
        }
        Some(std::slice::from_raw_parts(vec, vec_len).to_vec())
    };

    let probe = (hash_present != 0)
        .then(|| (Hash64((u64::from(hash_hi) << 32) | u64::from(hash_lo)), hash_max));

    let query = FusedQuery {
        text,
        facet: &clause,
        range: &bound,
        vector: embed.as_deref(),
        hash: probe,
        alpha: (alpha >= 0.0).then_some(alpha),
    };
    let index = handle.index.as_ref().expect("checked above");
    let hit: Vec<(u32, f32, u32)> = index
        .search_fused(&query, k as usize)
        .into_iter()
        .map(|x| {
            let mut why = 0;
            if x.why.text {
                why |= WHY_TEXT;
            }
            if x.why.vector {
                why |= WHY_VECTOR;
            }
            if x.why.hash {
                why |= WHY_HASH;
            }
            if x.why.color {
                why |= WHY_COLOR;
            }
            (x.doc, x.score, why)
        })
        .collect();
    handle.write_image_hit(&hit);
    hit.len() as u32
}

/// Near-duplicates of a dHash probe within a Hamming radius, over the whole hash column.
///
/// The probe crosses as two `u32` halves, for the reason [`idx_image_add`] gives. Writes
/// `IDX_IMAGE_NEAR_BYTE` records — `u32 doc`, `u32 distance` — ordered by ascending distance then
/// ascending ordinal, and returns the count, or `u32::MAX` on a null handle or an index not yet
/// built. A document with no dHash appears in no result at all.
///
/// `max` is the caller's radius rather than a constant, because a threshold measured for a 64-bit
/// code does not port to a 256-bit one; 4 is the measured default for this width.
///
/// This writes to the SAME buffer the searches use, in a **different** record layout, so the count
/// this returns is what tells a host which decoder to run. Read it before the next call.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_image_hash_near(h: *mut ImageHandle, hi: u32, lo: u32, max: u32) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    let Some(index) = handle.index.as_ref() else { return u32::MAX };
    let near = index.hash_near(Hash64((u64::from(hi) << 32) | u64::from(lo)), max);
    handle.result.clear();
    handle.result.reserve(near.len() * IMAGE_NEAR_BYTE);
    handle.why.clear();
    for (doc, distance) in &near {
        handle.result.extend_from_slice(&doc.to_le_bytes());
        handle.result.extend_from_slice(&distance.to_le_bytes());
    }
    near.len() as u32
}

/// Which signal found hit `hit` of the last search — the explicability `p59` requires, preserved
/// across the boundary.
///
/// A bitmask of `IDX_WHY_TEXT` | `IDX_WHY_VECTOR` | `IDX_WHY_HASH` | `IDX_WHY_COLOR`. Returns
/// `u32::MAX` past the end of the last result or on a null handle — never 0, which is a legitimate
/// mask meaning "no arm claimed it" and must stay distinguishable from an error.
///
/// The mask is also the third field of every hit record, so a host decoding records itself never
/// needs this call. It exists for the host that wants one number about one row without writing a
/// decoder — a Python or PHP binding, typically, where unpacking a packed struct out of a `bytes`
/// is the fiddliest part of the whole ABI.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_image_why(h: *const ImageHandle, hit: u32) -> u32 {
    h.as_ref().and_then(|x| x.why.get(hit as usize).copied()).unwrap_or(u32::MAX)
}

/// Pointer to the last image result. Invalidated by the next call on the same handle.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_image_result_ptr(h: *const ImageHandle) -> *const u8 {
    h.as_ref().map_or(std::ptr::null(), |x| x.result.as_ptr())
}

/// Byte length of the last image result.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_image_result_len(h: *const ImageHandle) -> usize {
    h.as_ref().map_or(0, |x| x.result.len())
}

/// Release an image handle, built or not. Nothing else frees it.
///
/// # Safety
/// `h` must have come from [`idx_image_new`] and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn idx_image_free(h: *mut ImageHandle) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

// ---- The range tier: querying an index without materialising it ---------------------------------
//
// `bench/roadmap/p56-ten-million.md` measures a 10 M-document index at ~910 MB, and
// `p68-opfs-tier.md` closed with the honest complaint that a browser could only *open* such a file,
// never query it: the worker proved a section is range-readable, but `idx_open` still took the
// whole buffer, so the tab still had to hold all 910 MB.
//
// # Why a PREFETCH PLAN and not a lazy handle
//
// The other credible shape is a lazy handle -- the module keeps the section table plus a host
// callback for "give me bytes [at, len)" and pulls spans as the searcher walks. It was rejected on
// three counts, and the first is fatal on its own:
//
// 1. **The bytes are not reachable from inside a WASM call.** `createSyncAccessHandle().read()` is
//    worker-only, and a Rust -> JS callback that must await an OPFS handle cannot return
//    synchronously. A lazy handle therefore needs either JSPI/Asyncify (a build-mode dependency and
//    a whole-module rewrite) or a SharedArrayBuffer and `Atomics.wait` (cross-origin isolation,
//    which most consumers of this engine cannot turn on). Neither is a property this ABI should
//    require of every host.
// 2. **Reentrancy into JS from Rust is the one thing this ABI has never done.** A host that throws
//    inside the callback unwinds through the module, which is the failure this crate spends its
//    entire error model avoiding.
// 3. A plan is inspectable. The host can *count the bytes it read*, which is the claim being made;
//    a callback hides that count inside the module.
//
// The plan costs one extra round trip per query -- plan, fetch, search -- and buys a boundary that
// is still a pure `(ptr, len)` in and `(ptr, len)` out.
//
// # What is resident and what is not
//
// The plan splits the eighteen sections in two. **Resident**: everything whose size is bounded by
// the document count or the term count -- meta, schema, alias, dictionary, posting-offset array,
// document lengths, priors, anchors, deletions, expansions, facets, numeric columns, keys.
// **Not resident**: `posting`, and the two position sections. Postings are the bulk (78.6 % of the
// shipped profstopick artifact, and that fraction only grows with the corpus) and they are the one
// section addressable per term, which is exactly what `format::posting_span` exists to compute.
//
// Positions are dropped rather than fetched, so `search_phrase` is not available on a range handle.
// That is stated rather than faked: a phrase query answered from a bag of words is a wrong answer
// that looks right, and this file already refuses that trade in [`idx_search_phrase`].
//
// # How a partial file is made openable at all
//
// `Index::from_bytes` validates every posting list against the offset array, so it cannot be handed
// a file with holes. Instead the module ASSEMBLES a smaller, entirely valid index image: the
// resident sections verbatim, a posting section holding only the fetched lists, and a rewritten
// posting-offset array in which every unfetched term is a zero-length span. Nothing in the reader
// has to learn about partial files, and the assembled index is genuinely well-formed -- it simply
// knows about fewer posting lists than the file it came from.
//
// # Why the answer is EXACT and not an approximation
//
// The planner asks the resident index -- whose postings are all empty -- for `term_stat`, the same
// dictionary expansion `plan_stat` performs for a real search and deliberately without the
// expansion cap. Dictionary expansion depends only on the FST, so the planned term set is a
// superset of what the search will scan. Every fetched list then carries its true length, so the
// `df` that orders and caps the expansions, and the `df` that scores them, are the file's own.
//
// The single place the two could disagree is `split_compound`, which breaks ties by document
// frequency -- and the planner sees zeroes there. So the search VERIFIES: it re-runs `term_stat` on
// the assembled index and returns `u32::MAX` if any term it would scan was not fetched. A host that
// sees that sentinel falls back to a full open. Answering from a term set that is silently missing
// a list is the failure this refuses to have.

/// Bytes a reader needs from the head of a file to learn where all eighteen sections live:
/// `MAGIC.len()` plus eighteen spans of two little-endian `u64`.
///
/// Pinned against `read_section_table` by `the_layout_of_any_index_costs_296_bytes` rather than
/// trusted, because `format.rs` owns the real constant and this one must not drift from it.
const RANGE_HEAD_BYTE: usize = 8 + 18 * 16;

/// Sections in a section table.
const SECTION_COUNT: usize = 18;

/// Bytes per span record in a plan: `u64` offset, `u64` length, little-endian.
const SPAN_BYTE: usize = 16;

/// Table slot of `posting_offset` -- the array `posting_span` addresses lists through.
const SLOT_POSTING_OFFSET: usize = 4;
/// Table slot of `posting`: the one section fetched per query rather than at open.
const SLOT_POSTING: usize = 5;
/// Table slot of `position_at`, dropped entirely on a range handle.
const SLOT_POSITION_AT: usize = 15;
/// Table slot of `position`, dropped for the same reason.
const SLOT_POSITION: usize = 16;

/// An index opened from its 296-byte head, plus whichever sections a host has since fetched for it.
///
/// Opaque, like [`Handle`]; its layout is not part of the ABI.
pub struct RangeHandle {
    /// Where every section lives **in the original file**, which is what the host reads from.
    table: SectionTable,
    /// Resident section bytes by table slot. `posting` and the two position slots stay empty.
    part: Vec<Vec<u8>>,
    /// The file's own posting-offset array, kept verbatim: the assembled image gets a rewritten
    /// one, but `posting_span` must keep addressing the FILE.
    posting_offset: Vec<u8>,
    term_count: usize,
    /// The index over the resident sections alone. It answers no query -- every posting list is
    /// empty -- but it holds the dictionary, the aliases and the expansion table, which is what
    /// planning needs.
    resident: Option<Index>,
    /// `(term id, span in the file)` for the last planned query, sorted by term id, deduplicated.
    plan: Vec<(u32, Span)>,
    result: Vec<u8>,
}

/// The `i`th span of a section table, in table order.
fn section_span(t: &SectionTable, i: usize) -> Span {
    match i {
        0 => t.meta,
        1 => t.schema,
        2 => t.alias,
        3 => t.dict,
        4 => t.posting_offset,
        5 => t.posting,
        6 => t.doc_len,
        7 => t.prior,
        8 => t.first_term,
        9 => t.deleted,
        10 => t.expansion,
        11 => t.facet_label,
        12 => t.facet_id,
        13 => t.numeric_field,
        14 => t.numeric_value,
        15 => t.position_at,
        16 => t.position,
        _ => t.doc_key,
    }
}

/// Whether slot `i` is fetched once at open rather than per query.
fn is_resident(i: usize) -> bool {
    !matches!(i, SLOT_POSTING | SLOT_POSITION_AT | SLOT_POSITION)
}

/// Lay eighteen sections out as a valid index file, backfilling the section table.
///
/// Order is table order, which is also the order `to_bytes` writes -- but nothing depends on that:
/// `from_bytes` reads every section through its span, so this could pack them in any order at all.
fn assemble(part: &[Vec<u8>]) -> Vec<u8> {
    let total: usize = part.iter().map(|p| p.len()).sum();
    let mut out = Vec::with_capacity(RANGE_HEAD_BYTE + total);
    out.extend_from_slice(&index_text::MAGIC);
    out.resize(RANGE_HEAD_BYTE, 0);
    let mut table = Vec::with_capacity(SECTION_COUNT * SPAN_BYTE);
    for p in part {
        let at = out.len() as u64;
        out.extend_from_slice(p);
        table.extend_from_slice(&at.to_le_bytes());
        table.extend_from_slice(&(p.len() as u64).to_le_bytes());
    }
    out[index_text::MAGIC.len()..RANGE_HEAD_BYTE].copy_from_slice(&table);
    out
}

impl RangeHandle {
    /// Write `span` into the result buffer as the plan the host is to fetch.
    fn write_plan(&mut self, span: &[Span]) {
        self.result.clear();
        self.result.reserve(span.len() * SPAN_BYTE);
        for s in span {
            self.result.extend_from_slice(&s.offset.to_le_bytes());
            self.result.extend_from_slice(&s.len.to_le_bytes());
        }
    }

    /// The posting section and its offset array for an image holding only the planned lists.
    ///
    /// `fetched` is the concatenation of the planned spans, in plan order — which is term-id order,
    /// so it is consumed by a single forward cursor as the terms are walked.
    ///
    /// **An unfetched term gets a ONE-BYTE list, not a zero-length span.** Since `IDXTXT10` every
    /// posting list is self-describing and opens with a varint count, so "this term has no
    /// postings" is the byte `0x00` — and a zero-length span is not an empty list but an unreadable
    /// one, which `from_bytes` correctly refuses. That distinction did not exist under the
    /// fixed-width format this was first written against, where an empty list genuinely was zero
    /// bytes; it is the one place the encoding change reaches into the range path.
    fn narrow(&self, fetched: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut posting = Vec::with_capacity(fetched.len() + self.term_count);
        let mut out = Vec::with_capacity((self.term_count + 1) * 8);
        let mut acc = 0u64;
        let mut next = 0usize;
        let mut at = 0usize;
        for i in 0..self.term_count {
            out.extend_from_slice(&acc.to_le_bytes());
            match self.plan.get(next) {
                Some((id, span)) if *id as usize == i => {
                    let n = span.len as usize;
                    // The caller has already checked that `fetched` is exactly the planned length;
                    // this guard keeps a short buffer from panicking rather than trusting that.
                    let Some(bytes) = fetched.get(at..at + n) else { break };
                    posting.extend_from_slice(bytes);
                    at += n;
                    acc += n as u64;
                    next += 1;
                }
                _ => {
                    posting.push(0);
                    acc += 1;
                }
            }
        }
        out.extend_from_slice(&acc.to_le_bytes());
        (posting, out)
    }

    /// Assemble an image from the resident sections plus `posting`, and open it.
    fn open_image(&self, posting: Vec<u8>, offset_array: Vec<u8>) -> Option<Index> {
        let mut part = self.part.clone();
        part[SLOT_POSTING_OFFSET] = offset_array;
        part[SLOT_POSTING] = posting;
        Index::from_bytes(&assemble(&part)).ok()
    }
}

/// Bytes of a file's head that [`idx_range_open`] requires. Constant for every index this format
/// has ever written and independent of the file's size -- that invariance is the whole point.
#[no_mangle]
pub extern "C" fn idx_range_head_byte() -> u32 {
    RANGE_HEAD_BYTE as u32
}

/// Bytes per span record in a plan: `u64` offset then `u64` length, little-endian.
#[no_mangle]
pub extern "C" fn idx_range_span_byte() -> u32 {
    SPAN_BYTE as u32
}

/// Open an index from the first [`idx_range_head_byte`] bytes of its file -- and **nothing else**.
///
/// Returns an opaque handle, or null if those bytes are not the head of an index this build reads.
/// The handle cannot answer anything yet: the host must run [`idx_range_plan`], fetch the spans it
/// names, and hand them back to [`idx_range_load`].
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_range_open(ptr: *const u8, len: usize) -> *mut RangeHandle {
    if ptr.is_null() || len < RANGE_HEAD_BYTE {
        return std::ptr::null_mut();
    }
    let head = std::slice::from_raw_parts(ptr, len);
    let Ok(table) = index_text::read_section_table(head) else { return std::ptr::null_mut() };
    Box::into_raw(Box::new(RangeHandle {
        table,
        part: vec![Vec::new(); SECTION_COUNT],
        posting_offset: Vec::new(),
        term_count: 0,
        resident: None,
        plan: Vec::new(),
        result: Vec::new(),
    }))
}

/// The spans a host must read to make `rh` queryable: everything except the postings and the
/// positions. Writes [`idx_range_span_byte`] records into the handle's result buffer and returns
/// how many, or `u32::MAX` on a null handle.
///
/// Fifteen records, always, including any that are zero-length -- a fixed count is a shape the host
/// can concatenate against without branching, and a zero-length read costs nothing.
///
/// # Safety
/// `rh` must be a live handle from [`idx_range_open`].
#[no_mangle]
pub unsafe extern "C" fn idx_range_plan(rh: *mut RangeHandle) -> u32 {
    let Some(h) = rh.as_mut() else { return u32::MAX };
    let span: Vec<Span> =
        (0..SECTION_COUNT).filter(|i| is_resident(*i)).map(|i| section_span(&h.table, i)).collect();
    h.write_plan(&span);
    span.len() as u32
}

/// Hand back the bytes [`idx_range_plan`] asked for, concatenated **in plan order**.
///
/// Returns 1 once the handle can plan queries, and 0 if the buffer is the wrong length, if the
/// posting-offset array is malformed, or if the resident sections do not form an index. A wrong
/// length is refused rather than split on a best guess: the sections are positional, so a short
/// buffer would silently reinterpret a dictionary as a length column.
///
/// # Safety
/// `rh` must be a live handle; `ptr` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_range_load(rh: *mut RangeHandle, ptr: *const u8, len: usize) -> u32 {
    let Some(h) = rh.as_mut() else { return 0 };
    if ptr.is_null() {
        return 0;
    }
    let slot: Vec<usize> = (0..SECTION_COUNT).filter(|i| is_resident(*i)).collect();
    let want: u64 = slot.iter().map(|i| section_span(&h.table, *i).len).sum();
    if want != len as u64 {
        return 0;
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    let mut at = 0usize;
    let mut part = vec![Vec::new(); SECTION_COUNT];
    for i in slot {
        let n = section_span(&h.table, i).len as usize;
        part[i] = bytes[at..at + n].to_vec();
        at += n;
    }

    let off = std::mem::take(&mut part[SLOT_POSTING_OFFSET]);
    if off.len() % 8 != 0 || off.len() < 16 {
        return 0;
    }
    h.term_count = off.len() / 8 - 1;
    h.posting_offset = off;
    h.part = part;
    // The resident image: every posting list present, every posting list empty. It answers no
    // query and is never asked one -- it holds the dictionary the planner expands against.
    h.plan.clear();
    let (empty_posting, empty_offset) = h.narrow(&[]);
    match h.open_image(empty_posting, empty_offset) {
        Some(index) => {
            h.resident = Some(index);
            1
        }
        None => 0,
    }
}

/// The posting spans `query` needs, in the file's own coordinates.
///
/// Writes [`idx_range_span_byte`] records into the result buffer and returns how many, or
/// `u32::MAX` on a null handle, a non-UTF-8 query, or a handle that has not been loaded. Zero is a
/// legitimate answer -- a query whose every token is absent from the dictionary reads nothing at
/// all and then matches nothing, which is the correct amount of I/O for it.
///
/// `prefix` non-zero plans for typeahead semantics on the final token, and must match the `prefix`
/// later passed to [`idx_range_search`]; planning one and searching the other is caught there
/// rather than answered.
///
/// # Safety
/// `rh` must be a live handle; `q` readable for `q_len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_range_plan_query(
    rh: *mut RangeHandle,
    q: *const u8,
    q_len: usize,
    prefix: u32,
) -> u32 {
    let Some(h) = rh.as_mut() else { return u32::MAX };
    if q.is_null() {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    let Some(index) = h.resident.as_ref() else { return u32::MAX };

    // Two sources, and both are decided by structures that are already resident, which is why a
    // plan can be exact: the dictionary FST decides the expansions, and the learned-expansion table
    // decides the rest. Neither consults a posting list.
    let mut term: Vec<u32> = index
        .term_stat(query, prefix != 0)
        .into_iter()
        .filter_map(|(text, _)| index.term_id_of(&text))
        .collect();
    term.extend_from_slice(index.expansion_of(query));
    term.sort_unstable();
    term.dedup();

    let mut plan = Vec::with_capacity(term.len());
    for id in term {
        match index_text::posting_span(&h.table, &h.posting_offset, id) {
            // A zero-length list is a term no document carries: nothing to read, nothing to score.
            Ok(span) if span.len > 0 => plan.push((id, span)),
            Ok(_) => {}
            Err(_) => return u32::MAX,
        }
    }
    let span: Vec<Span> = plan.iter().map(|(_, s)| *s).collect();
    h.plan = plan;
    h.write_plan(&span);
    span.len() as u32
}

/// Answer `query` from the posting bytes the last [`idx_range_plan_query`] asked for, concatenated
/// in plan order.
///
/// Writes 12-byte hit records into the result buffer -- **replacing the plan** -- and returns the
/// hit count, or `u32::MAX` on a null handle, a non-UTF-8 query, a buffer whose length does not
/// match the plan, an image that fails to assemble, or a plan this query has outrun.
///
/// The hits are the hits a full [`idx_open`] of the same file would return, and that is checked
/// rather than assumed: the assembled index is asked which terms it would scan, and any term the
/// host did not fetch aborts with the sentinel instead of scoring against a term set missing a
/// list. See this section's header for the one case that can arise.
///
/// # Safety
/// `rh` must be a live handle; `q` readable for `q_len` bytes; `ptr` readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn idx_range_search(
    rh: *mut RangeHandle,
    q: *const u8,
    q_len: usize,
    k: u32,
    prefix: u32,
    ptr: *const u8,
    len: usize,
) -> u32 {
    let Some(h) = rh.as_mut() else { return u32::MAX };
    if q.is_null() || (ptr.is_null() && len != 0) {
        return u32::MAX;
    }
    let Ok(query) = std::str::from_utf8(std::slice::from_raw_parts(q, q_len)) else {
        return u32::MAX;
    };
    if h.resident.is_none() {
        return u32::MAX;
    }
    let want: u64 = h.plan.iter().map(|(_, s)| s.len).sum();
    if want != len as u64 {
        return u32::MAX;
    }
    let posting = if len == 0 { Vec::new() } else { std::slice::from_raw_parts(ptr, len).to_vec() };
    let (narrow_posting, offset_array) = h.narrow(&posting);
    let Some(index) = h.open_image(narrow_posting, offset_array) else { return u32::MAX };

    // The verification. `term_stat` is the expansion `plan_stat` runs for the real search, so a
    // term it names that nothing fetched is a plan that no longer describes this query.
    for (text, _) in index.term_stat(query, prefix != 0) {
        let Some(id) = index.term_id_of(&text) else { return u32::MAX };
        // A term with no postings in the FILE either: nothing was owed for it.
        if index_text::posting_span(&h.table, &h.posting_offset, id).is_ok_and(|s| s.len == 0) {
            continue;
        }
        if h.plan.binary_search_by_key(&id, |(t, _)| *t).is_err() {
            return u32::MAX;
        }
    }

    let hit = if prefix != 0 {
        index.search_prefix(query, k as usize)
    } else {
        index.search(query, k as usize)
    };
    write_hit(&mut h.result, &hit);
    hit.len() as u32
}

/// Documents in the file `rh` was opened from, or 0 before [`idx_range_load`].
///
/// # Safety
/// `rh` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_range_doc_count(rh: *const RangeHandle) -> u32 {
    rh.as_ref().and_then(|h| h.resident.as_ref()).map_or(0, |x| x.doc_count() as u32)
}

/// Distinct terms in the file `rh` was opened from, or 0 before [`idx_range_load`]. Known from the
/// posting-offset array alone, without a single posting byte.
///
/// # Safety
/// `rh` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_range_term_count(rh: *const RangeHandle) -> u32 {
    rh.as_ref().map_or(0, |h| h.term_count as u32)
}

/// The last plan or the last result, whichever was written most recently. Invalidated by the next
/// call on this handle.
///
/// # Safety
/// `rh` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_range_result_ptr(rh: *const RangeHandle) -> *const u8 {
    match rh.as_ref() {
        Some(h) => h.result.as_ptr(),
        None => std::ptr::null(),
    }
}

/// Length in bytes of what [`idx_range_result_ptr`] points at, or 0 for a null handle.
///
/// # Safety
/// `rh` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn idx_range_result_len(rh: *const RangeHandle) -> usize {
    rh.as_ref().map_or(0, |h| h.result.len())
}

/// Release a range handle. Nothing else frees it.
///
/// # Safety
/// `rh` must have come from [`idx_range_open`] and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn idx_range_close(rh: *mut RangeHandle) {
    if !rh.is_null() {
        drop(Box::from_raw(rh));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use index_text::{Doc, Field, IndexBuilder, Schema};

    fn blob() -> Vec<u8> {
        let mut b = IndexBuilder::new(Schema::new(vec![Field::new("title", 1.0, 0.4)]));
        b.add(&Doc::new(["Colgate Total Charcoal 80g"]));
        b.add(&Doc::new(["Nescafe Classic Reseal 200g"]));
        b.add(&Doc::new(["Bear Brand Powdered Milk 300g"]));
        b.build().unwrap().to_bytes()
    }

    /// Exercise the ABI exactly as a host would: alloc, copy in, open, search, read, free.
    #[test]
    fn the_abi_round_trips_a_query() {
        assert_eq!(idx_abi_version(), 14);
        let bytes = blob();
        unsafe {
            let p = idx_alloc(bytes.len());
            assert!(!p.is_null());
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            let h = idx_open(p, bytes.len());
            assert!(!h.is_null(), "a valid index must open");
            idx_free(p, bytes.len()); // the host's copy is no longer needed
            assert_eq!(idx_doc_count(h), 3);

            let q = b"colgaye"; // a typo
            let qp = idx_alloc(q.len());
            std::ptr::copy_nonoverlapping(q.as_ptr(), qp, q.len());
            let n = idx_search(h, qp, q.len(), 5, 0);
            idx_free(qp, q.len());
            assert!(n > 0 && n != u32::MAX, "typo query must return hits, got {n}");

            let rp = idx_result_ptr(h);
            assert_eq!(idx_result_len(h), n as usize * HIT_BYTE);
            let doc = u32::from_le_bytes(std::slice::from_raw_parts(rp, 4).try_into().unwrap());
            assert_eq!(doc, 0, "the Colgate document must rank first");
            idx_close(h);
        }
    }

    /// Every failure mode must return a sentinel, never trap. A trap in WASM kills the whole
    /// module instance and takes the host page with it.
    #[test]
    fn bad_input_returns_sentinels_rather_than_trapping() {
        unsafe {
            assert!(idx_open(std::ptr::null(), 10).is_null());
            assert!(idx_open(b"garbage!".as_ptr(), 8).is_null(), "bad magic must not open");
            assert_eq!(idx_search(std::ptr::null_mut(), b"x".as_ptr(), 1, 5, 0), u32::MAX);
            assert_eq!(idx_doc_count(std::ptr::null()), 0);
            assert_eq!(idx_result_len(std::ptr::null()), 0);
            assert!(idx_alloc(0).is_null());
            idx_free(std::ptr::null_mut(), 0); // must not panic
            idx_close(std::ptr::null_mut()); // must not panic

            // Invalid UTF-8 in the query.
            let bytes = blob();
            let p = idx_alloc(bytes.len());
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            let h = idx_open(p, bytes.len());
            idx_free(p, bytes.len());
            let bad = [0xffu8, 0xfe];
            let bp = idx_alloc(2);
            std::ptr::copy_nonoverlapping(bad.as_ptr(), bp, 2);
            assert_eq!(idx_search(h, bp, 2, 5, 0), u32::MAX);
            idx_free(bp, 2);
            idx_close(h);
        }
    }

    /// Faceting across the ABI: build with a facet, filter, and tally.
    #[test]
    fn the_abi_facets() {
        unsafe {
            let spec = b"name:3:0.4 brand:1:0.6";
            let sp = idx_alloc(spec.len());
            std::ptr::copy_nonoverlapping(spec.as_ptr(), sp, spec.len());
            let b = idx_build_new(sp, spec.len());
            idx_free(sp, spec.len());
            assert!(!b.is_null());
            assert_eq!(idx_build_facet(b, 1), 1, "field 1 is a valid facet");
            assert_eq!(idx_build_facet(b, 99), 0, "out of range is refused, not trapped");

            for row in [
                &b"Colgate Total Toothpaste 150g Colgate"[..],
                &b"Colgate Fresh Gel 100g Colgate"[..],
                &b"Bear Brand Powdered Milk 300g Bear Brand"[..],
            ] {
                let p = idx_alloc(row.len());
                std::ptr::copy_nonoverlapping(row.as_ptr(), p, row.len());
                assert_ne!(idx_build_add(b, p, row.len()), u32::MAX);
                idx_free(p, row.len());
            }
            let h = idx_build_finish(b);
            assert!(!h.is_null());
            assert_eq!(idx_facet_slot_count(h), 1, "one facet slot");
            assert_eq!(idx_facet_count(h, 0), 2, "two distinct brands");

            let q = b"Colgate";
            let qp = idx_alloc(q.len());
            std::ptr::copy_nonoverlapping(q.as_ptr(), qp, q.len());

            let v = b"Colgate";
            let vp = idx_alloc(v.len());
            std::ptr::copy_nonoverlapping(v.as_ptr(), vp, v.len());
            assert_eq!(idx_search_facet(h, qp, q.len(), 10, 0, vp, v.len()), 2);
            idx_free(vp, v.len());

            let miss = b"Nestle";
            let mp = idx_alloc(miss.len());
            std::ptr::copy_nonoverlapping(miss.as_ptr(), mp, miss.len());
            assert_eq!(
                idx_search_facet(h, qp, q.len(), 10, 0, mp, miss.len()),
                0,
                "an unknown facet value returns nothing, not everything"
            );
            idx_free(mp, miss.len());

            // Tally: u32 len, bytes, u32 count -- decoded here exactly as a host would.
            let n = idx_facet_tally(h, qp, q.len(), 0);
            assert!(n >= 1);
            let raw = std::slice::from_raw_parts(idx_result_ptr(h), idx_result_len(h));
            let mut at = 0usize;
            let mut top = None;
            for _ in 0..n {
                let l = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap()) as usize;
                at += 4;
                let label = std::str::from_utf8(&raw[at..at + l]).unwrap().to_string();
                at += l;
                let c = u32::from_le_bytes(raw[at..at + 4].try_into().unwrap());
                at += 4;
                if top.is_none() {
                    top = Some((label, c));
                }
            }
            assert_eq!(at, raw.len(), "the tally encoding is self-delimiting");
            assert_eq!(top, Some(("Colgate".to_string(), 2)));

            // Conjunctive spec, parsed by the ABI rather than by the host.
            let spec = b"0=Colgate";
            let sp2 = idx_alloc(spec.len());
            std::ptr::copy_nonoverlapping(spec.as_ptr(), sp2, spec.len());
            assert_eq!(idx_search_facet_all(h, qp, q.len(), 10, sp2, spec.len()), 2);
            idx_free(sp2, spec.len());

            let bad = b"notanumber=Colgate";
            let bp2 = idx_alloc(bad.len());
            std::ptr::copy_nonoverlapping(bad.as_ptr(), bp2, bad.len());
            assert_eq!(
                idx_search_facet_all(h, qp, q.len(), 10, bp2, bad.len()),
                u32::MAX,
                "a malformed spec is an error, not a silent match-everything"
            );
            idx_free(bp2, bad.len());

            idx_free(qp, q.len());
            idx_close(h);
        }
    }

    /// Build one segment through the builder ABI, exactly as a host would.
    ///
    /// Field 1 is the facet slot. Both the schema spec and each row are NUL-separated, which is
    /// the builder ABI's framing.
    unsafe fn segment(row: &[&str]) -> *mut Handle {
        let spec = b"name:3:0.4\0brand:1:0.6";
        let sp = idx_alloc(spec.len());
        std::ptr::copy_nonoverlapping(spec.as_ptr(), sp, spec.len());
        let b = idx_build_new(sp, spec.len());
        idx_free(sp, spec.len());
        assert_eq!(idx_build_facet(b, 1), 1);
        for text in row {
            let raw = text.as_bytes();
            let p = idx_alloc(raw.len());
            std::ptr::copy_nonoverlapping(raw.as_ptr(), p, raw.len());
            assert_ne!(idx_build_add(b, p, raw.len()), u32::MAX);
            idx_free(p, raw.len());
        }
        idx_build_finish(b)
    }

    /// Copy `text` into module memory, run `f` with the pointer, and free it.
    unsafe fn with_bytes<T>(text: &[u8], f: impl FnOnce(*const u8, usize) -> T) -> T {
        let p = idx_alloc(text.len());
        std::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
        let out = f(p, text.len());
        idx_free(p, text.len());
        out
    }

    /// Read the searcher's result buffer back as document ordinals, as a host would.
    unsafe fn searcher_doc(s: *const SearcherHandle, n: u32) -> Vec<u32> {
        assert_ne!(n, u32::MAX, "the call must not have errored");
        let raw = std::slice::from_raw_parts(idx_searcher_result_ptr(s), idx_searcher_result_len(s));
        assert_eq!(raw.len(), n as usize * HIT_BYTE);
        raw.chunks_exact(HIT_BYTE)
            .map(|c| u32::from_le_bytes(c[..4].try_into().unwrap()))
            .collect()
    }

    /// The filter bar across SEGMENTS, through the ABI — including both ends of the unknown-value
    /// rule, which is the pair that silently returns the whole corpus when reversed.
    #[test]
    fn the_searcher_abi_filters_and_pages_across_segments() {
        unsafe {
            // Segment 0 knows only Colgate; segment 1 knows only Oral B. Neither has seen the
            // other's brand, which is the situation the per-segment rule exists for.
            let a = segment(&[
                "Colgate Total Toothpaste 150g\0Colgate",
                "Colgate Fresh Gel Toothpaste 100g\0Colgate",
            ]);
            let b = segment(&[
                "Oral B Toothpaste Pro 120g\0Oral B",
                "Oral B Whitening Toothpaste 90g\0Oral B",
            ]);
            let s = idx_searcher_new(a);
            assert!(!s.is_null());
            assert_eq!(idx_searcher_push(s, b), 1, "the append succeeded");
            assert_eq!(idx_searcher_segment_count(s), 2);
            assert_eq!(idx_searcher_doc_count(s), 4);

            let q = b"Toothpaste";
            with_bytes(q, |qp, qn| {
                // OR within a clause, spanning two segments that intern the values differently.
                let spec = b"0=Colgate|Oral B";
                let n = with_bytes(spec, |sp, sn| {
                    idx_searcher_search_clause(s, qp, qn, 10, 0, sp, sn)
                });
                assert_eq!(searcher_doc(s, n).len(), 4, "the OR must reach both segments");

                // Exclude removes only its own segment's rows, never the other's.
                let spec = b"0!=Colgate";
                let n = with_bytes(spec, |sp, sn| {
                    idx_searcher_search_clause(s, qp, qn, 10, 0, sp, sn)
                });
                assert_eq!(searcher_doc(s, n), vec![2, 3], "not Colgate leaves the Oral B segment");

                // Include, unknown to EVERY segment: nobody can satisfy it.
                let spec = b"0=Nestle";
                let n = with_bytes(spec, |sp, sn| {
                    idx_searcher_search_clause(s, qp, qn, 10, 0, sp, sn)
                });
                assert_eq!(n, 0, "an all-unknown include matches nothing");

                // Exclude, unknown to every segment: there is nothing to remove.
                let spec = b"0!=Nestle";
                let n = with_bytes(spec, |sp, sn| {
                    idx_searcher_search_clause(s, qp, qn, 10, 0, sp, sn)
                });
                assert_eq!(searcher_doc(s, n).len(), 4, "an all-unknown exclude removes nothing");

                // Malformed spec is an error, not a match-everything.
                let spec = b"notanumber=Colgate";
                let n = with_bytes(spec, |sp, sn| {
                    idx_searcher_search_clause(s, qp, qn, 10, 0, sp, sn)
                });
                assert_eq!(n, u32::MAX);

                // Paging partitions the merged ranking: two pages of two equal one call for four,
                // with no document on both pages. The merge is what makes this non-trivial -- a
                // segment's 2nd-best can be the page's 1st.
                let all = searcher_doc(s, idx_searcher_search_page(s, qp, qn, 0, 4));
                let mut paged = searcher_doc(s, idx_searcher_search_page(s, qp, qn, 0, 2));
                paged.extend(searcher_doc(s, idx_searcher_search_page(s, qp, qn, 2, 2)));
                assert_eq!(paged, all, "pages partition the merged ranking");
                assert_eq!(idx_searcher_search_page(s, qp, qn, 99, 2), 0, "past the end is empty");
            });

            idx_searcher_close(s);
        }
    }

    /// Phrase queries through the ABI, including the refusal an index without positions must give.
    #[test]
    fn the_abi_answers_a_phrase_only_with_positions() {
        unsafe {
            let spec = b"name:3:0.4\0brand:1:0.6";
            let row: [&[u8]; 4] = [
                b"Vanilla Ice Cream Tub\0Selecta",
                b"Ice Crushed Cream Soda\0Selecta",
                b"Cream Ice Bar\0Selecta",
                b"Chocolate Ice\0Cream Co", // adjacent only ACROSS the field boundary
            ];
            // Build twice from identical rows: once with positions, once without.
            let build = |positions: bool| -> *mut Handle {
                let sp = idx_alloc(spec.len());
                std::ptr::copy_nonoverlapping(spec.as_ptr(), sp, spec.len());
                let b = idx_build_new(sp, spec.len());
                idx_free(sp, spec.len());
                if positions {
                    assert_eq!(idx_build_position(b), 1, "positions must be accepted before any row");
                }
                for raw in row {
                    let p = idx_alloc(raw.len());
                    std::ptr::copy_nonoverlapping(raw.as_ptr(), p, raw.len());
                    assert_ne!(idx_build_add(b, p, raw.len()), u32::MAX);
                    idx_free(p, raw.len());
                }
                idx_build_finish(b)
            };

            let with = build(true);
            let without = build(false);
            assert!(!with.is_null() && !without.is_null());

            let q = b"Ice Cream";
            let qp = idx_alloc(q.len());
            std::ptr::copy_nonoverlapping(q.as_ptr(), qp, q.len());

            // All four contain both words, so the ordinary search cannot tell them apart.
            assert_eq!(idx_search(with, qp, q.len(), 10, 0), 4);

            let n = idx_search_phrase(with, qp, q.len(), 0, 10);
            assert_eq!(n, 1, "only the adjacent, in-order, same-field document matches");
            let raw = std::slice::from_raw_parts(idx_result_ptr(with), idx_result_len(with));
            assert_eq!(u32::from_le_bytes(raw[..4].try_into().unwrap()), 0);

            // No positions: 0 hits, NOT a silent fallback to the 4-hit term search.
            assert_eq!(
                idx_search_phrase(without, qp, q.len(), 0, 10),
                0,
                "an index without positions refuses the phrase rather than answering a different query"
            );
            assert_eq!(idx_search(without, qp, q.len(), 10, 0), 4, "its term search is unaffected");
            idx_free(qp, q.len());

            // A word no document has: nothing, never everything.
            let miss = b"Ice Sorbet";
            let mp = idx_alloc(miss.len());
            std::ptr::copy_nonoverlapping(miss.as_ptr(), mp, miss.len());
            assert_eq!(idx_search_phrase(with, mp, miss.len(), 0, 10), 0);
            idx_free(mp, miss.len());

            // Once a row is added, positions can no longer be turned on -- refused, not trapped.
            let sp = idx_alloc(spec.len());
            std::ptr::copy_nonoverlapping(spec.as_ptr(), sp, spec.len());
            let late = idx_build_new(sp, spec.len());
            idx_free(sp, spec.len());
            let one = &b"Vanilla Ice Cream Tub\0Selecta"[..];
            let p = idx_alloc(one.len());
            std::ptr::copy_nonoverlapping(one.as_ptr(), p, one.len());
            idx_build_add(late, p, one.len());
            idx_free(p, one.len());
            assert_eq!(idx_build_position(late), 0, "too late is refused, not accepted or trapped");
            assert_eq!(idx_build_position(std::ptr::null_mut()), 0, "a null builder does not trap");
            idx_build_free(late);

            idx_close(with);
            idx_close(without);
        }
    }

    /// **The whole point of keys, through the ABI: express an update without knowing an ordinal.**
    ///
    /// A host receiving "row sku-1 changed" has the key and nothing else. Before `p62` it could
    /// search but never say which row it meant, because `idx_searcher_delete` takes a dense ordinal
    /// assigned at insertion that no database row carries.
    #[test]
    fn the_abi_updates_and_deletes_by_application_key() {
        unsafe {
            let spec = b"sku:0:0.6\0name:3:0.4";
            let seg = |row: &[&str]| -> *mut Handle {
                let sp = idx_alloc(spec.len());
                std::ptr::copy_nonoverlapping(spec.as_ptr(), sp, spec.len());
                let b = idx_build_new(sp, spec.len());
                idx_free(sp, spec.len());
                assert_eq!(idx_build_key(b, 0), 1, "the key field is accepted before any row");
                for text in row {
                    let raw = text.as_bytes();
                    let p = idx_alloc(raw.len());
                    std::ptr::copy_nonoverlapping(raw.as_ptr(), p, raw.len());
                    assert_ne!(idx_build_add(b, p, raw.len()), u32::MAX);
                    idx_free(p, raw.len());
                }
                idx_build_finish(b)
            };
            let with_key = |k: &str, f: &dyn Fn(*const u8, usize) -> u32| -> u32 {
                let raw = k.as_bytes();
                let p = idx_alloc(raw.len());
                std::ptr::copy_nonoverlapping(raw.as_ptr(), p, raw.len());
                let out = f(p, raw.len());
                idx_free(p, raw.len());
                out
            };

            let base = seg(&["sku-1\0Colgate Total Toothpaste 150g", "sku-2\0Aquafresh Mini 50g"]);
            assert_eq!(idx_keyed_count(base), 2);
            assert_eq!(with_key("sku-1", &|p, n| idx_doc_of_key(base, p, n)), 0);
            assert_eq!(
                with_key("sku-404", &|p, n| idx_doc_of_key(base, p, n)),
                u32::MAX,
                "an unknown key is a sentinel, not a trap"
            );
            // Read a key back out of the result buffer, as a host would.
            let n = idx_key_of(base, 1);
            assert_eq!(std::str::from_utf8(std::slice::from_raw_parts(idx_result_ptr(base), n)), Ok("sku-2"));

            let s = idx_searcher_new(base); // CONSUMES base
            assert_eq!(idx_searcher_has_key(s), 1);

            // sku-1 is UPDATED: append the new version, and `push` retires the old one.
            let delta = seg(&["sku-1\0Colgate Total Charcoal 200g", "sku-3\0Oral B Pro 120g"]);
            assert_eq!(idx_searcher_push(s, delta), 1);
            assert_eq!(idx_searcher_doc_count(s), 4);
            assert_eq!(idx_searcher_live_count(s), 3, "the superseded row was retired");
            assert_eq!(with_key("sku-1", &|p, n| idx_searcher_doc_of_key(s, p, n)), 2,
                "the LIVE sku-1 is the new one");

            let n = idx_searcher_key_of(s, 2);
            let raw = std::slice::from_raw_parts(idx_searcher_result_ptr(s), n);
            assert_eq!(std::str::from_utf8(raw), Ok("sku-1"));

            // A delete arriving from a change stream, expressed the only way a host can.
            assert_eq!(with_key("sku-2", &|p, n| idx_searcher_delete_key(s, p, n)), 1);
            assert_eq!(
                with_key("sku-2", &|p, n| idx_searcher_delete_key(s, p, n)),
                0,
                "replaying a delete is a no-op, not an error -- a stream re-delivers them"
            );
            assert_eq!(
                with_key("sku-404", &|p, n| idx_searcher_delete_key(s, p, n)),
                0,
                "deleting a key that never existed reports 0 rather than trapping"
            );
            assert_eq!(idx_searcher_live_count(s), 2);
            assert_eq!(with_key("sku-2", &|p, n| idx_searcher_doc_of_key(s, p, n)), u32::MAX);

            // Null and non-UTF-8 must return sentinels, never trap.
            assert_eq!(idx_doc_of_key(std::ptr::null(), b"x".as_ptr(), 1), u32::MAX);
            assert_eq!(idx_searcher_delete_key(std::ptr::null_mut(), b"x".as_ptr(), 1), 0);
            assert_eq!(idx_searcher_doc_of_key(s, std::ptr::null(), 0), u32::MAX);
            assert_eq!(idx_keyed_count(std::ptr::null()), 0);
            assert_eq!(idx_build_key(std::ptr::null_mut(), 0), 0);

            idx_searcher_close(s);
        }
    }

    /// A truncated index — the case that actually happens when a fetch is interrupted.
    #[test]
    fn a_truncated_index_does_not_open() {
        let bytes = blob();
        unsafe {
            for frac in [1usize, 2, 3, 4, 6, 7] {
                let cut = bytes.len() * frac / 8;
                if cut == 0 {
                    continue;
                }
                let p = idx_alloc(cut);
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, cut);
                let h = idx_open(p, cut);
                // Either it refuses, or it opens something self-consistent. It must never trap.
                if !h.is_null() {
                    idx_close(h);
                }
                idx_free(p, cut);
            }
        }
    }

    // ---- The range tier ------------------------------------------------------------------------

    /// A host that owns a FILE and can only read slices of it — the shape a sync access handle
    /// gives an OPFS worker. It COUNTS what it hands over, because "materially less than the whole
    /// file" is the claim being made and an uncounted claim is a vibe.
    struct RangeHost {
        file: Vec<u8>,
        read_byte: usize,
    }

    impl RangeHost {
        fn read(&mut self, at: u64, len: u64) -> Vec<u8> {
            self.read_byte += len as usize;
            self.file[at as usize..(at + len) as usize].to_vec()
        }
    }

    /// Decode the `(offset, len)` records the module last wrote.
    unsafe fn plan_of(rh: *mut RangeHandle, n: u32) -> Vec<(u64, u64)> {
        assert_ne!(n, u32::MAX, "a plan must not fail");
        let bytes = std::slice::from_raw_parts(idx_range_result_ptr(rh), idx_range_result_len(rh));
        assert_eq!(bytes.len(), n as usize * SPAN_BYTE);
        bytes
            .chunks_exact(SPAN_BYTE)
            .map(|c| {
                (
                    u64::from_le_bytes(c[..8].try_into().unwrap()),
                    u64::from_le_bytes(c[8..].try_into().unwrap()),
                )
            })
            .collect()
    }

    /// Open a range handle the way a host must: 296 bytes, then the resident plan, then nothing.
    unsafe fn range_open(host: &mut RangeHost) -> *mut RangeHandle {
        let head = host.read(0, idx_range_head_byte() as u64);
        let rh = idx_range_open(head.as_ptr(), head.len());
        assert!(!rh.is_null(), "296 bytes must be enough to open");
        let n = idx_range_plan(rh);
        let mut buf = Vec::new();
        for (at, len) in plan_of(rh, n) {
            buf.extend_from_slice(&host.read(at, len));
        }
        assert_eq!(idx_range_load(rh, buf.as_ptr(), buf.len()), 1, "resident sections must load");
        rh
    }

    /// Plan a query, fetch exactly its spans, search. Returns the raw hit records.
    unsafe fn range_search(host: &mut RangeHost, rh: *mut RangeHandle, q: &str, k: u32) -> Vec<u8> {
        let n = idx_range_plan_query(rh, q.as_ptr(), q.len(), 0);
        let mut buf = Vec::new();
        for (at, len) in plan_of(rh, n) {
            buf.extend_from_slice(&host.read(at, len));
        }
        let hit = idx_range_search(rh, q.as_ptr(), q.len(), k, 0, buf.as_ptr(), buf.len());
        assert_ne!(hit, u32::MAX, "range search of {q:?} must not fail");
        std::slice::from_raw_parts(idx_range_result_ptr(rh), idx_range_result_len(rh)).to_vec()
    }

    /// A corpus big enough that the posting section is the bulk of the file, which is the whole
    /// premise: the sections a range open skips have to be worth skipping.
    fn corpus_blob() -> Vec<u8> {
        let brand = ["Colgate", "Nescafe", "Bear Brand", "Lucky Me", "Oral B", "Milo", "Argentina"];
        let kind = ["Toothpaste", "Coffee", "Powdered Milk", "Instant Noodle", "Corned Beef"];
        let note = ["Charcoal", "Classic", "Reseal", "Fortified", "Original", "Advanced", "Pro"];
        let mut b = IndexBuilder::new(Schema::new(vec![
            Field::new("name", 3.0, 0.4),
            Field::new("detail", 1.0, 0.6),
        ]));
        for i in 0..600usize {
            let name = format!(
                "{} {} {} {}g",
                brand[i % brand.len()],
                kind[i % kind.len()],
                note[i % note.len()],
                50 + (i % 40) * 5
            );
            let detail = format!("sku {i} {} variant {}", note[(i + 3) % note.len()], i % 17);
            b.add(&Doc::new([name, detail]));
        }
        b.build().unwrap().to_bytes()
    }

    /// The claim `p68` left open: answer a real query having read materially less than the file.
    #[test]
    fn a_range_query_answers_from_a_fraction_of_the_file() {
        let file = corpus_blob();
        let mut host = RangeHost { file: file.clone(), read_byte: 0 };
        unsafe {
            let rh = range_open(&mut host);
            let after_open = host.read_byte;
            assert_eq!(idx_range_doc_count(rh), 600);
            assert!(idx_range_term_count(rh) > 0);

            let hit = range_search(&mut host, rh, "colgate charcoal", 10);
            assert!(!hit.is_empty(), "the range path must actually answer");

            assert!(
                host.read_byte < file.len(),
                "read {} of {} bytes — the range path read the whole file",
                host.read_byte,
                file.len()
            );
            // The posting section is the bulk, so skipping it must be worth a clear majority.
            assert!(
                host.read_byte * 2 < file.len(),
                "read {} of {} bytes, which is not materially less",
                host.read_byte,
                file.len()
            );
            assert!(after_open < host.read_byte, "the query must read something of its own");
            idx_range_close(rh);
        }
    }

    /// Same hits, same order, same scores as a full `idx_open` — byte for byte. A cheaper answer
    /// that is a DIFFERENT answer is not a cheaper answer.
    #[test]
    fn a_range_query_matches_a_full_open_byte_for_byte() {
        let file = corpus_blob();
        unsafe {
            let p = idx_alloc(file.len());
            std::ptr::copy_nonoverlapping(file.as_ptr(), p, file.len());
            let full = idx_open(p, file.len());
            idx_free(p, file.len());
            assert!(!full.is_null());

            let mut host = RangeHost { file: file.clone(), read_byte: 0 };
            let rh = range_open(&mut host);

            for q in [
                "colgate",
                "colgaye",          // a typo, so the plan must cover the fuzzy expansion
                "nescafe classic",  // two tokens
                "bearbrand",        // a compound split
                "powdered milk 200g",
                "zzzznothing",      // nothing in the dictionary at all
            ] {
                let want = {
                    let n = idx_search(full, q.as_ptr(), q.len(), 10, 0);
                    assert_ne!(n, u32::MAX);
                    std::slice::from_raw_parts(idx_result_ptr(full), idx_result_len(full)).to_vec()
                };
                let got = range_search(&mut host, rh, q, 10);
                assert_eq!(got, want, "range and full answers disagree for {q:?}");
            }
            idx_range_close(rh);
            idx_close(full);
        }
    }

    /// The 296 bytes are the property the whole tier rests on, and `format.rs` owns the real
    /// number — so it is asserted against `read_section_table`, not against itself.
    #[test]
    fn the_layout_of_any_index_costs_296_bytes() {
        assert_eq!(idx_range_head_byte(), 296);
        assert_eq!(idx_range_span_byte(), 16);
        let file = corpus_blob();
        let head = idx_range_head_byte() as usize;
        assert!(index_text::read_section_table(&file[..head]).is_ok());
        assert!(
            index_text::read_section_table(&file[..head - 1]).is_err(),
            "296 must be the MINIMUM, or this constant has drifted from format.rs"
        );
        unsafe {
            let rh = idx_range_open(file.as_ptr(), head - 1);
            assert!(rh.is_null(), "a short head must be refused, not guessed at");
        }
    }

    /// Every range entry point, given every bad input a host can produce. Nothing may trap.
    #[test]
    fn range_bad_input_returns_sentinels_rather_than_trapping() {
        let file = corpus_blob();
        unsafe {
            assert!(idx_range_open(std::ptr::null(), 296).is_null());
            assert!(idx_range_open(file.as_ptr(), 8).is_null());
            assert!(idx_range_open(b"garbage!garbage!".as_ptr(), 16).is_null());
            assert_eq!(idx_range_plan(std::ptr::null_mut()), u32::MAX);
            assert_eq!(idx_range_load(std::ptr::null_mut(), file.as_ptr(), 4), 0);
            assert_eq!(idx_range_plan_query(std::ptr::null_mut(), b"x".as_ptr(), 1, 0), u32::MAX);
            assert_eq!(
                idx_range_search(std::ptr::null_mut(), b"x".as_ptr(), 1, 5, 0, file.as_ptr(), 1),
                u32::MAX
            );
            assert_eq!(idx_range_doc_count(std::ptr::null()), 0);
            assert_eq!(idx_range_term_count(std::ptr::null()), 0);
            assert_eq!(idx_range_result_len(std::ptr::null()), 0);
            assert!(idx_range_result_ptr(std::ptr::null()).is_null());
            idx_range_close(std::ptr::null_mut());

            // A live handle, then every way a host can get the second phase wrong.
            let rh = idx_range_open(file.as_ptr(), file.len());
            assert!(!rh.is_null());
            assert_eq!(idx_range_doc_count(rh), 0, "nothing is loaded yet");
            assert_eq!(idx_range_plan_query(rh, b"x".as_ptr(), 1, 0), u32::MAX, "not loaded");
            assert_eq!(idx_range_load(rh, file.as_ptr(), 3), 0, "a short resident buffer");
            assert_eq!(idx_range_load(rh, std::ptr::null(), 0), 0);

            let mut host = RangeHost { file: file.clone(), read_byte: 0 };
            let rh2 = range_open(&mut host);
            let q = b"colgate";
            assert_ne!(idx_range_plan_query(rh2, q.as_ptr(), q.len(), 0), u32::MAX);
            // The plan said N bytes; hand over the wrong number and get a sentinel, not a guess.
            assert_eq!(
                idx_range_search(rh2, q.as_ptr(), q.len(), 10, 0, file.as_ptr(), 7),
                u32::MAX
            );
            let bad = [0xffu8, 0xfe];
            assert_eq!(idx_range_plan_query(rh2, bad.as_ptr(), 2, 0), u32::MAX);
            idx_range_close(rh);
            idx_range_close(rh2);
        }
    }
}
