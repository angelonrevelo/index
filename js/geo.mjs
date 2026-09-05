// index-geo — JavaScript host for the WASM point-location engine.
//
// Same shape as `js/index.mjs`, and for the same reasons: no wasm-bindgen, a hand-written C ABI,
// so Node, the browser, an edge worker and a native FFI consume one artifact.
//
// Two rules carried over from the text host because they are just as true here:
//
//   1. **`memory.grow()` detaches every view of the buffer, even `grow(0)`.** Every helper below
//      re-derives its typed array from `memory.buffer` on each use rather than caching one.
//      Caching is the bug you cannot reproduce.
//   2. **WASM has no unsigned 32-bit return type.** Every `u32` arrives in JS as a signed i32, so
//      `u32::MAX` shows up as -1. Coerce at every boundary with `>>> 0`.
//
// The third rule is specific to this module: **batch your queries.** `locateMany` exists because
// the cost of crossing the JS↔WASM boundary is comparable to a single point location. Calling
// `locate` in a loop measures the boundary, not the index.

const ABI_VERSION = 1;

const u32 = (n) => n >>> 0;
export const NONE = 0xffffffff;

export class GeoIndex {
  #exports;
  #handle;

  constructor(exports, handle) {
    this.#exports = exports;
    this.#handle = handle;
  }

  /**
   * Instantiate the module and build an index from rings.
   *
   * `polygon` is an array of polygons; each polygon is an array of rings; each ring is a flat
   * array (or Float64Array) of interleaved lon/lat, closed (first point repeated last).
   *
   * A ring may instead be `{ pt, outer }` to say explicitly whether it is an outer boundary or a
   * hole. **Use that form for MultiPolygons**: an island is a second *outer* ring, not a hole, and
   * the convenient default here -- first ring outer, rest holes -- silently turns every island
   * into a hole. That default is right for a simple polygon and wrong for 88 Philippine provinces,
   * which is exactly the mistake this comment exists to stop.
   *
   * `order` is the grid resolution. `bench/roadmap/p10-geo-join.md` sweeps it and finds an
   * interior optimum at 6-8: past it the index grows faster than it prunes.
   */
  static async build(wasmBytes, polygon, { order = 8, bounds = null } = {}) {
    const { instance } = await WebAssembly.instantiate(wasmBytes, {});
    const e = instance.exports;
    if (e.geo_abi_version() !== ABI_VERSION) throw new Error('geo ABI mismatch');

    // Flatten to the wire format the ABI documents.
    const polyOf = [];
    const outer = [];
    const offset = [0];
    const coord = [];
    let box = bounds;
    for (let p = 0; p < polygon.length; p++) {
      for (let r = 0; r < polygon[p].length; r++) {
        const raw = polygon[p][r];
        const ring = raw && raw.pt ? raw.pt : raw;
        const isOuter = raw && raw.pt ? (raw.outer ? 1 : 0) : (r === 0 ? 1 : 0);
        polyOf.push(p);
        outer.push(isOuter);
        for (let i = 0; i < ring.length; i += 2) coord.push(ring[i], ring[i + 1]);
        offset.push(coord.length / 2);
      }
    }
    if (!box) {
      let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
      for (let i = 0; i < coord.length; i += 2) {
        if (coord[i] < x0) x0 = coord[i];
        if (coord[i] > x1) x1 = coord[i];
        if (coord[i + 1] < y0) y0 = coord[i + 1];
        if (coord[i + 1] > y1) y1 = coord[i + 1];
      }
      // A hair of padding, so a point exactly on the eastern or northern edge still lands in a
      // cell rather than falling off the grid.
      const px = (x1 - x0) * 1e-9 || 1e-9;
      const py = (y1 - y0) * 1e-9 || 1e-9;
      box = { minLon: x0 - px, minLat: y0 - py, maxLon: x1 + px, maxLat: y1 + py };
    }

    const ringCount = polyOf.length;
    if (ringCount === 0) throw new Error('no rings');

    const pPoly = u32(e.geo_alloc(ringCount * 4));
    const pOuter = u32(e.geo_alloc(ringCount));
    const pOff = u32(e.geo_alloc((ringCount + 1) * 4));
    const pCoord = u32(e.geo_alloc(coord.length * 8));
    if (!pPoly || !pOuter || !pOff || !pCoord) throw new Error('geo_alloc failed');

    // Written AFTER every allocation, because each alloc may grow memory and detach views.
    new Uint32Array(e.memory.buffer, pPoly, ringCount).set(polyOf);
    new Uint8Array(e.memory.buffer, pOuter, ringCount).set(outer);
    new Uint32Array(e.memory.buffer, pOff, ringCount + 1).set(offset);
    new Float64Array(e.memory.buffer, pCoord, coord.length).set(coord);

    const handle = e.geo_build(
      pPoly, pOuter, pOff, pCoord, ringCount, order,
      box.minLon, box.minLat, box.maxLon, box.maxLat,
    );

    e.geo_free(pPoly, ringCount * 4);
    e.geo_free(pOuter, ringCount);
    e.geo_free(pOff, (ringCount + 1) * 4);
    e.geo_free(pCoord, coord.length * 8);

    if (!handle) throw new Error('geo_build rejected the input');
    return new GeoIndex(e, handle);
  }

  get cellCount() { return u32(this.#exports.geo_cell_count(this.#handle)); }
  get interiorCount() { return u32(this.#exports.geo_interior_count(this.#handle)); }
  get polygonCount() { return u32(this.#exports.geo_polygon_count(this.#handle)); }

  /** Which polygon contains this point? Returns `NONE` for none. Prefer `locateMany`. */
  locate(lon, lat) {
    return u32(this.#exports.geo_locate(this.#handle, lon, lat));
  }

  /**
   * Locate a batch. `xy` is interleaved lon/lat. Returns a `Uint32Array` of polygon ids, with
   * `NONE` for points inside no polygon.
   *
   * The returned array is a **copy**, because the module's own buffer is invalidated by the next
   * call and by anything that grows memory.
   */
  locateMany(xy) {
    const e = this.#exports;
    const n = xy.length >>> 1;
    if (n === 0) return new Uint32Array(0);

    const p = u32(e.geo_alloc(xy.length * 8));
    if (!p) throw new Error('geo_alloc failed');
    new Float64Array(e.memory.buffer, p, xy.length).set(xy);

    const got = u32(e.geo_locate_many(this.#handle, p, n));
    if (got === NONE) {
      e.geo_free(p, xy.length * 8);
      throw new Error('geo_locate_many failed');
    }
    const rp = u32(e.geo_result_ptr(this.#handle));
    const out = new Uint32Array(e.memory.buffer, rp, got).slice();
    e.geo_free(p, xy.length * 8);
    return out;
  }

  close() {
    if (this.#handle) {
      this.#exports.geo_close(this.#handle);
      this.#handle = 0;
    }
  }
}

/**
 * Pure-JavaScript point-in-polygon, the baseline this is measured against.
 *
 * This is the same crossing-number algorithm `@turf/boolean-point-in-polygon` implements, written
 * out so the comparison is against an equivalent computation rather than against a library's
 * wrapper overhead. Deliberately includes the bounding-box reject, because a competent hand-rolled
 * version has one and beating a strawman proves nothing.
 */
export function scanLocate(polygon, bbox, lon, lat) {
  for (let p = 0; p < polygon.length; p++) {
    const b = bbox[p];
    if (lon < b[0] || lon > b[2] || lat < b[1] || lat > b[3]) continue;
    // Inside ANY outer ring and inside NO hole. Not "inside ring 0" -- a MultiPolygon has one
    // outer ring per island.
    let hit = false;
    let holed = false;
    for (let r = 0; r < polygon[p].length && !holed; r++) {
      const raw = polygon[p][r];
      const ring = raw && raw.pt ? raw.pt : raw;
      const isOuter = raw && raw.pt ? !!raw.outer : r === 0;
      if (isOuter && hit) continue;
      let inside = false;
      const n = ring.length >>> 1;
      let j = n - 1;
      for (let i = 0; i < n; i++) {
        const xi = ring[2 * i], yi = ring[2 * i + 1];
        const xj = ring[2 * j], yj = ring[2 * j + 1];
        if ((yi > lat) !== (yj > lat) && lon < ((xj - xi) * (lat - yi)) / (yj - yi) + xi) {
          inside = !inside;
        }
        j = i;
      }
      if (isOuter) { if (inside) hit = true; }
      else if (inside) { holed = true; }
    }
    if (hit && !holed) return p;
  }
  return NONE;
}
