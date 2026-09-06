// index-text — JavaScript host for the WASM engine.
//
// No wasm-bindgen. This talks to the hand-written C ABI in `crates/index-wasm`, which is the same
// ABI a native `.so`/`.dll` exposes — so Node, the browser, an edge worker and a Go/Python/PHP FFI
// all consume one artifact. It is also the shape onegrid already ratified on this machine
// (`ACCEL_ABI_VERSION`, JS owns the heap), so this drops into the socket that already exists.
//
// The one rule that will bite you if you forget it: **`memory.grow()` detaches every view of the
// buffer, even `grow(0)`**. Every helper below re-derives its typed array from `memory.buffer` on
// each use rather than caching one. Caching is the bug you cannot reproduce.

// Must track `ABI_VERSION` in `crates/index-wasm`. This host sat at 2 while the module reached 9,
// which made every `build()` and `open()` throw `ABI mismatch` -- the check worked, the constant
// did not. `js/smoke.mjs` asserts the module's version but instantiates the module directly, so it
// never touched this file. Nothing here is gate-covered until it is, which is `p44`'s open note.
const ABI_VERSION = 13;
const HIT_BYTE = 12; // u32 doc, f32 score, u32 typo_bucket

// WASM has no unsigned 32-bit return type: every `u32` arrives in JS as a **signed** i32, so
// `u32::MAX` shows up as -1 and a pointer above 2 GiB would show up negative. Coerce at every
// boundary. Missing this makes the error sentinel silently unrecognisable, which is exactly the
// kind of bug that only appears once something has already gone wrong.
const u32 = (n) => n >>> 0;
const ERR = 0xffffffff;

export class SearchIndex {
  #exports;
  #handle;
  #label;

  constructor(exports, handle, label) {
    this.#exports = exports;
    this.#handle = handle;
    this.#label = label ?? null;
  }

  /**
   * Instantiate the module and build an index in-process from documents.
   *
   * `field` is `[{name, boost, b}, ...]`; `doc` is an array of arrays of field strings,
   * positionally aligned with `field`. Use this when the host has the data but no build step —
   * `open()` is for a prebuilt artifact.
   */
  static async build(wasmBytes, field, doc, { label = null, position = false, key = null } = {}) {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    const e = instance.exports;
    if (e.idx_abi_version() !== ABI_VERSION) throw new Error('ABI mismatch');

    const enc = new TextEncoder();
    const write = (str) => {
      const b = enc.encode(str);
      const p = u32(e.idx_alloc(b.length || 1));
      if (p === 0) throw new Error('idx_alloc failed');
      if (b.length) new Uint8Array(e.memory.buffer, p, b.length).set(b);
      return [p, b.length];
    };

    const spec = field.map((f) => `${f.name}:${f.boost}:${f.b}`).join('\0');
    const [sp, sl] = write(spec);
    const builder = u32(e.idx_build_new(sp, sl));
    e.idx_free(sp, sl || 1);
    if (builder === 0) throw new Error('idx_build_new rejected the field spec');

    // Positions must be requested before the first document; they cannot be recovered afterwards.
    if (position && u32(e.idx_build_position(builder)) === 0) {
      e.idx_build_free(builder);
      throw new Error('idx_build_position was refused');
    }
    // Same for the key field, and without one this collection can never be updated by key.
    if (key !== null && u32(e.idx_build_key(builder, key)) === 0) {
      e.idx_build_free(builder);
      throw new Error(`idx_build_key was refused for field ${key}`);
    }

    for (const d of doc) {
      const [dp, dl] = write(d.map((x) => x ?? '').join('\0'));
      const ord = u32(e.idx_build_add(builder, dp, dl));
      e.idx_free(dp, dl || 1);
      if (ord === ERR) {
        e.idx_build_free(builder);
        throw new Error('idx_build_add failed');
      }
    }

    const handle = u32(e.idx_build_finish(builder));
    if (handle === 0) throw new Error('idx_build_finish failed');
    return new SearchIndex(e, handle, label);
  }

  /** Instantiate the module and open a serialized index. */
  static async open(wasmBytes, indexBytes, { label = null } = {}) {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    const e = instance.exports;

    const got = e.idx_abi_version();
    if (got !== ABI_VERSION) {
      throw new Error(`ABI mismatch: module is v${got}, this host speaks v${ABI_VERSION}`);
    }

    // Copy the index into linear memory, open it, then release our copy — the module keeps its own.
    const ptr = u32(e.idx_alloc(indexBytes.length));
    if (ptr === 0) throw new Error(`idx_alloc(${indexBytes.length}) failed`);
    // Re-derive AFTER the allocation, which may have grown memory and detached older views.
    new Uint8Array(e.memory.buffer, ptr, indexBytes.length).set(indexBytes);

    const handle = u32(e.idx_open(ptr, indexBytes.length));
    e.idx_free(ptr, indexBytes.length);
    if (handle === 0) {
      throw new Error('idx_open rejected the bytes — truncated, corrupt, or a different format');
    }
    return new SearchIndex(e, handle, label);
  }

  get docCount() {
    return u32(this.#exports.idx_doc_count(this.#handle));
  }

  get termCount() {
    return u32(this.#exports.idx_term_count(this.#handle));
  }

  /**
   * Run a query.
   * @param {string} query
   * @param {{k?: number, prefix?: boolean}} opt  `prefix` enables typeahead on the last token.
   * @returns {{doc: number, score: number, typoBucket: number, label: string|null}[]}
   */
  search(query, { k = 10, prefix = false } = {}) {
    const e = this.#exports;
    const bytes = new TextEncoder().encode(query);
    if (bytes.length === 0) return [];

    const qp = u32(e.idx_alloc(bytes.length));
    if (qp === 0) throw new Error('idx_alloc failed for the query');
    new Uint8Array(e.memory.buffer, qp, bytes.length).set(bytes);

    const n = u32(e.idx_search(this.#handle, qp, bytes.length, k, prefix ? 1 : 0));
    e.idx_free(qp, bytes.length);
    if (n === ERR) throw new Error('idx_search failed (bad handle or non-UTF-8 query)');
    if (n === 0) return [];
    return this.#hit(n);
  }

  /**
   * Decode `n` hit records from a result buffer. One decoder for every search entry point.
   * @returns {{doc: number, score: number, typoBucket: number, label: string|null}[]}
   */
  #hit(n) {
    const e = this.#exports;
    const rp = u32(e.idx_result_ptr(this.#handle));
    const rlen = u32(e.idx_result_len(this.#handle));
    // Fresh view: the search allocated, so any earlier view may be detached.
    const view = new DataView(e.memory.buffer, rp, rlen);
    const out = [];
    for (let i = 0; i < n; i++) {
      const at = i * HIT_BYTE;
      const doc = view.getUint32(at, true);
      out.push({
        doc,
        score: view.getFloat32(at + 4, true),
        typoBucket: view.getUint32(at + 8, true),
        label: this.#label ? (this.#label[doc] ?? null) : null,
      });
    }
    return out;
  }

  /**
   * Copy `str` into linear memory, run `f`, then free it. Every string crossing the boundary goes
   * through here, so no caller can leak one by taking an early return.
   */
  #withStr(str, f) {
    const e = this.#exports;
    const bytes = new TextEncoder().encode(str);
    const p = u32(e.idx_alloc(bytes.length || 1));
    if (p === 0) throw new Error('idx_alloc failed');
    if (bytes.length) new Uint8Array(e.memory.buffer, p, bytes.length).set(bytes);
    try {
      return f(p, bytes.length);
    } finally {
      e.idx_free(p, bytes.length || 1);
    }
  }

  /**
   * The whole filter bar: **OR within a clause, AND across clauses**, and NOT.
   *
   * Ticking two brands widens the result; ticking a brand and a category narrows it. That is what
   * a filter bar means, and it is why the two operators nest this way round.
   *
   * An unknown value is ignored rather than fatal, because a bar built from one segment's labels
   * may name a value another segment has never seen. The two ends of that rule are deliberately
   * OPPOSITE, and reversing them is how a filter silently returns the whole corpus:
   * an include whose values are all unknown matches **nothing**; an exclude whose values are all
   * unknown excludes **nothing**. A row with no value in the slot is kept by an exclude and
   * dropped by an include — an unbranded row survives "not Colgate".
   *
   * @param {string} query
   * @param {{slot: number, value: string[], exclude?: boolean}[]} clause
   * @param {{k?: number, offset?: number}} opt
   */
  searchClause(query, clause, { k = 10, offset = 0 } = {}) {
    const spec = clause
      .map(({ slot, value, exclude = false }) => {
        // `|` separates alternatives on the wire, so a value containing one cannot be expressed.
        // Refusing beats shipping a spec that means two values where the caller meant one.
        for (const v of value) {
          if (v.includes('|')) throw new Error(`a facet value containing '|' cannot be expressed: ${v}`);
        }
        return `${slot}${exclude ? '!' : ''}=${value.join('|')}`;
      })
      .join(' ');

    const n = this.#withStr(query, (qp, ql) =>
      this.#withStr(spec, (sp, sl) =>
        u32(this.#exports.idx_search_clause(this.#handle, qp, ql, k, offset, sp, sl)),
      ),
    );
    if (n === ERR) throw new Error('idx_search_clause rejected the filter');
    return n === 0 ? [] : this.#hit(n);
  }

  /**
   * Page `n` is `offset = n * k`.
   *
   * **Cost grows with `offset`, not with `k`.** The engine over-fetches `offset + k` and drops the
   * prefix, because rank order is only known once everything above the page has been scored. That
   * is true of every engine without a stored cursor. For deep paging, filter instead of paging.
   */
  searchPage(query, offset = 0, k = 10) {
    const n = this.#withStr(query, (qp, ql) =>
      u32(this.#exports.idx_search_page(this.#handle, qp, ql, offset, k)),
    );
    if (n === ERR) throw new Error('idx_search_page failed');
    return n === 0 ? [] : this.#hit(n);
  }

  /**
   * **Phrase query**: the query's tokens, consecutive and in order, within ONE field.
   *
   * Requires `build(..., { position: true })`. Without positions this returns `[]` and does **not**
   * fall back to an ordinary term search — a fallback would hand back bag-of-words rows that are
   * indistinguishable from phrase rows once they are in the array.
   *
   * A phrase is **exact**: no token is typo-corrected or prefix-expanded, because quoting is the
   * caller asserting these words, and correcting inside a phrase answers a different question. A
   * token absent from the dictionary matches nothing.
   *
   * Ranking is unchanged — the phrase is a filter applied after scoring, like a facet clause, so a
   * phrase hit carries the score the same query would have given it.
   */
  searchPhrase(query, { k = 10, offset = 0 } = {}) {
    const n = this.#withStr(query, (qp, ql) =>
      u32(this.#exports.idx_search_phrase(this.#handle, qp, ql, offset, k)),
    );
    if (n === ERR) throw new Error('idx_search_phrase failed');
    return n === 0 ? [] : this.#hit(n);
  }

  /**
   * The document carrying `key`, or `null`.
   *
   * A **deleted** document is still found: this answers "which ordinal is this row", which a
   * caller needs precisely in order to delete it.
   */
  docOfKey(key) {
    const d = this.#withStr(key, (p, n) => u32(this.#exports.idx_doc_of_key(this.#handle, p, n)));
    return d === ERR ? null : d;
  }

  /** The application key of `doc`, or `null` when the row has none. */
  keyOf(doc) {
    const e = this.#exports;
    const n = u32(e.idx_key_of(this.#handle, doc));
    if (n === 0) return null;
    const p = u32(e.idx_result_ptr(this.#handle));
    return new TextDecoder().decode(new Uint8Array(e.memory.buffer, p, n));
  }

  /**
   * Rows carrying a key. Below `docCount` when some key fields were blank — and those rows can
   * never be addressed by a change stream, which is why it is worth checking rather than assuming.
   */
  get keyedCount() {
    return u32(this.#exports.idx_keyed_count(this.#handle));
  }

  /** Serialize this index to bytes, so a host can build once and cache the artifact. */
  serialize() {
    const e = this.#exports;
    const n = u32(e.idx_serialize(this.#handle));
    if (n === 0) throw new Error('idx_serialize failed');
    const p = u32(e.idx_result_ptr(this.#handle));
    return new Uint8Array(e.memory.buffer, p, n).slice();
  }

  /** Release the index. The instance stays alive; open another if you need one. */
  close() {
    if (this.#handle !== 0) {
      this.#exports.idx_close(this.#handle);
      this.#handle = 0;
    }
  }
}
