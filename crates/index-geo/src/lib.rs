//! `index-geo` — point location over polygons, as a range-readable index.
//!
//! This answers *"which polygon contains this point?"* for a whole batch of points, which is the
//! query behind every choropleth join, every "what region am I in", and every spatial filter that
//! precedes a text search.
//!
//! The design is the one measured in `bench/roadmap/p10-geo-join.md`. Rasterize the polygon set
//! onto a `2^order` grid and sort the occupied cells by **Hilbert key**. A cell is then one of
//! three things:
//!
//!   - **absent** — no polygon touches it. Answered by a failed binary search, with no geometry.
//!   - **interior** — the cell lies wholly inside one polygon. Answered by its stored id, with
//!     **zero** point-in-polygon tests.
//!   - **boundary** — some ring passes through. Only the polygons listed on that cell are tested.
//!
//! Most points in a real workload land in the first two cases, which is why this is 28.2× faster
//! than a scan and 9.1× faster than a bbox-prefiltered scan on maphy's municipal polygons.
//!
//! # Why a Hilbert key and not a row-major cell id
//!
//! A point lookup would be happy with any cell ordering. Hilbert is chosen so that the *same*
//! structure also answers a **viewport** query as a small set of contiguous key ranges — measured
//! in `bench/roadmap/p9-sfc-2d.md` at 100 % recall with 17 ranges for a 1 % viewport, against
//! Morton's 29. One index, two query shapes, no second structure to keep consistent.
//!
//! # Correctness
//!
//! The classification is exact rather than sampled, and that took three fixes to get right — all
//! three found by asserting agreement with a brute-force scan over every point of a real dataset,
//! never by sampling. See [`CellIndex::build`] and `p10-geo-join.md`.

#![forbid(unsafe_code)]

use std::collections::HashMap;

/// File magic. Bumped if the layout changes incompatibly.
pub const MAGIC: [u8; 8] = *b"IDXGEO1\0";

/// A closed ring. `outer` distinguishes an outer boundary from a hole.
#[derive(Clone, Debug)]
pub struct Ring {
    pub outer: bool,
    /// `(lon, lat)` pairs. The first and last should coincide.
    pub pt: Vec<(f64, f64)>,
}

/// An axis-aligned box in lon/lat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bbox {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Bbox {
    pub fn empty() -> Self {
        Bbox { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN }
    }
    #[inline]
    pub fn add(&mut self, x: f64, y: f64) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }
    #[inline]
    pub fn has(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.x0 > self.x1 || self.y0 > self.y1
    }
}

/// One polygon: outer rings plus holes, with a cached bounding box.
#[derive(Clone, Debug)]
pub struct Polygon {
    pub ring: Vec<Ring>,
    pub bbox: Bbox,
}

impl Polygon {
    pub fn new(ring: Vec<Ring>) -> Self {
        let mut bbox = Bbox::empty();
        for r in &ring {
            for &(x, y) in &r.pt {
                bbox.add(x, y);
            }
        }
        Polygon { ring, bbox }
    }
}

/// Hilbert index of a cell on a `2^order` grid, via the standard rotation walk.
#[inline]
pub fn hilbert_2d(order: u32, mut x: u32, mut y: u32) -> u64 {
    let mut d: u64 = 0;
    let mut s: u32 = 1 << (order - 1);
    while s > 0 {
        let rx = u32::from((x & s) > 0);
        let ry = u32::from((y & s) > 0);
        d += (s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64;
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            core::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

/// Crossing-number point-in-ring.
///
/// The half-open `(yi > y) != (yj > y)` test counts a vertex exactly once, which is what makes a
/// point on a horizontal scanline through a vertex resolve consistently instead of twice.
#[inline]
fn point_in_ring(x: f64, y: f64, pt: &[(f64, f64)]) -> bool {
    if pt.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = pt.len() - 1;
    for i in 0..pt.len() {
        let (xi, yi) = pt[i];
        let (xj, yj) = pt[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Inside an outer ring and not inside a hole.
pub fn point_in_polygon(x: f64, y: f64, p: &Polygon) -> bool {
    if !p.bbox.has(x, y) {
        return false;
    }
    let mut hit = false;
    for r in &p.ring {
        if r.outer {
            if !hit && point_in_ring(x, y, &r.pt) {
                hit = true;
            }
        } else if point_in_ring(x, y, &r.pt) {
            return false;
        }
    }
    hit
}

/// Every cell whose rectangle the segment touches, in cell coordinates.
///
/// Exact per-column traversal: clip the segment to each integer column it spans, evaluate `y` at
/// both ends of that clipped piece, and emit every row between. A *sampled* walk would be shorter
/// and would silently miss a cell whose corner the segment clips — and a missed boundary cell is
/// then misclassified as interior, which answers **wrongly** rather than slowly.
fn segment_cell(x0: f64, y0: f64, x1: f64, y1: f64, n: i64, out: &mut Vec<(u32, u32)>) {
    let clampc = |v: f64| -> i64 { (v.floor() as i64).clamp(0, n - 1) };
    let (mut ax, mut ay, mut bx, mut by) = (x0, y0, x1, y1);
    if ax > bx {
        core::mem::swap(&mut ax, &mut bx);
        core::mem::swap(&mut ay, &mut by);
    }
    for cx in clampc(ax)..=clampc(bx) {
        let (sx, ex) = ((cx as f64).max(ax), ((cx + 1) as f64).min(bx));
        let (ly, hy) = if (bx - ax).abs() < 1e-12 {
            (ay.min(by), ay.max(by))
        } else {
            let ya = ay + (by - ay) * (sx - ax) / (bx - ax);
            let yb = ay + (by - ay) * (ex - ax) / (bx - ax);
            (ya.min(yb), ya.max(yb))
        };
        if !ly.is_finite() || !hy.is_finite() {
            continue;
        }
        for cy in clampc(ly)..=clampc(hy) {
            out.push((cx as u32, cy as u32));
        }
    }
}

/// Point-location index over a polygon set.
pub struct CellIndex {
    order: u32,
    bounds: Bbox,
    /// Sorted Hilbert keys of occupied cells.
    key: Vec<u64>,
    /// `cand[start[i]..start[i + 1]]` are cell `i`'s candidate polygons, ascending.
    start: Vec<u32>,
    cand: Vec<u32>,
    /// 1 = interior (answer directly), 0 = boundary (test the candidates).
    interior: Vec<u8>,
    poly: Vec<Polygon>,
}

impl CellIndex {
    /// Number of occupied cells.
    pub fn cell_count(&self) -> usize {
        self.key.len()
    }
    /// Number of polygons.
    pub fn polygon_count(&self) -> usize {
        self.poly.len()
    }
    pub fn order(&self) -> u32 {
        self.order
    }
    pub fn bounds(&self) -> Bbox {
        self.bounds
    }
    /// Cells that answer with no geometry at all.
    pub fn interior_count(&self) -> usize {
        self.interior.iter().filter(|&&f| f == 1).count()
    }

    /// Build the index.
    ///
    /// `order` is the grid resolution; `p10-geo-join.md` sweeps it and finds an **interior
    /// optimum at 6–8** — past it the index grows faster than it prunes and the binary search
    /// stops fitting in cache.
    ///
    /// # Correctness, and the three things that make it exact
    ///
    /// 1. Boundary cells are found by exact segment traversal, never by sampling.
    /// 2. A boundary cell also carries **the polygons containing its centre**, not only those whose
    ///    rings cross it. Independently simplified neighbours leave metre-wide slivers, so a cell
    ///    crossed by polygon A's edge can lie inside polygon B whose ring never enters the cell.
    ///    The union is provably a superset: if any point of a cell is inside P while the centre is
    ///    not, then P's boundary crosses the cell and P is already listed.
    /// 3. Candidate lists are sorted, so "first match" means the same thing here as in a scan when
    ///    polygons overlap — which tile-clipped fragments genuinely do.
    pub fn build(poly: Vec<Polygon>, order: u32, bounds: Bbox) -> Self {
        assert!((1..=16).contains(&order), "order must be 1..=16");
        let n = 1i64 << order;
        let nf = n as f64;
        let (bw, bh) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        let to_cellf = |x: f64, y: f64| -> (f64, f64) {
            ((x - bounds.x0) / bw * nf, (y - bounds.y0) / bh * nf)
        };
        let centre_of = |cx: u32, cy: u32| -> (f64, f64) {
            (
                bounds.x0 + (cx as f64 + 0.5) / nf * bw,
                bounds.y0 + (cy as f64 + 0.5) / nf * bh,
            )
        };

        // 1. Boundary cells: every cell any ring segment passes through.
        let mut boundary: HashMap<(u32, u32), Vec<u32>> = HashMap::new();
        let mut buf = Vec::new();
        for (pi, p) in poly.iter().enumerate() {
            for r in &p.ring {
                for w in r.pt.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let (ax, ay) = to_cellf(a.0, a.1);
                    let (bx, by) = to_cellf(b.0, b.1);
                    buf.clear();
                    segment_cell(ax, ay, bx, by, n, &mut buf);
                    for &c in &buf {
                        let e = boundary.entry(c).or_default();
                        if e.last() != Some(&(pi as u32)) && !e.contains(&(pi as u32)) {
                            e.push(pi as u32);
                        }
                    }
                }
            }
        }

        // 2. Interior cells: no boundary crosses them, so the centre decides the whole cell.
        let mut interior: HashMap<(u32, u32), u32> = HashMap::new();
        for (pi, p) in poly.iter().enumerate() {
            if p.bbox.is_empty() {
                continue;
            }
            let (lx, ly) = to_cellf(p.bbox.x0, p.bbox.y0);
            let (hx, hy) = to_cellf(p.bbox.x1, p.bbox.y1);
            let (lx, ly) = (lx.floor().max(0.0) as i64, ly.floor().max(0.0) as i64);
            let (hx, hy) = ((hx.ceil() as i64).min(n - 1), (hy.ceil() as i64).min(n - 1));
            for cx in lx..=hx {
                for cy in ly..=hy {
                    let c = (cx as u32, cy as u32);
                    if boundary.contains_key(&c) || interior.contains_key(&c) {
                        continue;
                    }
                    let (mx, my) = centre_of(c.0, c.1);
                    if point_in_polygon(mx, my, p) {
                        interior.insert(c, pi as u32);
                    }
                }
            }
        }

        // 3. A boundary cell also needs whichever polygon contains its centre. See the doc comment.
        for (c, list) in boundary.iter_mut() {
            let (mx, my) = centre_of(c.0, c.1);
            for (pi, p) in poly.iter().enumerate() {
                if !list.contains(&(pi as u32)) && point_in_polygon(mx, my, p) {
                    list.push(pi as u32);
                }
            }
            list.sort_unstable();
        }

        let mut all: Vec<((u32, u32), u64)> = boundary
            .keys()
            .chain(interior.keys())
            .map(|&c| (c, hilbert_2d(order, c.0, c.1)))
            .collect();
        all.sort_unstable_by_key(|e| e.1);

        let mut key = Vec::with_capacity(all.len());
        let mut start = Vec::with_capacity(all.len() + 1);
        let mut cand: Vec<u32> = Vec::new();
        let mut inter = Vec::with_capacity(all.len());
        start.push(0u32);
        for (c, k) in all {
            match boundary.get(&c) {
                Some(list) => {
                    cand.extend_from_slice(list);
                    inter.push(0u8);
                }
                None => {
                    cand.push(interior[&c]);
                    inter.push(1u8);
                }
            }
            key.push(k);
            start.push(cand.len() as u32);
        }

        CellIndex { order, bounds, key, start, cand, interior: inter, poly }
    }

    /// Which polygon contains `(x, y)`? Lowest id wins when polygons overlap.
    #[inline]
    pub fn locate(&self, x: f64, y: f64) -> Option<u32> {
        if !self.bounds.has(x, y) {
            return None;
        }
        let n = (1u32 << self.order) as f64;
        let lim = (1u32 << self.order) - 1;
        let cx = (((x - self.bounds.x0) / (self.bounds.x1 - self.bounds.x0) * n) as u32).min(lim);
        let cy = (((y - self.bounds.y0) / (self.bounds.y1 - self.bounds.y0) * n) as u32).min(lim);
        let k = hilbert_2d(self.order, cx, cy);
        let i = self.key.partition_point(|&e| e < k);
        if i >= self.key.len() || self.key[i] != k {
            return None; // ocean: answered with no geometry at all
        }
        let (s, e) = (self.start[i] as usize, self.start[i + 1] as usize);
        if self.interior[i] == 1 {
            return self.cand.get(s).copied();
        }
        self.cand[s..e]
            .iter()
            .find(|&&pid| point_in_polygon(x, y, &self.poly[pid as usize]))
            .copied()
    }

    /// Locate a batch. `out[i]` is the polygon id for `xy[2 * i], xy[2 * i + 1]`, or `u32::MAX`.
    ///
    /// Batching exists because the per-call cost of crossing a WASM boundary from JavaScript is
    /// comparable to the query itself. A one-point-per-call API measures the boundary, not the
    /// index.
    pub fn locate_many(&self, xy: &[f64], out: &mut [u32]) {
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.locate(xy[2 * i], xy[2 * i + 1]).unwrap_or(u32::MAX);
        }
    }

    /// Brute-force scan, for differential testing. Same tie-break as [`Self::locate`].
    pub fn locate_by_scan(&self, x: f64, y: f64) -> Option<u32> {
        self.poly
            .iter()
            .position(|p| point_in_polygon(x, y, p))
            .map(|i| i as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
        Polygon::new(vec![Ring {
            outer: true,
            pt: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)],
        }])
    }

    fn unit_bounds() -> Bbox {
        Bbox { x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0 }
    }

    #[test]
    fn a_point_inside_a_square_is_found() {
        let idx = CellIndex::build(vec![sq(2.0, 2.0, 8.0, 8.0)], 6, unit_bounds());
        assert_eq!(idx.locate(5.0, 5.0), Some(0));
        assert_eq!(idx.locate(1.0, 1.0), None);
        assert_eq!(idx.locate(9.5, 9.5), None);
    }

    #[test]
    fn a_hole_is_not_inside() {
        let p = Polygon::new(vec![
            Ring {
                outer: true,
                pt: vec![(1.0, 1.0), (9.0, 1.0), (9.0, 9.0), (1.0, 9.0), (1.0, 1.0)],
            },
            Ring {
                outer: false,
                pt: vec![(4.0, 4.0), (6.0, 4.0), (6.0, 6.0), (4.0, 6.0), (4.0, 4.0)],
            },
        ]);
        let idx = CellIndex::build(vec![p], 7, unit_bounds());
        assert_eq!(idx.locate(2.0, 2.0), Some(0), "inside the ring");
        assert_eq!(idx.locate(5.0, 5.0), None, "inside the hole");
    }

    #[test]
    fn overlapping_polygons_resolve_to_the_lowest_id_like_a_scan() {
        // Tile-clipped fragments genuinely overlap; the index must agree with the scan on which
        // one wins, or the two are answering different questions.
        let idx = CellIndex::build(
            vec![sq(1.0, 1.0, 6.0, 6.0), sq(4.0, 4.0, 9.0, 9.0)],
            6,
            unit_bounds(),
        );
        assert_eq!(idx.locate(5.0, 5.0), Some(0));
        assert_eq!(idx.locate(5.0, 5.0), idx.locate_by_scan(5.0, 5.0));
        assert_eq!(idx.locate(8.0, 8.0), Some(1));
    }

    #[test]
    fn the_index_agrees_with_a_scan_everywhere_on_a_grid() {
        // The property that matters, checked exhaustively rather than sampled: this is the shape
        // of assertion that caught all three real bugs recorded in p10-geo-join.md.
        let poly = vec![
            sq(1.0, 1.0, 4.0, 4.0),
            sq(3.5, 3.5, 7.0, 7.0),
            Polygon::new(vec![Ring {
                outer: true,
                pt: vec![(6.0, 1.0), (9.0, 2.0), (8.0, 5.0), (6.5, 3.0), (6.0, 1.0)],
            }]),
        ];
        for order in [4u32, 6, 8] {
            let idx = CellIndex::build(poly.clone(), order, unit_bounds());
            for i in 0..100 {
                for j in 0..100 {
                    let (x, y) = (i as f64 * 0.1, j as f64 * 0.1);
                    assert_eq!(
                        idx.locate(x, y),
                        idx.locate_by_scan(x, y),
                        "order {order} at ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn interior_cells_exist_and_answer_without_geometry() {
        let idx = CellIndex::build(vec![sq(1.0, 1.0, 9.0, 9.0)], 7, unit_bounds());
        assert!(
            idx.interior_count() > idx.cell_count() / 2,
            "a big square should be mostly interior, got {} of {}",
            idx.interior_count(),
            idx.cell_count()
        );
    }

    #[test]
    fn locate_many_matches_locate() {
        let idx = CellIndex::build(vec![sq(2.0, 2.0, 8.0, 8.0)], 6, unit_bounds());
        let xy = [5.0, 5.0, 0.5, 0.5, 7.0, 3.0];
        let mut out = [0u32; 3];
        idx.locate_many(&xy, &mut out);
        assert_eq!(out, [0, u32::MAX, 0]);
    }

    #[test]
    fn hilbert_is_a_bijection_on_a_small_grid() {
        let order = 4;
        let n = 1u32 << order;
        let mut seen = vec![false; (n * n) as usize];
        for x in 0..n {
            for y in 0..n {
                let k = hilbert_2d(order, x, y) as usize;
                assert!(!seen[k], "duplicate key {k}");
                seen[k] = true;
            }
        }
        assert!(seen.iter().all(|&b| b));
    }
}
