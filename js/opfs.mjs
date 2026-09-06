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
