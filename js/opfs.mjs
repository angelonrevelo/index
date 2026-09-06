// The OPFS tier: an index that lives in the browser's own filesystem, and is queried without a
// network call after the first visit.
//
// # Why this is the tier that matters
//
// `docs/research/landscape.md` §7.5 found the gap this fills, and found that it is architectural
// rather than neglected:
//
// > There is **no engine** where the same index file serves an OPFS-backed browser client, an
// > embedded server process and an object-store cold tier. Pagefind proves chunked static search
// > works, but its index is build-time-frozen and bespoke. **LanceDB closed WASM as "not planned";
// > Tantivy's WASM RFC has been open since 2019 and is read-only; Orama's Rust core has no wasm32
// > target.**
//
// Tantivy is mmap-based, and `p7` recorded what that does here: *an mmap format does not fail to
// port to WASM — it silently succeeds and reads the whole index into linear memory.* Meilisearch and
// Elasticsearch are servers; there is no "run it in the tab" mode to add. So the position is open
// not because nobody wants it but because their storage layer forbids it.
//
// # Two things OPFS gives that a `fetch` cannot
//
// **Persistence.** The bytes survive a reload, so the second visit makes NO network request. For
// profstopick that replaces a 2,505,813-byte JSON shard occupying 95.6 % of the 5 MB localStorage
// quota with a 386 KB binary in a quota measured in gigabytes.
//
// **Range reads.** `createSyncAccessHandle().read(buf, { at })` reads a slice of a file without
// loading it. That is the whole reason `format.rs` puts a fixed section table at a known offset:
// `read_section_table` needs `MAGIC.len() + 288` bytes to learn where everything lives. A browser
// can therefore open an index larger than the tab's memory — which is what an object-store cold
// tier looks like from the client side.
//
// Sync access handles are worker-only in every engine that ships them, so the range path lives in
// `opfs-worker.mjs`. The main thread gets `getFile()`, which is enough to hand the whole artifact
// to `idx_open`.

const DIR = 'index-tier';

/** Whether this context can persist an index at all. */
export function opfsAvailable() {
  return typeof navigator !== 'undefined' && !!navigator.storage?.getDirectory;
}

async function dir() {
  const root = await navigator.storage.getDirectory();
  return root.getDirectoryHandle(DIR, { create: true });
}

/**
 * Store `bytes` under `name`, replacing any previous copy.
 *
 * Written through a temporary name and then re-written in place rather than truncated-in-place, so
 * a tab closed mid-write cannot leave a half-index that `idx_open` will refuse — the same reason
 * `collection.rs` writes a temp file and renames.
 */
export async function put(name, bytes) {
  const d = await dir();
  const tmp = `${name}.partial`;
  const h = await d.getFileHandle(tmp, { create: true });
  const w = await h.createWritable();
  await w.write(bytes);
  await w.close();
  // OPFS has no rename, so the commit is a copy-then-remove. The partial name is what makes an
  // interrupted write detectable rather than silently short.
  const final = await d.getFileHandle(name, { create: true });
  const fw = await final.createWritable();
  await fw.write(await (await h.getFile()).arrayBuffer());
  await fw.close();
  await d.removeEntry(tmp);
  return (await (await final.getFile())).size;
}

/** The stored bytes for `name`, or `null` if this browser has never seen it. */
export async function get(name) {
  try {
    const d = await dir();
    const f = await (await d.getFileHandle(name)).getFile();
    return new Uint8Array(await f.arrayBuffer());
  } catch {
    return null;
  }
}

/** Size on disk, or 0 when absent. Cheap: no read. */
export async function size(name) {
  try {
    const d = await dir();
    return (await (await d.getFileHandle(name)).getFile()).size;
  } catch {
    return 0;
  }
}

/** Forget a stored index — used by the demo's "cold start" button and by the checks. */
export async function drop(name) {
  try {
    // `await` is load-bearing: without it the rejection escapes the try/catch as an unhandled
    // promise rejection and shows up as a console error on every first visit.
    await (await dir()).removeEntry(name);
  } catch {
    /* already gone */
  }
}

/**
 * Load `name` from OPFS, fetching `url` and persisting it only if this browser has not seen it.
 *
 * Returns `{ bytes, source, ms }` where `source` is `'opfs'` or `'network'` — the demo reports it,
 * because "no network call" is the claim and an unlabelled fast path is not evidence of it.
 */
export async function load(name, url) {
  const t0 = performance.now();
  const cached = await get(name);
  if (cached) {
    return { bytes: cached, source: 'opfs', ms: performance.now() - t0 };
  }
  const res = await fetch(url);
  if (!res.ok) {
    throw new Error(`${url}: ${res.status}`);
  }
  const bytes = new Uint8Array(await res.arrayBuffer());
  await put(name, bytes);
  return { bytes, source: 'network', ms: performance.now() - t0 };
}

// ---- The range tier: querying a file this tab could not hold ------------------------------------
//
// Everything above still hands `idx_open` the WHOLE artifact. That is fine at 386 KB and impossible
// at the ~910 MB `bench/roadmap/p56-ten-million.md` measured for ten million documents, and it is
// the gap `p68` closed with rather than closed:
//
// > The worker proves a section can be read in isolation; the module still takes the whole buffer.
//
// `RangeIndex` is the other way in. It never materialises the file: 296 bytes for the layout, one
// read for the sections that are not postings, and then one read per query for exactly the posting
// lists that query needs. Every byte goes through the `read` callback the caller supplies, so the
// caller can COUNT them — which is the claim, and a claim nobody counted is a decoration.
//
// The module decides what to read; this file only fetches and hands back. See the range tier's
// header comment in `crates/index-wasm/src/lib.rs` for why the plan runs in that direction (short
// version: a sync access handle is worker-only and cannot be awaited from inside a WASM call).

/** Must track `ABI_VERSION` in `crates/index-wasm`, for the reason `js/index.mjs` documents. */
const RANGE_ABI_VERSION = 14;
/** `u32` doc, `f32` score, `u32` typo bucket. Same record `js/index.mjs` decodes. */
const HIT_BYTE = 12;
/** WASM has no unsigned return type; `u32::MAX` arrives as -1 unless coerced. */
const u32 = (n) => n >>> 0;
const ERR = 0xffffffff;

/**
 * An index queried through byte ranges, never held whole.
 *
 * `read(span)` takes `[[offset, len], ...]` and resolves to those bytes CONCATENATED in that order
 * — which is exactly `opfs-worker.mjs`'s `span` op, and equally an HTTP `Range` request or a
 * `fetch` against an object store. The reader is injected rather than assumed so the same class
 * covers all three; the tier this belongs to is "cold storage", not "OPFS".
 */
export class RangeIndex {
  #e;
  #rh;
  #read;
  #label;

  constructor(exports, handle, read, label) {
    this.#e = exports;
    this.#rh = handle;
    this.#read = read;
    this.#label = label ?? null;
  }

  /** Copy `bytes` into linear memory. Returns `[ptr, len]`; `[0, 0]` for nothing. */
  #write(bytes) {
    if (bytes.length === 0) return [0, 0];
    const p = u32(this.#e.idx_alloc(bytes.length));
    if (p === 0) throw new Error(`idx_alloc(${bytes.length}) failed`);
    // Re-derived AFTER the allocation: `memory.grow()` detaches every earlier view.
    new Uint8Array(this.#e.memory.buffer, p, bytes.length).set(bytes);
    return [p, bytes.length];
  }

  /** Decode the `(offset, len)` records the module just wrote into its result buffer. */
  #plan(count) {
    const e = this.#e;
    const stride = u32(e.idx_range_span_byte());
    const ptr = u32(e.idx_range_result_ptr(this.#rh));
    const len = u32(e.idx_range_result_len(this.#rh));
    if (len !== count * stride) throw new Error('plan buffer disagrees with the plan count');
    const dv = new DataView(e.memory.buffer, ptr, len);
    const span = [];
    for (let i = 0; i < count; i++) {
      // Number is exact to 2^53 — past any offset a u64 file this format can address will reach.
      span.push([Number(dv.getBigUint64(i * stride, true)), Number(dv.getBigUint64(i * stride + 8, true))]);
    }
    return span;
  }

  /**
   * Open an index over `read`, having read only its head and its non-posting sections.
   *
   * Throws rather than returning a half-built object: an index that cannot plan is not an index,
   * and a host that got one would discover it one query later with a much worse error.
   */
  static async open(wasmBytes, read, { label = null } = {}) {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    const e = instance.exports;
    const got = e.idx_abi_version();
    if (got !== RANGE_ABI_VERSION) {
      throw new Error(`ABI mismatch: module is v${got}, this host speaks v${RANGE_ABI_VERSION}`);
    }
    if (typeof e.idx_range_open !== 'function') {
      throw new Error('this module has no range tier — rebuild index-wasm');
    }

    // 1. The layout. 296 bytes, and the same 296 whatever the file weighs.
    const head = await read([[0, u32(e.idx_range_head_byte())]]);
    const ix = new RangeIndex(e, 0, read, label);
    const [hp, hl] = ix.#write(head);
    const rh = u32(e.idx_range_open(hp, hl));
    e.idx_free(hp, hl);
    if (rh === 0) throw new Error('idx_range_open rejected the head — not an index, or an old one');
    ix.#rh = rh;

    // 2. Everything that is not a posting list. One plan, one read.
    const n = u32(e.idx_range_plan(rh));
    if (n === ERR) throw new Error('idx_range_plan failed');
    const resident = await read(ix.#plan(n));
    const [rp, rl] = ix.#write(resident);
    const ok = u32(e.idx_range_load(rh, rp, rl));
    e.idx_free(rp, rl);
    if (ok !== 1) throw new Error('idx_range_load refused the resident sections');
    return ix;
  }

  get docCount() {
    return u32(this.#e.idx_range_doc_count(this.#rh));
  }

  get termCount() {
    return u32(this.#e.idx_range_term_count(this.#rh));
  }

  /**
   * Run a query, reading only the posting lists it needs.
   *
   * Two calls into the module with one `await` between them: the first says which spans, the
   * second answers from them. That await is the whole reason this is not a callback — see the
   * range tier's header in `crates/index-wasm/src/lib.rs`.
   */
  async search(query, { k = 10, prefix = false } = {}) {
    const e = this.#e;
    const q = new TextEncoder().encode(query);
    if (q.length === 0) return [];
    const [qp, ql] = this.#write(q);
    try {
      const n = u32(e.idx_range_plan_query(this.#rh, qp, ql, prefix ? 1 : 0));
      if (n === ERR) throw new Error('idx_range_plan_query failed');
      const posting = await this.#read(this.#plan(n));

      const [pp, pl] = this.#write(posting);
      const hit = u32(e.idx_range_search(this.#rh, qp, ql, k, prefix ? 1 : 0, pp, pl));
      if (hit === ERR) {
        e.idx_free(pp, pl);
        // Not necessarily a bug: the module refuses rather than answering from a term set it
        // cannot prove complete. The caller's fallback is a full `idx_open`.
        throw new Error('idx_range_search refused this query — fall back to a whole-file open');
      }
      // Read the records BEFORE freeing anything, and re-derive the view: any allocation since the
      // last one may have moved the buffer out from under an older Uint8Array.
      const ptr = u32(e.idx_range_result_ptr(this.#rh));
      const view = new DataView(e.memory.buffer, ptr, hit * HIT_BYTE);
      const out = [];
      for (let i = 0; i < hit; i++) {
        const doc = view.getUint32(i * HIT_BYTE, true);
        out.push({
          doc,
          score: view.getFloat32(i * HIT_BYTE + 4, true),
          typoBucket: view.getUint32(i * HIT_BYTE + 8, true),
          label: this.#label?.[doc] ?? null,
        });
      }
      e.idx_free(pp, pl);
      return out;
    } finally {
      e.idx_free(qp, ql);
    }
  }

  close() {
    this.#e.idx_range_close(this.#rh);
    this.#rh = 0;
  }
}

/**
 * A `read` for [`RangeIndex`] backed by an OPFS sync access handle in `worker`.
 *
 * Wraps the worker's `span` op and TALLIES every byte, because the point of the tier is the number
 * it produces: `state.byteRead` against `state.size` is the claim "this tab answered a query
 * without holding the file", stated as a fraction rather than as an adjective.
 */
export function opfsRangeReader(worker, file) {
  const state = { byteRead: 0, readCount: 0, size: 0 };
  const read = (span) =>
    new Promise((resolve, reject) => {
      const id = `range-${state.readCount}-${Math.random()}`;
      worker.addEventListener('message', function once(e) {
        if (e.data.id !== id) return;
        worker.removeEventListener('message', once);
        if (!e.data.ok) return reject(new Error(e.data.error));
        state.byteRead += e.data.bytes.length;
        state.readCount++;
        state.size = e.data.size;
        resolve(e.data.bytes);
      });
      worker.postMessage({ id, op: 'span', file, span });
    });
  return { read, state };
}
