//! `index-geo-wasm` — WASM + C ABI binding for [`index_geo`].
//!
//! Same conventions as `index-wasm`, for the same reasons: **no `wasm-bindgen`**, a hand-written
//! raw-pointer C ABI, so one artifact serves the browser, Node, and native FFI without a JS glue
//! generator in the middle.
//!
//! # Why this exists
//!
//! `docs/research/geometry-sota.md` surveyed the geometry stack and found one hole with **no
//! published figure in any language**: point-in-polygon at scale in a browser. `geo-join` filled
//! the native half. This is the browser half.
//!
//! # The two ABI rules that matter
//!
//! 1. **Batch, don't iterate.** [`geo_locate_many`] takes N points and writes N results. A
//!    one-point-per-call API would measure the JS↔WASM boundary rather than the index — the
//!    boundary cost is comparable to the query itself.
//! 2. **The host owns the heap.** Buffers come from [`geo_alloc`] and go back via [`geo_free`].
//!    Note that any allocation can cause `memory.grow()`, which **detaches every existing
//!    JavaScript view** of the module's memory — so the host must re-create its typed arrays after
//!    calling in. This bit `js/index.mjs` before and the comment there says so.

use index_geo::{Bbox, CellIndex, Polygon, Ring};
use std::alloc::{alloc, dealloc, Layout};

/// Bumped on any incompatible ABI change. The host checks it before anything else.
///
/// `2` adds the viewport query ([`geo_polygon_in_view`]) and the portable file
/// ([`geo_serialize`] / [`geo_open`]). Nothing existing moved, so a `1` host still works against a
/// `2` module — but a `2` host asking a `1` module for a viewport would get a missing export rather
/// than an answer, and a version number is a cheaper way to find that out than a stack trace.
const ABI_VERSION: u32 = 2;

/// Opaque handle. The host sees a pointer and never its contents.
pub struct Handle {
    index: CellIndex,
    /// Reused across calls so a steady-state query does not allocate.
    out: Vec<u32>,
    /// Holds the bytes of the last [`geo_serialize`], so the host can read them without a copy.
    byte: Vec<u8>,
}

/// ABI version. Call first; if it does not match, stop.
#[no_mangle]
pub extern "C" fn geo_abi_version() -> u32 {
    ABI_VERSION
}

/// Allocate `len` bytes in linear memory. Returns null on zero length or failure.
///
/// # Safety
/// The pointer is valid for `len` bytes until passed to [`geo_free`] with the identical length.
#[no_mangle]
pub extern "C" fn geo_alloc(len: usize) -> *mut u8 {
    if len == 0 {
        return std::ptr::null_mut();
    }
    match Layout::from_size_align(len, 8) {
        // SAFETY: `len` is non-zero and the layout is valid, which is `alloc`'s contract.
        Ok(layout) => unsafe { alloc(layout) },
        Err(_) => std::ptr::null_mut(),
    }
}

/// Free memory from [`geo_alloc`].
///
/// # Safety
/// `ptr` must have come from [`geo_alloc`] with the same `len` and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn geo_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() || len == 0 {
        return;
    }
    if let Ok(layout) = Layout::from_size_align(len, 8) {
        dealloc(ptr, layout);
    }
}

/// Build an index from a flat ring description.
///
/// The wire format is deliberately dumb, because the host may be any language:
///
/// - `poly_of`: `ring_count` × `u32` — which polygon each ring belongs to, non-decreasing.
/// - `outer`:   `ring_count` × `u8`  — 1 for an outer ring, 0 for a hole.
/// - `offset`:  `(ring_count + 1)` × `u32` — start of each ring in `coord`, in *points*.
/// - `coord`:   `offset[ring_count]` × 2 × `f64` — interleaved lon, lat.
///
/// Returns an opaque handle, or **null** on any inconsistency rather than trapping, because this
/// parses data that may have arrived over a network.
///
/// # Safety
/// All four pointers must be readable for the lengths implied by `ring_count`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn geo_build(
    poly_of: *const u32,
    outer: *const u8,
    offset: *const u32,
    coord: *const f64,
    ring_count: usize,
    order: u32,
    min_lon: f64,
    min_lat: f64,
    max_lon: f64,
    max_lat: f64,
) -> *mut Handle {
    if poly_of.is_null() || outer.is_null() || offset.is_null() || coord.is_null() {
        return std::ptr::null_mut();
    }
    if ring_count == 0 || !(1..=16).contains(&order) {
        return std::ptr::null_mut();
    }
    if !(min_lon < max_lon && min_lat < max_lat) {
        return std::ptr::null_mut();
    }
    let poly_of = std::slice::from_raw_parts(poly_of, ring_count);
    let outer = std::slice::from_raw_parts(outer, ring_count);
    let offset = std::slice::from_raw_parts(offset, ring_count + 1);
    // Offsets must be non-decreasing, or the slicing below is nonsense.
    if offset.windows(2).any(|w| w[0] > w[1]) {
        return std::ptr::null_mut();
    }
    let total = offset[ring_count] as usize;
    let coord = std::slice::from_raw_parts(coord, total * 2);

    let poly_count = poly_of.iter().copied().max().unwrap_or(0) as usize + 1;
    let mut ring_of: Vec<Vec<Ring>> = vec![Vec::new(); poly_count];
    for i in 0..ring_count {
        let (s, e) = (offset[i] as usize, offset[i + 1] as usize);
        let pt: Vec<(f64, f64)> = (s..e).map(|j| (coord[2 * j], coord[2 * j + 1])).collect();
        if pt.len() < 4 {
            continue; // not a closed ring; skip rather than fail the whole build
        }
        ring_of[poly_of[i] as usize].push(Ring { outer: outer[i] != 0, pt });
    }

    let bounds = Bbox { x0: min_lon, y0: min_lat, x1: max_lon, y1: max_lat };
    let poly: Vec<Polygon> = ring_of.into_iter().map(Polygon::new).collect();
    let index = CellIndex::build(poly, order, bounds);
    Box::into_raw(Box::new(Handle { index, out: Vec::new(), byte: Vec::new() }))
}

/// Release a handle from [`geo_build`].
///
/// # Safety
/// `h` must have come from [`geo_build`] and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn geo_close(h: *mut Handle) {
    if !h.is_null() {
        drop(Box::from_raw(h));
    }
}

/// Occupied cells in the index, or 0 for a null handle.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_cell_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.cell_count() as u32)
}

/// Cells that answer with no geometry at all, or 0 for a null handle.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_interior_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.interior_count() as u32)
}

/// Polygons in the index, or 0 for a null handle.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_polygon_count(h: *const Handle) -> u32 {
    h.as_ref().map_or(0, |x| x.index.polygon_count() as u32)
}

/// Locate a single point. Returns the polygon id, or `u32::MAX` for none.
///
/// Provided for completeness. **Prefer [`geo_locate_many`]** — see the module docs on why a
/// per-point API measures the wrong thing.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_locate(h: *const Handle, lon: f64, lat: f64) -> u32 {
    h.as_ref().map_or(u32::MAX, |x| x.index.locate(lon, lat).unwrap_or(u32::MAX))
}

/// Locate `n` points. `xy` is `n` interleaved `(lon, lat)` `f64` pairs.
///
/// Results are written into the handle's internal buffer; the host reads them with
/// [`geo_result_ptr`]. Returns the number of results, or `u32::MAX` on error.
///
/// Note that `u32::MAX` is what a WASM `i32` return of `-1` means to JavaScript — the host must
/// convert with `>>> 0` before comparing.
///
/// # Safety
/// `h` must be a live handle and `xy` readable for `n * 2` `f64`s.
#[no_mangle]
pub unsafe extern "C" fn geo_locate_many(h: *mut Handle, xy: *const f64, n: usize) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    if xy.is_null() && n != 0 {
        return u32::MAX;
    }
    if n > u32::MAX as usize {
        return u32::MAX;
    }
    let xy = std::slice::from_raw_parts(xy, n * 2);
    handle.out.clear();
    handle.out.resize(n, u32::MAX);
    handle.index.locate_many(xy, &mut handle.out);
    n as u32
}

/// Pointer to the results of the last [`geo_locate_many`].
///
/// **Invalidated by any call that can allocate**, including the next `geo_locate_many`.
///
/// # Safety
/// `h` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_result_ptr(h: *const Handle) -> *const u32 {
    h.as_ref().map_or(std::ptr::null(), |x| x.out.as_ptr())
}

/// Every polygon that can intersect the viewport, ascending, written to the result buffer.
///
/// Returns the count, or `u32::MAX` for a null handle. The host reads the ids with
/// [`geo_result_ptr`], the same buffer [`geo_locate_many`] uses — a viewport query and a batch
/// lookup are the same table read two ways, and they share the same output path.
///
/// `budget` is the cover's cell budget. `p9-sfc-2d.md` measures the trade: 64 answers a 1 %
/// viewport at 1.14× over-fetch with 14 key ranges. Zero is clamped to one.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_polygon_in_view(
    h: *mut Handle,
    min_lon: f64,
    min_lat: f64,
    max_lon: f64,
    max_lat: f64,
    budget: u32,
) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    let view = Bbox { x0: min_lon, y0: min_lat, x1: max_lon, y1: max_lat };
    handle.out = handle.index.polygon_in_view(view, (budget as usize).max(1));
    handle.out.len() as u32
}

/// Serialize the index into the handle's own buffer and return its byte length.
///
/// The bytes stay owned by the handle so the host can read them with [`geo_bytes_ptr`] without a
/// second allocation. Returns `u32::MAX` for a null handle or a file that will not fit a `u32`.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_serialize(h: *mut Handle) -> u32 {
    let Some(handle) = h.as_mut() else { return u32::MAX };
    handle.byte = handle.index.to_bytes();
    if handle.byte.len() > u32::MAX as usize {
        handle.byte = Vec::new();
        return u32::MAX;
    }
    handle.byte.len() as u32
}

/// Pointer to the bytes of the last [`geo_serialize`].
///
/// **Invalidated by any call that can allocate**, including the next `geo_serialize`.
///
/// # Safety
/// `h` must be null or a live handle.
#[no_mangle]
pub unsafe extern "C" fn geo_bytes_ptr(h: *const Handle) -> *const u8 {
    h.as_ref().map_or(std::ptr::null(), |x| x.byte.as_ptr())
}

/// Open an index from bytes written by [`geo_serialize`], skipping the build entirely.
///
/// Returns null on any malformed input rather than trapping — a browser fetches this file over the
/// network, and a truncated response has to be an error the host can report, not a WASM trap that
/// takes the page's whole module with it.
///
/// # Safety
/// `ptr` must be readable for `len` bytes.
#[no_mangle]
pub unsafe extern "C" fn geo_open(ptr: *const u8, len: usize) -> *mut Handle {
    if ptr.is_null() || len == 0 {
        return std::ptr::null_mut();
    }
    let byte = std::slice::from_raw_parts(ptr, len);
    match CellIndex::from_bytes(byte) {
        Ok(index) => Box::into_raw(Box::new(Handle { index, out: Vec::new(), byte: Vec::new() })),
        Err(_) => std::ptr::null_mut(),
    }
}
