//! `index-geo` — point location over polygons, as a range-readable index.
//!
//! This answers *"which polygon contains this point?"* for a whole batch of points, which is the
//! query behind every choropleth join, every "what region am I in", and every spatial filter that
//! precedes a text search. It also answers *"which polygons are on screen?"* — the same table, read
//! as key ranges instead of probed at one key.
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
//! Morton's 29. One index, two query shapes, no second structure to keep consistent. Both shapes
//! live here: [`CellIndex::locate`] and [`CellIndex::cover`] / [`CellIndex::polygon_in_view`].
//!
//! # Correctness
//!
//! The classification is exact rather than sampled, and that took four fixes to get right — all
//! four found by asserting agreement with a brute-force scan over every point of a dataset, never
//! by sampling. See [`CellIndex::build`] and `p10-geo-join.md`.

#![forbid(unsafe_code)]

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
    /// Do the two boxes share any point, edges included?
    #[inline]
    pub fn hits(&self, o: &Bbox) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
}

/// One polygon: outer rings plus holes, with a cached box for the polygon and for every ring.
///
/// The fields are private and the only constructor is [`Polygon::new`], because two invariants have
/// to hold together or the index and a brute-force scan answer different questions: the boxes must
/// match the rings, and **every coordinate must be finite**. See [`Polygon::new`] on the second.
#[derive(Clone, Debug)]
pub struct Polygon {
    ring: Vec<Ring>,
    ring_bbox: Vec<Bbox>,
    bbox: Bbox,
}

impl Polygon {
    /// Build a polygon, dropping any vertex that is not finite.
    ///
    /// **A non-finite vertex is not a slow path, it is a wrong answer.** `segment_cell` converts a
    /// coordinate to a column with `(v.floor() as i64)`, and Rust saturates a NaN cast to `0` — so
    /// the two segments meeting a NaN vertex rasterize into column 0 instead of where they belong,
    /// the cells they really cross are never marked boundary, and those cells are then filled as
    /// *interior* and answer with a stored id without testing any geometry. A ring with one NaN
    /// vertex produced **300 wrong answers out of 40,401 probes** at order 4 before this filter
    /// existed, and the index disagreed with its own [`CellIndex::locate_by_scan`].
    ///
    /// `geo_build` in `index-geo-wasm` parses coordinates that may have arrived over a network, so
    /// this is the boundary where the data stops being arbitrary. Dropping the vertex rather than
    /// rejecting the polygon keeps the index and the scan looking at the *identical* geometry,
    /// which is what makes the differential assertion mean anything.
    pub fn new(ring: Vec<Ring>) -> Self {
        let ring: Vec<Ring> = ring
            .into_iter()
            .map(|r| Ring {
                outer: r.outer,
                pt: r.pt.into_iter().filter(|p| p.0.is_finite() && p.1.is_finite()).collect(),
            })
            .collect();
        let mut bbox = Bbox::empty();
        let mut ring_bbox = Vec::with_capacity(ring.len());
        for r in &ring {
            let mut rb = Bbox::empty();
            for &(x, y) in &r.pt {
                rb.add(x, y);
                bbox.add(x, y);
            }
            ring_bbox.push(rb);
        }
        Polygon { ring, ring_bbox, bbox }
    }

    pub fn ring(&self) -> &[Ring] {
        &self.ring
    }
    pub fn bbox(&self) -> Bbox {
        self.bbox
    }
    /// Total vertices, after the finite filter in [`Polygon::new`].
    pub fn vertex_count(&self) -> usize {
        self.ring.iter().map(|r| r.pt.len()).sum()
    }
}

/// Hilbert index of a cell on a `2^order` grid, via the standard rotation walk.
///
/// `order == 0` is the single cell that covers everything, and its key is 0. Saying so explicitly
/// matters because the loop below would otherwise compute `1 << (order - 1)` on an underflowed
/// `u32` — a debug panic and a masked shift in release, two different answers from one public
/// function.
#[inline]
pub fn hilbert_2d(order: u32, mut x: u32, mut y: u32) -> u64 {
    if order == 0 {
        return 0;
    }
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
///
/// The per-ring box is checked before the crossing walk. A polygon in maphy's province set averages
/// 11 rings and its largest reaches 2,214 vertices, so rejecting a ring on four comparisons instead
/// of walking it is most of the query cost — the A/B is in `p10-geo-join.md`.
pub fn point_in_polygon(x: f64, y: f64, p: &Polygon) -> bool {
    if !p.bbox.has(x, y) {
        return false;
    }
    let mut hit = false;
    for (r, rb) in p.ring.iter().zip(p.ring_bbox.iter()) {
        if !rb.has(x, y) {
            continue;
        }
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

/// Set on a cell's single candidate slot to mean *"interior — answer with this id, test nothing"*.
///
/// Folding the flag into the id rather than carrying a parallel `Vec<u8>` is part of what makes the
/// cell table eight bytes per cell. Polygon ids are bounded below this bit at build time.
const INTERIOR_BIT: u32 = 1 << 31;

/// Point-location index over a polygon set.
///
/// # Memory
///
/// The cell table is **eight bytes per occupied cell** — a `u32` Hilbert key and a `u32` candidate
/// offset — plus four bytes per candidate membership. The key is a `u32` because a key at
/// `order <= 16` needs at most 32 bits, and halving the array a binary search walks is worth more
/// than the bytes it saves: the search is the cache-missing part of a lookup, and it is what makes
/// the grid resolution have an interior optimum at all.
pub struct CellIndex {
    order: u32,
    bounds: Bbox,
    /// Sorted Hilbert keys of occupied cells.
    key: Vec<u32>,
    /// `cand[start[i]..start[i + 1]]` are cell `i`'s candidate polygons, ascending.
    start: Vec<u32>,
    /// Candidate polygon ids. A cell whose single candidate carries [`INTERIOR_BIT`] is interior
    /// and answers without geometry.
    cand: Vec<u32>,
    interior_count: usize,
    poly: Vec<Polygon>,
}

/// One horizontal crossing of a ring with a grid row's centre line.
struct Crossing {
    row: u32,
    x: f64,
    ring: u32,
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
        self.interior_count
    }
    /// Candidate memberships stored across every cell.
    pub fn candidate_count(&self) -> usize {
        self.cand.len()
    }
    /// Bytes the cell table occupies, excluding the polygon geometry it points at.
    pub fn table_byte(&self) -> usize {
        (self.key.len() + self.start.len() + self.cand.len()) * 4
    }
    /// The polygons, in id order.
    pub fn polygon(&self) -> &[Polygon] {
        &self.poly
    }

    #[inline]
    fn side(&self) -> f64 {
        (1u32 << self.order) as f64
    }

    /// Grid cell of a point already known to lie inside `bounds`.
    #[inline]
    fn cell_of(&self, x: f64, y: f64) -> (u32, u32) {
        let n = self.side();
        let lim = (1u32 << self.order) - 1;
        let cx = (((x - self.bounds.x0) / (self.bounds.x1 - self.bounds.x0) * n) as u32).min(lim);
        let cy = (((y - self.bounds.y0) / (self.bounds.y1 - self.bounds.y0) * n) as u32).min(lim);
        (cx, cy)
    }

    /// Build the index.
    ///
    /// `order` is the grid resolution; `p10-geo-join.md` sweeps it and finds an **interior
    /// optimum at 6–8** — past it the index grows faster than it prunes and the binary search
    /// stops fitting in cache.
    ///
    /// # Correctness, and the four things that make it exact
    ///
    /// 1. Boundary cells are found by exact segment traversal, never by sampling.
    /// 2. A boundary cell also carries **the polygons containing its centre**, not only those whose
    ///    rings cross it. Independently simplified neighbours leave metre-wide slivers, so a cell
    ///    crossed by polygon A's edge can lie inside polygon B whose ring never enters the cell.
    ///    The union is provably a superset: if any point of a cell is inside P while the centre is
    ///    not, then P's boundary crosses the cell and P is already listed.
    /// 3. Candidate lists are sorted, so "first match" means the same thing here as in a scan when
    ///    polygons overlap — which tile-clipped fragments genuinely do.
    /// 4. Non-finite vertices are dropped by [`Polygon::new`] before they can rasterize into
    ///    column 0 and leave a real boundary cell filled as interior.
    ///
    /// # Why one row sweep and not two nested loops
    ///
    /// The obvious build does rule 2 and rule 3 as separate passes: for each polygon, test the
    /// centre of every cell in its box; then, for each boundary cell, test every polygon. Those are
    /// `O(cell × ring_len)` and `O(boundary_cell × polygon)`, and profiling the bin behind
    /// `p10-geo-join.md` found them to be **94 % of build time on the province set and 75 % on the
    /// municipal one** — 6,015 ms and 3,010 ms respectively at L=12.
    ///
    /// Both fall out of one **row sweep** instead. Every ring edge is turned into the horizontal
    /// crossings it makes with the row centre lines, bucketed by row. A row is then swept left to
    /// right carrying a per-ring parity, so the set of polygons containing a cell centre is known
    /// in `O(1)` per cell — for *every* polygon at once, in ascending id, which is also the
    /// tie-break rule. Cost falls to the total edge-row span plus one visit per cell, and the two
    /// rules stop being separate passes that can disagree.
    pub fn build(poly: Vec<Polygon>, order: u32, bounds: Bbox) -> Self {
        assert!((1..=16).contains(&order), "order must be 1..=16");
        assert!(
            bounds.x0 < bounds.x1 && bounds.y0 < bounds.y1,
            "bounds must be non-degenerate (x0 < x1 and y0 < y1)"
        );
        assert!(
            poly.len() < INTERIOR_BIT as usize,
            "too many polygons; the top id bit flags an interior cell"
        );
        let n = 1i64 << order;
        let nf = n as f64;
        let (bw, bh) = (bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        let to_cellf = |x: f64, y: f64| -> (f64, f64) {
            ((x - bounds.x0) / bw * nf, (y - bounds.y0) / bh * nf)
        };
        let row_y = |cy: u32| -> f64 { bounds.y0 + (cy as f64 + 0.5) / nf * bh };
        let col_x = |cx: u32| -> f64 { bounds.x0 + (cx as f64 + 0.5) / nf * bw };

        // 1. Boundary cells: every cell any ring segment passes through.
        let mut raw: Vec<(u32, u32, u32)> = Vec::new(); // (row, column, polygon)
        let mut buf = Vec::new();
        for (pi, p) in poly.iter().enumerate() {
            for r in p.ring.iter() {
                for w in r.pt.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    let (ax, ay) = to_cellf(a.0, a.1);
                    let (bx, by) = to_cellf(b.0, b.1);
                    buf.clear();
                    segment_cell(ax, ay, bx, by, n, &mut buf);
                    for &(cx, cy) in &buf {
                        raw.push((cy, cx, pi as u32));
                    }
                }
            }
        }
        raw.sort_unstable();
        raw.dedup();

        // Grouped: `bcand[bcell[i].2 .. bcell[i + 1].2]` are boundary cell `i`'s polygons.
        let mut bcell: Vec<(u32, u32, u32)> = Vec::new(); // (row, column, candidate start)
        let mut bcand: Vec<u32> = Vec::with_capacity(raw.len());
        for &(row, col, pid) in &raw {
            if bcell.last().map(|c| (c.0, c.1)) != Some((row, col)) {
                bcell.push((row, col, bcand.len() as u32));
            }
            bcand.push(pid);
        }
        drop(raw);
        // Row directory into `bcell`, so the sweep finds a row's boundary cells without a hash map.
        let mut brow: Vec<u32> = vec![0; n as usize + 1];
        for c in &bcell {
            brow[c.0 as usize + 1] += 1;
        }
        for i in 0..n as usize {
            brow[i + 1] += brow[i];
        }

        // 2. Every crossing of a ring with a row's centre line, bucketed by row.
        //
        // The predicate and the x it solves for are written exactly as `point_in_ring` writes them,
        // so the sweep and a direct point test cannot disagree about a tie.
        let mut ring_poly: Vec<u32> = Vec::new();
        let mut ring_hole: Vec<bool> = Vec::new();
        let mut cross: Vec<Crossing> = Vec::new();
        for (pi, p) in poly.iter().enumerate() {
            for r in p.ring.iter() {
                let gr = ring_poly.len() as u32;
                ring_poly.push(pi as u32);
                ring_hole.push(!r.outer);
                if r.pt.len() < 3 {
                    continue;
                }
                let mut j = r.pt.len() - 1;
                for i in 0..r.pt.len() {
                    let (xi, yi) = r.pt[i];
                    let (xj, yj) = r.pt[j];
                    j = i;
                    // Row centres sit at `y0 + (cy + 0.5) / n * bh`; inverting that bounds the rows
                    // this edge can reach, and the exact half-open test below decides each one.
                    let flo = (yi.min(yj) - bounds.y0) / bh * nf - 0.5;
                    let fhi = (yi.max(yj) - bounds.y0) / bh * nf - 0.5;
                    let r0 = flo.floor().max(0.0) as i64;
                    let r1 = (fhi.ceil() as i64).min(n - 1);
                    for cy in r0..=r1 {
                        let y = row_y(cy as u32);
                        if (yi > y) == (yj > y) {
                            continue;
                        }
                        cross.push(Crossing {
                            row: cy as u32,
                            x: (xj - xi) * (y - yi) / (yj - yi) + xi,
                            ring: gr,
                        });
                    }
                }
            }
        }
        cross.sort_unstable_by(|a, b| (a.row, a.x).partial_cmp(&(b.row, b.x)).unwrap());

        // 3. Sweep each row. `active` holds, ascending, every polygon containing the cell centre
        //    the sweep is standing on — which is both the interior answer and the centre
        //    augmentation a boundary cell needs, computed once instead of twice.
        let mut parity = vec![false; ring_poly.len()];
        let mut outer_odd = vec![0i32; poly.len()];
        let mut hole_odd = vec![0i32; poly.len()];
        let mut inside = vec![false; poly.len()];
        let mut active: Vec<u32> = Vec::new();
        let mut interior: Vec<(u32, u32, u32)> = Vec::new(); // (row, column, polygon)
        let mut extra: Vec<Vec<u32>> = vec![Vec::new(); bcell.len()];
        let mut at = 0usize;
        while at < cross.len() {
            let row = cross[at].row;
            let mut end = at;
            while end < cross.len() && cross[end].row == row {
                end += 1;
            }
            let ev = &cross[at..end];
            // Only cells whose centre lies between the first and last crossing can be inside
            // anything: a closed ring crosses any line an even number of times, so parity is back
            // to zero once the last crossing is passed.
            let lo = ((ev[0].x - bounds.x0) / bw * nf - 0.5).floor().max(0.0) as i64;
            let hi = (((ev[ev.len() - 1].x - bounds.x0) / bw * nf - 0.5).ceil() as i64).min(n - 1);
            let mut k = 0usize;
            let mut bi = brow[row as usize] as usize;
            let brow_end = brow[row as usize + 1] as usize;
            for cx in lo..=hi {
                let mx = col_x(cx as u32);
                while k < ev.len() && ev[k].x <= mx {
                    let gr = ev[k].ring as usize;
                    let p = ring_poly[gr] as usize;
                    parity[gr] = !parity[gr];
                    let step = if parity[gr] { 1 } else { -1 };
                    if ring_hole[gr] {
                        hole_odd[p] += step;
                    } else {
                        outer_odd[p] += step;
                    }
                    let now = outer_odd[p] > 0 && hole_odd[p] == 0;
                    if now != inside[p] {
                        inside[p] = now;
                        match active.binary_search(&(p as u32)) {
                            Ok(idx) => {
                                active.remove(idx);
                            }
                            Err(idx) => active.insert(idx, p as u32),
                        }
                    }
                    k += 1;
                }
                if active.is_empty() {
                    continue;
                }
                while bi < brow_end && bcell[bi].1 < cx as u32 {
                    bi += 1;
                }
                if bi < brow_end && bcell[bi].1 == cx as u32 {
                    extra[bi].extend_from_slice(&active);
                } else {
                    interior.push((row, cx as u32, active[0]));
                }
            }
            // Reset only what this row touched.
            for e in ev {
                let gr = e.ring as usize;
                parity[gr] = false;
                let p = ring_poly[gr] as usize;
                outer_odd[p] = 0;
                hole_odd[p] = 0;
                inside[p] = false;
            }
            active.clear();
            at = end;
        }

        // 4. Pack, ordered by Hilbert key.
        let mut slot: Vec<(u32, u32, bool)> = Vec::with_capacity(bcell.len() + interior.len());
        for (i, c) in bcell.iter().enumerate() {
            slot.push((hilbert_2d(order, c.1, c.0) as u32, i as u32, false));
        }
        for (i, c) in interior.iter().enumerate() {
            slot.push((hilbert_2d(order, c.1, c.0) as u32, i as u32, true));
        }
        slot.sort_unstable_by_key(|e| e.0);

        let mut key = Vec::with_capacity(slot.len());
        let mut start = Vec::with_capacity(slot.len() + 1);
        let mut cand: Vec<u32> = Vec::with_capacity(bcand.len() + interior.len());
        let mut interior_count = 0usize;
        start.push(0u32);
        for &(k, i, is_interior) in &slot {
            if is_interior {
                cand.push(interior[i as usize].2 | INTERIOR_BIT);
                interior_count += 1;
            } else {
                let i = i as usize;
                let s = bcell[i].2 as usize;
                let e = bcell.get(i + 1).map_or(bcand.len(), |c| c.2 as usize);
                let mut list: Vec<u32> = bcand[s..e].to_vec();
                list.extend_from_slice(&extra[i]);
                list.sort_unstable();
                list.dedup();
                cand.extend_from_slice(&list);
            }
            key.push(k);
            start.push(cand.len() as u32);
        }

        CellIndex { order, bounds, key, start, cand, interior_count, poly }
    }

    /// Slot of the cell holding `(x, y)`, or `None` when the point is outside `bounds` or its cell
    /// is absent from the table.
    #[inline]
    fn slot_of(&self, x: f64, y: f64) -> Option<usize> {
        if !self.bounds.has(x, y) {
            return None;
        }
        let (cx, cy) = self.cell_of(x, y);
        let k = hilbert_2d(self.order, cx, cy) as u32;
        let i = self.key.partition_point(|&e| e < k);
        if i >= self.key.len() || self.key[i] != k {
            return None; // ocean: answered with no geometry at all
        }
        Some(i)
    }

    /// Which polygon contains `(x, y)`? Lowest id wins when polygons overlap.
    #[inline]
    pub fn locate(&self, x: f64, y: f64) -> Option<u32> {
        let i = self.slot_of(x, y)?;
        let (s, e) = (self.start[i] as usize, self.start[i + 1] as usize);
        let first = self.cand[s];
        if first & INTERIOR_BIT != 0 {
            return Some(first & !INTERIOR_BIT);
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
        self.poly.iter().position(|p| point_in_polygon(x, y, p)).map(|i| i as u32)
    }

    // -----------------------------------------------------------------------------------------
    // Viewport — the second query shape the Hilbert key was chosen for.
    // -----------------------------------------------------------------------------------------

    /// The contiguous key range a quadtree cell at `lvl` occupies.
    ///
    /// The Hilbert curve is defined recursively by quadrant, so a cell at level `lvl` owns exactly
    /// `4^(order - lvl)` consecutive keys starting at its own key scaled by that span. That is the
    /// property which turns a rectangle into a *few ranges* rather than a set of cells.
    #[inline]
    fn key_span(&self, lvl: u32, cx: u32, cy: u32) -> (u32, u32) {
        let below = self.order - lvl;
        let span = 1u64 << (2 * below);
        let base = hilbert_2d(lvl, cx, cy) * span;
        (base as u32, (base + span - 1) as u32)
    }

    /// Cover `view` with about `budget` contiguous **key** ranges, sorted and merged.
    ///
    /// Quadrant descent: a cell fully inside becomes a range, a cell fully outside is dropped, a
    /// straddling cell is split. When the budget runs out the remaining straddling cells are
    /// emitted whole, which trades over-fetch for a bounded range count — the trade S2's
    /// `max_cells` makes.
    ///
    /// # Which straddling cell to split next is the whole ball game
    ///
    /// The obvious descent keeps the pending cells on a **stack**, so what is holding the budget
    /// when it runs out is an accident of traversal order — and a level-2 cell covering a quarter
    /// of the map gets emitted whole. `p9-sfc-2d.md` measured that at **16.5× over-fetch** on
    /// maphy's 53,715 real POIs, and concluded over-fetch was dominated by point clustering rather
    /// than by cover coarseness. **That conclusion was wrong, and it was the stack's fault.**
    ///
    /// Splitting the cell with the largest area *outside* the rectangle first — a max-heap rather
    /// than a stack — takes the same budget to **1.14×** over-fetch at 100 % recall with *fewer*
    /// ranges (14 against 17). The re-measurement is in `p9-sfc-2d.md`.
    pub fn cover(&self, view: Bbox, budget: usize) -> Vec<(u32, u32)> {
        use std::collections::BinaryHeap;
        let mut out: Vec<(u32, u32)> = Vec::new();
        let Some((x0, y0, x1, y1)) = self.clip(view) else { return out };
        // (area outside the rectangle, level, cell x, cell y). A BinaryHeap is a max-heap, and the
        // largest excess is exactly what is worth splitting next.
        let mut heap: BinaryHeap<(u64, u32, u32, u32)> = BinaryHeap::new();
        let offer = |heap: &mut BinaryHeap<(u64, u32, u32, u32)>,
                         out: &mut Vec<(u32, u32)>,
                         lvl: u32,
                         cx: u32,
                         cy: u32| {
            let below = self.order - lvl;
            let size = 1u32 << below;
            let (ax0, ay0) = (cx << below, cy << below);
            let (ax1, ay1) = (ax0 + size - 1, ay0 + size - 1);
            if ax1 < x0 || ax0 > x1 || ay1 < y0 || ay0 > y1 {
                return; // disjoint
            }
            if (ax0 >= x0 && ax1 <= x1 && ay0 >= y0 && ay1 <= y1) || below == 0 {
                out.push(self.key_span(lvl, cx, cy)); // exact, never worth splitting
                return;
            }
            let side = 1u64 << below;
            let ox = (u64::from(ax1.min(x1)) + 1).saturating_sub(u64::from(ax0.max(x0)));
            let oy = (u64::from(ay1.min(y1)) + 1).saturating_sub(u64::from(ay0.max(y0)));
            heap.push((side * side - ox * oy, lvl, cx, cy));
        };
        offer(&mut heap, &mut out, 0, 0, 0);
        while out.len() + heap.len() < budget {
            let Some((_, lvl, cx, cy)) = heap.pop() else { break };
            for (dx, dy) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
                offer(&mut heap, &mut out, lvl + 1, cx * 2 + dx, cy * 2 + dy);
            }
        }
        for (_, lvl, cx, cy) in heap {
            out.push(self.key_span(lvl, cx, cy));
        }
        out.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(out.len());
        for r in out {
            match merged.last_mut() {
                Some(last) if r.0 <= last.1.saturating_add(1) => last.1 = last.1.max(r.1),
                _ => merged.push(r),
            }
        }
        merged
    }

    /// The cover of `view` as half-open **slot** ranges into the sorted cell table.
    ///
    /// This is the form a reader wants: a slot range is a contiguous run of the table, which the
    /// portable layout turns into one byte range. Ranges that hold no occupied cell are dropped, so
    /// the count here is scans actually issued rather than cells nominally covered.
    pub fn cover_slot(&self, view: Bbox, budget: usize) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        for (a, b) in self.cover(view, budget) {
            let lo = self.key.partition_point(|&k| k < a) as u32;
            let hi = self.key.partition_point(|&k| k <= b) as u32;
            if lo < hi {
                out.push((lo, hi));
            }
        }
        out
    }

    /// Every polygon that can intersect `view`, ascending and deduplicated.
    ///
    /// This is the map's own question — *what is on screen?* — answered by reading the same table as
    /// a range scan instead of probing it at one key. The answer is a **superset**: a cell in the
    /// cover lists a polygon that touches the cell, and the cover is a superset of the viewport.
    /// That is the right contract for a draw list, and a caller needing exactness tests the few
    /// candidates it gets back.
    pub fn polygon_in_view(&self, view: Bbox, budget: usize) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for (lo, hi) in self.cover_slot(view, budget) {
            let (s, e) = (self.start[lo as usize] as usize, self.start[hi as usize] as usize);
            out.extend(self.cand[s..e].iter().map(|&c| c & !INTERIOR_BIT));
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// `view` clipped to `bounds` and quantized to grid cells, or `None` when they do not meet.
    fn clip(&self, view: Bbox) -> Option<(u32, u32, u32, u32)> {
        if view.is_empty() || !view.hits(&self.bounds) {
            return None;
        }
        let (x0, y0) = self.cell_of(view.x0.max(self.bounds.x0), view.y0.max(self.bounds.y0));
        let (x1, y1) = self.cell_of(view.x1.min(self.bounds.x1), view.y1.min(self.bounds.y1));
        Some((x0, y0, x1, y1))
    }
}

// ---------------------------------------------------------------------------------------------
// Serialization.
// ---------------------------------------------------------------------------------------------

/// Byte length of the section table that follows [`MAGIC`].
///
/// Same shape as `index-text`'s `format.rs`, and for the same reason: a fixed-size table of `u64`
/// spans at a fixed head offset, so a range reader fetches `MAGIC.len() + TABLE_BYTE` bytes and
/// then seeks straight to the section it wants. `u64` offsets even though nothing here is near
/// 4 GB, because a 32-bit offset is the one format mistake that cannot be fixed after it ships.
pub const TABLE_BYTE: usize = 4 * 16 + 4 * 8 + 4 * 4;

#[inline]
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn put_u64(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}
#[inline]
fn get_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
#[inline]
fn get_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}
#[inline]
fn get_f64(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

impl CellIndex {
    /// Serialize to the portable single-file layout.
    ///
    /// The head is `MAGIC`, then four `(offset, length)` `u64` spans — key, offset, candidate,
    /// geometry — then the grid parameters. Everything after the head is little-endian and
    /// stride-regular, so each of the three cell-table sections is one range request and needs no
    /// parsing to index into.
    pub fn to_bytes(&self) -> Vec<u8> {
        let head = MAGIC.len() + TABLE_BYTE;
        let mut body: Vec<u8> = Vec::new();

        let key_at = head as u64;
        for &k in &self.key {
            put_u32(&mut body, k);
        }
        let start_at = head as u64 + body.len() as u64;
        for &s in &self.start {
            put_u32(&mut body, s);
        }
        let cand_at = head as u64 + body.len() as u64;
        for &c in &self.cand {
            put_u32(&mut body, c);
        }
        let geo_at = head as u64 + body.len() as u64;
        put_u32(&mut body, self.poly.len() as u32);
        for p in &self.poly {
            put_u32(&mut body, p.ring.len() as u32);
            for r in &p.ring {
                body.push(u8::from(r.outer));
                put_u32(&mut body, r.pt.len() as u32);
                for &(x, y) in &r.pt {
                    body.extend_from_slice(&x.to_le_bytes());
                    body.extend_from_slice(&y.to_le_bytes());
                }
            }
        }
        let geo_len = head as u64 + body.len() as u64 - geo_at;

        let mut out: Vec<u8> = Vec::with_capacity(head + body.len());
        out.extend_from_slice(&MAGIC);
        for (at, len) in [
            (key_at, self.key.len() as u64 * 4),
            (start_at, self.start.len() as u64 * 4),
            (cand_at, self.cand.len() as u64 * 4),
            (geo_at, geo_len),
        ] {
            put_u64(&mut out, at);
            put_u64(&mut out, len);
        }
        for v in [self.bounds.x0, self.bounds.y0, self.bounds.x1, self.bounds.y1] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        put_u32(&mut out, self.order);
        put_u32(&mut out, self.interior_count as u32);
        put_u32(&mut out, 0); // reserved
        put_u32(&mut out, 0); // reserved
        debug_assert_eq!(out.len(), head);
        out.extend_from_slice(&body);
        out
    }

    /// Read an index written by [`Self::to_bytes`].
    ///
    /// Returns a message rather than panicking, because this parses a file that may have arrived
    /// over a network. Every span is bounds-checked against the buffer, and the offsets in the
    /// offset section are checked against the candidate section's real length — a truncated file
    /// has to fail here, not as an out-of-range index during a query.
    pub fn from_bytes(b: &[u8]) -> Result<Self, &'static str> {
        let head = MAGIC.len() + TABLE_BYTE;
        if b.len() < head {
            return Err("shorter than the fixed head");
        }
        if b[..MAGIC.len()] != MAGIC {
            return Err("not an index-geo file");
        }
        let span = |i: usize| -> Option<(usize, usize)> {
            let at = get_u64(b, MAGIC.len() + i * 16)? as usize;
            let len = get_u64(b, MAGIC.len() + i * 16 + 8)? as usize;
            if at.checked_add(len)? > b.len() {
                return None;
            }
            Some((at, len))
        };
        let mut sec = [(0usize, 0usize); 4];
        for (i, s) in sec.iter_mut().enumerate() {
            *s = span(i).ok_or("a section span reaches past the end of the file")?;
        }
        let p = MAGIC.len() + 4 * 16;
        let bounds = Bbox {
            x0: get_f64(b, p).ok_or("truncated bounds")?,
            y0: get_f64(b, p + 8).ok_or("truncated bounds")?,
            x1: get_f64(b, p + 16).ok_or("truncated bounds")?,
            y1: get_f64(b, p + 24).ok_or("truncated bounds")?,
        };
        let order = get_u32(b, p + 32).ok_or("truncated order")?;
        if !(1..=16).contains(&order) {
            return Err("order is not 1..=16");
        }
        if !(bounds.x0 < bounds.x1 && bounds.y0 < bounds.y1) {
            return Err("degenerate bounds");
        }

        let read_u32 = |at: usize, len: usize| -> Vec<u32> {
            (0..len / 4).map(|i| get_u32(b, at + i * 4).unwrap_or(0)).collect()
        };
        let key = read_u32(sec[0].0, sec[0].1);
        let start = read_u32(sec[1].0, sec[1].1);
        let cand = read_u32(sec[2].0, sec[2].1);
        if start.len() != key.len() + 1 {
            return Err("the offset section is not one longer than the key section");
        }
        if start.first() != Some(&0) || start.last().map(|&v| v as usize) != Some(cand.len()) {
            return Err("the offsets do not span the candidate section");
        }
        if start.windows(2).any(|w| w[0] >= w[1]) {
            return Err("every cell must hold at least one candidate");
        }
        if key.windows(2).any(|w| w[0] >= w[1]) {
            return Err("cell keys are not strictly ascending");
        }

        let (mut at, end) = (sec[3].0, sec[3].0 + sec[3].1);
        let bad = || "truncated geometry";
        let poly_count = get_u32(b, at).ok_or_else(bad)? as usize;
        at += 4;
        let mut poly = Vec::new();
        for _ in 0..poly_count {
            let ring_count = get_u32(b, at).ok_or_else(bad)? as usize;
            at += 4;
            let mut ring = Vec::new();
            for _ in 0..ring_count {
                let outer = *b.get(at).ok_or_else(bad)? != 0;
                let pt_count = get_u32(b, at + 1).ok_or_else(bad)? as usize;
                at += 5;
                if pt_count.checked_mul(16).and_then(|n| at.checked_add(n)).unwrap_or(usize::MAX)
                    > end
                {
                    return Err(bad());
                }
                let pt = (0..pt_count)
                    .map(|i| {
                        let o = at + i * 16;
                        (
                            f64::from_le_bytes(b[o..o + 8].try_into().unwrap()),
                            f64::from_le_bytes(b[o + 8..o + 16].try_into().unwrap()),
                        )
                    })
                    .collect();
                at += pt_count * 16;
                ring.push(Ring { outer, pt });
            }
            poly.push(Polygon::new(ring));
        }
        if cand.iter().any(|&c| (c & !INTERIOR_BIT) as usize >= poly.len()) {
            return Err("a candidate names a polygon the file does not hold");
        }
        let interior_count = cand.iter().filter(|&&c| c & INTERIOR_BIT != 0).count();
        Ok(CellIndex { order, bounds, key, start, cand, interior_count, poly })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(x0: f64, y0: f64, x1: f64, y1: f64, outer: bool) -> Ring {
        Ring { outer, pt: vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1), (x0, y0)] }
    }

    fn sq(x0: f64, y0: f64, x1: f64, y1: f64) -> Polygon {
        Polygon::new(vec![ring(x0, y0, x1, y1, true)])
    }

    fn unit_bounds() -> Bbox {
        Bbox { x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0 }
    }

    /// Every claim of exactness in this module reduces to this: the index and a scan return the
    /// identical id at every probe of a grid, never at a sample of them.
    fn agrees(idx: &CellIndex, step: f64, b: Bbox) {
        let mut x = b.x0;
        while x <= b.x1 {
            let mut y = b.y0;
            while y <= b.y1 {
                assert_eq!(idx.locate(x, y), idx.locate_by_scan(x, y), "at ({x}, {y})");
                y += step;
            }
            x += step;
        }
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
        let p = Polygon::new(vec![ring(1.0, 1.0, 9.0, 9.0, true), ring(4.0, 4.0, 6.0, 6.0, false)]);
        let idx = CellIndex::build(vec![p], 7, unit_bounds());
        assert_eq!(idx.locate(2.0, 2.0), Some(0), "inside the ring");
        assert_eq!(idx.locate(5.0, 5.0), None, "inside the hole");
    }

    #[test]
    fn overlapping_polygons_resolve_to_the_lowest_id_like_a_scan() {
        // Tile-clipped fragments genuinely overlap; the index must agree with the scan on which
        // one wins, or the two are answering different questions.
        let idx =
            CellIndex::build(vec![sq(1.0, 1.0, 6.0, 6.0), sq(4.0, 4.0, 9.0, 9.0)], 6, unit_bounds());
        assert_eq!(idx.locate(5.0, 5.0), Some(0));
        assert_eq!(idx.locate(5.0, 5.0), idx.locate_by_scan(5.0, 5.0));
        assert_eq!(idx.locate(8.0, 8.0), Some(1));
    }

    #[test]
    fn the_index_agrees_with_a_scan_everywhere_on_a_grid() {
        // The property that matters, checked exhaustively rather than sampled: this is the shape of
        // assertion that caught every real bug recorded in p10-geo-join.md.
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
            agrees(&idx, 0.1, unit_bounds());
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
        assert_eq!(hilbert_2d(0, 0, 0), 0, "order 0 is one cell, not an underflowed shift");
    }

    // --- the exactness fuzz, and the edge cases it was written to reach ----------------------

    fn rnd(s: &mut u64) -> f64 {
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*s >> 11) as f64) / ((1u64 << 53) as f64)
    }

    #[test]
    fn fuzz_random_polygons_with_holes_agree_with_a_scan() {
        let mut s = 0xABCDEFu64;
        for trial in 0..30 {
            let mut poly = Vec::new();
            for _ in 0..(2 + trial % 3) {
                let (cx, cy) = (1.0 + rnd(&mut s) * 8.0, 1.0 + rnd(&mut s) * 8.0);
                let rad = 0.6 + rnd(&mut s) * 2.0;
                let k = 4 + (rnd(&mut s) * 10.0) as usize;
                let circle = |r: f64| -> Vec<(f64, f64)> {
                    let mut pt: Vec<(f64, f64)> = (0..k)
                        .map(|i| {
                            let a = i as f64 / k as f64 * std::f64::consts::TAU;
                            (cx + r * a.cos(), cy + r * a.sin())
                        })
                        .collect();
                    pt.push(pt[0]);
                    pt
                };
                let mut r = vec![Ring { outer: true, pt: circle(rad) }];
                if rnd(&mut s) < 0.5 {
                    r.push(Ring { outer: false, pt: circle(rad * 0.4) });
                }
                poly.push(Polygon::new(r));
            }
            for order in [3u32, 5, 7] {
                let idx = CellIndex::build(poly.clone(), order, unit_bounds());
                agrees(&idx, 10.0 / 90.0, unit_bounds());
            }
        }
    }

    #[test]
    fn an_enclave_inside_a_hole_is_found() {
        // A municipality wholly inside another's hole. The centre of a cell in the hole belongs to
        // the enclave and to nothing else, which is the case one interior id per cell has to get
        // right.
        let outer =
            Polygon::new(vec![ring(1.0, 1.0, 9.0, 9.0, true), ring(3.0, 3.0, 7.0, 7.0, false)]);
        let inner = Polygon::new(vec![ring(3.5, 3.5, 6.5, 6.5, true)]);
        for order in [3u32, 4, 5, 6, 7] {
            let idx = CellIndex::build(vec![outer.clone(), inner.clone()], order, unit_bounds());
            agrees(&idx, 0.1, unit_bounds());
        }
    }

    #[test]
    fn edges_lying_exactly_on_cell_boundaries_agree_with_a_scan() {
        let s = 10.0 / 16.0;
        for order in [4u32, 5] {
            let idx = CellIndex::build(
                vec![sq(2.0 * s, 3.0 * s, 9.0 * s, 11.0 * s)],
                order,
                unit_bounds(),
            );
            agrees(&idx, 0.05, unit_bounds());
        }
    }

    #[test]
    fn a_polygon_reaching_outside_bounds_is_still_exact_inside_them() {
        let idx = CellIndex::build(vec![sq(-5.0, -5.0, 5.0, 5.0)], 5, unit_bounds());
        agrees(&idx, 0.2, unit_bounds());
    }

    #[test]
    fn degenerate_and_empty_polygons_do_not_shift_the_answer() {
        let degenerate = Polygon::new(vec![Ring {
            outer: true,
            pt: vec![(2.0, 2.0), (2.0, 2.0), (2.0, 2.0), (2.0, 2.0)],
        }]);
        let sliver =
            Polygon::new(vec![Ring { outer: true, pt: vec![(1.0, 1.0), (8.0, 1.0), (1.0, 1.0)] }]);
        let empty = Polygon::new(Vec::new());
        for order in [4u32, 6] {
            let idx = CellIndex::build(
                vec![degenerate.clone(), sliver.clone(), empty.clone(), sq(3.0, 3.0, 7.0, 7.0)],
                order,
                unit_bounds(),
            );
            agrees(&idx, 0.1, unit_bounds());
        }
        assert_eq!(CellIndex::build(Vec::new(), 5, unit_bounds()).locate(5.0, 5.0), None);
    }

    #[test]
    fn a_non_finite_vertex_does_not_produce_a_phantom_interior_cell() {
        // The bug this is the regression test for: `(f64::NAN.floor() as i64)` saturates to 0, so
        // the segments meeting a NaN vertex rasterized into column 0, the cells they really crossed
        // were never marked boundary, and those cells then answered as INTERIOR without testing any
        // geometry — 300 wrong answers of 40,401 probes at order 4, disagreeing with a scan.
        for bad in [f64::NAN, f64::NEG_INFINITY, f64::INFINITY] {
            let p = Polygon::new(vec![Ring {
                outer: true,
                pt: vec![(2.0, 2.0), (8.0, 2.0), (8.0, 8.0), (bad, 8.0), (2.0, 8.0), (2.0, 2.0)],
            }]);
            assert!(
                p.bbox().x0.is_finite() && p.bbox().x1.is_finite(),
                "the box must not inherit the bad vertex"
            );
            for order in [4u32, 6] {
                let idx = CellIndex::build(vec![p.clone()], order, unit_bounds());
                agrees(&idx, 0.05, unit_bounds());
            }
        }
        assert_eq!(
            CellIndex::build(vec![sq(2.0, 2.0, 8.0, 8.0)], 5, unit_bounds()).locate(f64::NAN, 5.0),
            None,
            "a NaN query is outside every box"
        );
    }

    #[test]
    #[should_panic(expected = "non-degenerate")]
    fn degenerate_bounds_are_rejected_rather_than_silently_indexed() {
        // A zero-width box divides by zero in the cell transform and yields an index whose every
        // answer is a confident None. `index-geo-wasm` already refused this at its ABI; the library
        // refuses it too, so the two cannot disagree about what a valid index is.
        CellIndex::build(
            vec![sq(1.0, 1.0, 2.0, 2.0)],
            4,
            Bbox { x0: 0.0, y0: 0.0, x1: 0.0, y1: 5.0 },
        );
    }

    #[test]
    fn longitude_past_180_is_indexable_for_antimeridian_bounds() {
        // A viewport crossing 180 E is expressed by extending `bounds` past it (170..190) and
        // offering longitudes in the same frame. This records what that costs the caller: the
        // unwrapped -179 is NOT found, +181 is, and normalizing by +360 recovers it.
        let bb = Bbox { x0: 170.0, y0: -10.0, x1: 190.0, y1: 10.0 };
        let idx = CellIndex::build(vec![sq(175.0, -5.0, 185.0, 5.0)], 5, bb);
        assert_eq!(idx.locate(181.0, 0.0), Some(0));
        assert_eq!(idx.locate(-179.0, 0.0), None, "the caller normalizes into the bounds frame");
        assert_eq!(idx.locate(-179.0 + 360.0, 0.0), Some(0));
    }

    #[test]
    fn a_polygon_touching_the_pole_is_exact() {
        let bb = Bbox { x0: -180.0, y0: -90.0, x1: 180.0, y1: 90.0 };
        let idx = CellIndex::build(vec![sq(-10.0, 80.0, 10.0, 90.0)], 6, bb);
        for (x, y) in [(0.0, 90.0), (0.0, 85.0), (0.0, -90.0), (-180.0, 85.0), (180.0, 85.0)] {
            assert_eq!(idx.locate(x, y), idx.locate_by_scan(x, y), "at ({x}, {y})");
        }
    }

    // --- the viewport query -------------------------------------------------------------------

    #[test]
    fn a_cover_recovers_every_polygon_in_the_viewport() {
        let poly: Vec<Polygon> = (0..8)
            .map(|i| {
                let x = i as f64 * 1.2 + 0.3;
                sq(x, x, x + 0.8, x + 0.8)
            })
            .collect();
        let idx = CellIndex::build(poly.clone(), 7, unit_bounds());
        let view = Bbox { x0: 2.0, y0: 2.0, x1: 6.0, y1: 6.0 };
        let want: Vec<u32> =
            (0..poly.len() as u32).filter(|&i| poly[i as usize].bbox().hits(&view)).collect();
        for budget in [4usize, 8, 32, 128] {
            let got = idx.polygon_in_view(view, budget);
            for w in &want {
                assert!(got.contains(w), "budget {budget} lost polygon {w}: got {got:?}");
            }
        }
    }

    #[test]
    fn a_cover_reaches_every_occupied_cell_the_viewport_contains() {
        let idx = CellIndex::build(vec![sq(1.0, 1.0, 9.0, 9.0)], 6, unit_bounds());
        let view = Bbox { x0: 3.0, y0: 3.0, x1: 5.0, y1: 4.0 };
        let slot = idx.cover_slot(view, 64);
        let n = 1u32 << idx.order();
        let mut missing = 0;
        for cx in 0..n {
            for cy in 0..n {
                let mx = idx.bounds().x0 + (cx as f64 + 0.5) / n as f64 * 10.0;
                let my = idx.bounds().y0 + (cy as f64 + 0.5) / n as f64 * 10.0;
                if !view.has(mx, my) {
                    continue;
                }
                let Some(at) = idx.slot_of(mx, my) else { continue };
                if !slot.iter().any(|&(lo, hi)| at as u32 >= lo && (at as u32) < hi) {
                    missing += 1;
                }
            }
        }
        assert_eq!(missing, 0, "{missing} occupied cells inside the viewport were not covered");
    }

    #[test]
    fn a_viewport_outside_the_bounds_covers_nothing() {
        let idx = CellIndex::build(vec![sq(1.0, 1.0, 9.0, 9.0)], 6, unit_bounds());
        assert!(idx.cover_slot(Bbox { x0: 20.0, y0: 20.0, x1: 30.0, y1: 30.0 }, 16).is_empty());
        assert!(idx.cover_slot(Bbox::empty(), 16).is_empty());
    }

    // --- serialization --------------------------------------------------------------------------

    #[test]
    fn a_round_trip_answers_identically() {
        let poly = vec![
            Polygon::new(vec![ring(1.0, 1.0, 9.0, 9.0, true), ring(4.0, 4.0, 6.0, 6.0, false)]),
            sq(4.5, 4.5, 5.5, 5.5),
        ];
        let idx = CellIndex::build(poly, 6, unit_bounds());
        let byte = idx.to_bytes();
        let back = CellIndex::from_bytes(&byte).expect("round trip");
        assert_eq!(back.cell_count(), idx.cell_count());
        assert_eq!(back.interior_count(), idx.interior_count());
        assert_eq!(back.bounds(), idx.bounds());
        assert_eq!(back.order(), idx.order());
        let mut x = 0.0;
        while x <= 10.0 {
            let mut y = 0.0;
            while y <= 10.0 {
                assert_eq!(back.locate(x, y), idx.locate(x, y), "at ({x}, {y})");
                y += 0.1;
            }
            x += 0.1;
        }
    }

    #[test]
    fn a_truncated_or_foreign_file_errors_rather_than_panics() {
        let idx = CellIndex::build(vec![sq(2.0, 2.0, 8.0, 8.0)], 5, unit_bounds());
        let byte = idx.to_bytes();
        assert!(CellIndex::from_bytes(b"not an index").is_err());
        let mut wrong = byte.clone();
        wrong[0] = b'X';
        assert!(CellIndex::from_bytes(&wrong).is_err());
        for cut in [8usize, 40, 96, byte.len() / 2, byte.len() - 1] {
            assert!(CellIndex::from_bytes(&byte[..cut]).is_err(), "a file cut at {cut} must fail");
        }
    }

    #[test]
    fn the_head_is_a_fixed_size_a_range_reader_can_fetch_blind() {
        // The property `index-text`'s format.rs holds, for its reason: a reader must know how many
        // bytes to ask for BEFORE it knows anything about the file.
        assert_eq!(MAGIC.len() + TABLE_BYTE, 8 + 112);
        let idx = CellIndex::build(vec![sq(2.0, 2.0, 8.0, 8.0)], 5, unit_bounds());
        assert!(idx.to_bytes().len() > MAGIC.len() + TABLE_BYTE);
    }

    #[test]
    fn the_cell_table_is_eight_bytes_a_cell_plus_four_a_candidate() {
        let idx = CellIndex::build(vec![sq(1.0, 1.0, 9.0, 9.0)], 7, unit_bounds());
        // One u32 key and one u32 offset per cell, one trailing offset, one u32 per candidate.
        assert_eq!(
            idx.table_byte(),
            idx.cell_count() * 8 + 4 + idx.candidate_count() * 4
        );
        assert!(idx.candidate_count() >= idx.cell_count(), "every cell holds at least one");
    }
}
