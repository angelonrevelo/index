// A `read` for `RangeIndex` over plain HTTP `Range` requests — the reader `opfs.mjs` promised
// ("equally an HTTP `Range` request") and never shipped.
//
// `RangeIndex` is only as cheap as its reader. The obvious reader — one request per span — is the
// wrong one twice over:
//
//   1. A plan is many small, nearby spans (posting lists of neighbouring terms sit next to each
//      other). Round trips, not bytes, dominate a query over a network, so spans closer than `gap`
//      bytes are COALESCED into one request. Paying a few KB of filler to save an RTT is the trade
//      every range-read engine makes (DuckDB's httpfs, phiresky's sql.js-httpvfs).
//   2. The range tier has no plan cache: the same query, or two queries sharing a term, fetch the
//      same posting list again. A reader that remembers every byte it has held turns the second
//      read of a span into zero network. The module stays stateless; the cache lives in the host,
//      where the memory budget is the host's decision (`maxByte`).
//
// Correctness does not depend on the server honouring `Range`: a `200` with the whole body is
// accepted, cached whole, and every later read is served from it.

/**
 * @param {string} url
 * @param {{fetch?: typeof fetch, gap?: number, cache?: boolean, maxByte?: number, header?: Record<string,string>}} [opt]
 */
export function httpRangeReader(url, opt = {}) {
  const doFetch = opt.fetch ?? globalThis.fetch;
  const gap = opt.gap ?? 4096;
  const useCache = opt.cache ?? true;
  const maxByte = opt.maxByte ?? Infinity;
  const state = { byteRead: 0, requestCount: 0, readCount: 0, cacheHitByte: 0, size: 0, cacheByte: 0 };

  // Non-overlapping, sorted by start: [{ start, bytes }]. Adjacent chunks are merged on insert so
  // the lookup below never has to stitch across more pieces than the span actually needs.
  let chunk = [];

  const covered = (start, end) => {
    // Intervals inside [start, end) that no chunk covers.
    const missing = [];
    let at = start;
    for (const c of chunk) {
      const cEnd = c.start + c.bytes.length;
      if (cEnd <= at) continue;
      if (c.start >= end) break;
      if (c.start > at) missing.push([at, Math.min(c.start, end)]);
      at = Math.max(at, cEnd);
      if (at >= end) break;
    }
    if (at < end) missing.push([at, end]);
    return missing;
  };

  const insert = (start, bytes) => {
    if (bytes.length === 0) return;
    const end = start + bytes.length;
    const keep = [];
    let mStart = start;
    let mEnd = end;
    const piece = [];
    for (const c of chunk) {
      const cEnd = c.start + c.bytes.length;
      if (cEnd < mStart || c.start > mEnd) keep.push(c);
      else piece.push(c);
    }
    for (const c of piece) {
      mStart = Math.min(mStart, c.start);
      mEnd = Math.max(mEnd, c.start + c.bytes.length);
    }
    const merged = new Uint8Array(mEnd - mStart);
    for (const c of piece) merged.set(c.bytes, c.start - mStart);
    merged.set(bytes, start - mStart); // fresh bytes win; they are the same bytes if the file is immutable
    keep.push({ start: mStart, bytes: merged });
    keep.sort((a, b) => a.start - b.start);
    chunk = keep;
    state.cacheByte = chunk.reduce((s, c) => s + c.bytes.length, 0);
    if (state.cacheByte > maxByte) {
      // Budget exceeded: drop everything except what this read needs. Crude, bounded, correct.
      chunk = [{ start: mStart, bytes: merged }];
      state.cacheByte = merged.length;
    }
  };

  const fetchInterval = async ([start, end]) => {
    state.requestCount++;
    const res = await doFetch(url, { headers: { ...opt.header, Range: `bytes=${start}-${end - 1}` } });
    if (res.status === 200) {
      const whole = new Uint8Array(await res.arrayBuffer());
      state.byteRead += whole.length;
      state.size = whole.length;
      return [0, whole];
    }
    if (res.status !== 206) throw new Error(`range read ${start}-${end - 1}: HTTP ${res.status}`);
    const total = /\/(\d+)$/.exec(res.headers.get('content-range') ?? '');
    if (total) state.size = Number(total[1]);
    const got = new Uint8Array(await res.arrayBuffer());
    const from = Number(/bytes (\d+)-/.exec(res.headers.get('content-range') ?? '')?.[1] ?? start);
    state.byteRead += got.length;
    return [from, got];
  };

  /** @param {[number, number][]} span */
  const read = async (span) => {
    state.readCount++;
    const want = span.map(([o, l]) => [o, o + l]);
    const hold = useCache ? null : [];

    // Everything this read lacks, coalesced across ALL its spans, not per span.
    const lack = [];
    for (const [s, e] of want) {
      if (e <= s) continue;
      const miss = useCache ? covered(s, e) : [[s, e]];
      const hit = e - s - miss.reduce((t, [a, b]) => t + b - a, 0);
      state.cacheHitByte += hit;
      lack.push(...miss);
    }
    lack.sort((a, b) => a[0] - b[0]);
    const request = [];
    for (const iv of lack) {
      const last = request[request.length - 1];
      if (last && iv[0] <= last[1] + gap) last[1] = Math.max(last[1], iv[1]);
      else request.push([iv[0], iv[1]]);
    }

    const fetched = await Promise.all(request.map(fetchInterval));
    for (const [start, bytes] of fetched) {
      if (useCache) insert(start, bytes);
      else hold.push({ start, bytes });
    }

    const source = useCache ? chunk : hold.sort((a, b) => a.start - b.start);
    const total = want.reduce((t, [s, e]) => t + Math.max(0, e - s), 0);
    const out = new Uint8Array(total);
    let at = 0;
    for (const [s, e] of want) {
      let pos = s;
      while (pos < e) {
        const c = source.find((c) => c.start <= pos && pos < c.start + c.bytes.length);
        if (!c) throw new Error(`range read: byte ${pos} was neither cached nor fetched`);
        const take = Math.min(e, c.start + c.bytes.length) - pos;
        out.set(c.bytes.subarray(pos - c.start, pos - c.start + take), at);
        at += take;
        pos += take;
      }
    }
    return out;
  };

  return { read, state };
}
