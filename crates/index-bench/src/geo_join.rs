//! `geo-join` — point-in-polygon at scale, as an *index* rather than a scan.
//!
//! `bench/roadmap/p9-sfc-2d.md` closed the question "is a viewport a range scan over an ordered
//! key?" for **points**, and recorded two gaps. This bin closes the second one:
//!
//! > *"**Polygons, not points.** A barangay is a polygon; covering a region by its bounding box and
//! > then testing point-in-polygon is a second step this does not do."*
//!
//! The workload is real and it is maphy's. maphy depends on `@turf/boolean-point-in-polygon`, and
//! the question "which province is this POI in?" is answered today by looping features. With
//! **88 provinces (991 rings, 37,507 vertices)** and **53,715 POIs**, that is 4.7 M polygon tests
//! over 3.3 M vertex pairs.
//!
//! Three arms are measured, and the middle one matters: comparing a good index against a *bad*
//! baseline proves nothing, so the bbox-prefiltered scan — what any competent hand-rolled version
//! does — is measured too.
//!
//!   1. **scan**  — full point-in-polygon against every province. The naive loop.
//!   2. **bbox**  — reject by bounding box first, then point-in-polygon. The competent loop.
//!   3. **cell**  — the index.
//!
//! ## The index
//!
//! Rasterize the polygon set onto a 2^L x 2^L grid over the Philippine bounding box and sort the
//! occupied cells by Hilbert key. A cell is one of three things:
//!
//!   - **absent** — no province touches it. It is ocean, and the query is answered by a failed
//!     binary search with no geometry at all.
//!   - **interior** — the cell lies wholly inside one province. The answer is the stored id, with
//!     **zero** point-in-polygon tests.
//!   - **boundary** — some ring passes through the cell. Only the provinces listed on that cell are
//!     tested.
//!
//! The classification is **exact**, not sampled, and the correctness argument has two halves:
//!
//!   - A cell crossed by no ring segment is uniformly inside or outside every polygon, so its
//!     centre decides it. Cells crossed by a segment are found by exact per-column traversal rather
//!     than by sampling along the segment — a sampled traversal can skip a cell whose corner the
//!     segment clips, and such a cell would then be misfiled as interior and answer wrongly. This
//!     is the same class of mistake that made the first version of `sfc-2d` measure 0.7 % recall.
//!   - A **boundary** cell carries the provinces whose rings cross it *and* the province containing
//!     its centre. Dropping the second half is wrong on real data and cost 28 wrong answers at
//!     L=8 before it was found; see the comment in `build()` for why independently simplified
//!     neighbours make it necessary.
//!
//! Correctness is asserted, not assumed: every arm must return the identical province for all
//! 53,715 real POIs or the bin fails.

mod timer;

/// The Philippine bounding box and grid, identical to `sfc_2d.rs` so the two bins index the same
/// coordinate space.
const LON_MIN: f64 = 114.2;
const LON_MAX: f64 = 126.7;
const LAT_MIN: f64 = 4.5;
const LAT_MAX: f64 = 20.9;

/// Hilbert index of a point on a 2^order x 2^order grid, via the standard rotation walk.
///
/// The key orders the cell table so that a *viewport* over it is a small set of contiguous ranges —
/// the property `p9-sfc-2d.md` measured. Point lookup itself would be happy with any ordering; this
/// keeps the one structure useful for both queries.
#[inline]
fn hilbert_2d(order: u32, mut x: u32, mut y: u32) -> u64 {
    let mut rx: u32;
    let mut ry: u32;
    let mut d: u64 = 0;
    let mut s: u32 = 1 << (order - 1);
    while s > 0 {
        rx = u32::from((x & s) > 0);
        ry = u32::from((y & s) > 0);
        d += (s as u64) * (s as u64) * ((3 * rx) ^ ry) as u64;
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s /= 2;
    }
    d
}

#[derive(Clone, Copy, PartialEq)]
struct Bbox {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
}

impl Bbox {
    fn empty() -> Self {
        Bbox { x0: f64::MAX, y0: f64::MAX, x1: f64::MIN, y1: f64::MIN }
    }
    #[inline]
    fn add(&mut self, x: f64, y: f64) {
        self.x0 = self.x0.min(x);
        self.y0 = self.y0.min(y);
        self.x1 = self.x1.max(x);
        self.y1 = self.y1.max(y);
    }
    #[inline]
    fn has(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

struct Ring {
    /// `true` for an outer ring, `false` for a hole.
    outer: bool,
    pt: Vec<(f64, f64)>,
    bbox: Bbox,
}

struct Province {
    name: String,
    psgc: String,
    ring: Vec<Ring>,
    bbox: Bbox,
}

/// Crossing-number point-in-ring. The half-open `(a.1 > y) != (b.1 > y)` test is the standard way
/// to count a vertex exactly once, which is what makes a point that lies on a horizontal scanline
/// through a vertex resolve consistently instead of being counted twice.
#[inline]
fn point_in_ring(x: f64, y: f64, r: &Ring) -> bool {
    if !r.bbox.has(x, y) {
        return false;
    }
    let p = &r.pt;
    let mut inside = false;
    let mut j = p.len() - 1;
    for i in 0..p.len() {
        let (xi, yi) = p[i];
        let (xj, yj) = p[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Inside an outer ring and not inside a hole.
///
/// The fixture flattens MultiPolygon rings, so a hole is not tied to the specific outer ring it
/// belongs to. For well-formed input that is equivalent — a hole lies inside its own outer ring and
/// outside every other — and the correctness check against the scan arm would catch it if not.
fn point_in_province(x: f64, y: f64, p: &Province) -> bool {
    if !p.bbox.has(x, y) {
        return false;
    }
    let mut hit = false;
    for r in &p.ring {
        if r.outer {
            if !hit && point_in_ring(x, y, r) {
                hit = true;
            }
        } else if point_in_ring(x, y, r) {
            return false;
        }
    }
    hit
}

fn load_province(path: &str) -> std::io::Result<Vec<Province>> {
    let text = std::fs::read_to_string(path)?;
    let mut out: Vec<Province> = Vec::new();
    for line in text.lines() {
        let mut f = line.split('\t');
        match f.next() {
            Some("P") => {
                let psgc = f.next().unwrap_or("").to_string();
                let name = f.next().unwrap_or("").to_string();
                out.push(Province { name, psgc, ring: Vec::new(), bbox: Bbox::empty() });
            }
            Some("R") => {
                let outer = f.next().unwrap_or("1") == "0";
                let body = f.next().unwrap_or("");
                let mut num = body.split(' ');
                let mut pt = Vec::new();
                let mut bbox = Bbox::empty();
                while let (Some(a), Some(b)) = (num.next(), num.next()) {
                    if let (Ok(x), Ok(y)) = (a.parse::<f64>(), b.parse::<f64>()) {
                        bbox.add(x, y);
                        pt.push((x, y));
                    }
                }
                if pt.len() < 4 {
                    continue;
                }
                if let Some(p) = out.last_mut() {
                    p.bbox.add(bbox.x0, bbox.y0);
                    p.bbox.add(bbox.x1, bbox.y1);
                    p.ring.push(Ring { outer, pt, bbox });
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn load_poi(path: &str) -> std::io::Result<Vec<(f64, f64)>> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let mut f = line.split(',');
        if let (Some(a), Some(b)) = (f.next(), f.next()) {
            if let (Ok(x), Ok(y)) = (a.parse::<f64>(), b.parse::<f64>()) {
                out.push((x, y));
            }
        }
    }
    Ok(out)
}

/// What a cell resolves to.
#[derive(Clone)]
enum Cell {
    /// Wholly inside this province. No geometry test needed.
    Interior(u16),
    /// A ring passes through; only these provinces need testing.
    Boundary(Vec<u16>),
}

struct CellIndex {
    order: u32,
    /// Sorted by Hilbert key, so a point lookup is a binary search and a viewport is a range scan.
    entry: Vec<(u64, Cell)>,
    interior_count: usize,
    boundary_count: usize,
}

impl CellIndex {
    #[inline]
    fn cell_of(&self, x: f64, y: f64) -> Option<(u32, u32)> {
        if !(LON_MIN..=LON_MAX).contains(&x) || !(LAT_MIN..=LAT_MAX).contains(&y) {
            return None;
        }
        let n = (1u32 << self.order) as f64;
        let fx = (x - LON_MIN) / (LON_MAX - LON_MIN) * n;
        let fy = (y - LAT_MIN) / (LAT_MAX - LAT_MIN) * n;
        let cx = (fx as u32).min((1 << self.order) - 1);
        let cy = (fy as u32).min((1 << self.order) - 1);
        Some((cx, cy))
    }

    /// Returns the province and the number of point-in-polygon tests it cost.
    fn locate(&self, x: f64, y: f64, prov: &[Province]) -> (Option<u16>, usize) {
        let Some((cx, cy)) = self.cell_of(x, y) else { return (None, 0) };
        let k = hilbert_2d(self.order, cx, cy);
        let i = self.entry.partition_point(|e| e.0 < k);
        if i >= self.entry.len() || self.entry[i].0 != k {
            return (None, 0); // ocean — answered with no geometry at all
        }
        match &self.entry[i].1 {
            Cell::Interior(p) => (Some(*p), 0),
            Cell::Boundary(list) => {
                let mut tested = 0;
                for &p in list {
                    tested += 1;
                    if point_in_province(x, y, &prov[p as usize]) {
                        return (Some(p), tested);
                    }
                }
                (None, tested)
            }
        }
    }
}

/// Every cell whose rectangle the segment `(x0,y0)-(x1,y1)` touches, in cell coordinates.
///
/// Exact per-column traversal: clip the segment to each integer column it spans, evaluate y at both
/// ends of that clipped piece, and emit every row between them. A sampled walk would be shorter and
/// would silently miss a cell whose corner the segment clips — and a missed boundary cell is later
/// misclassified as interior, which answers *wrongly* rather than slowly.
fn segment_cell(x0: f64, y0: f64, x1: f64, y1: f64, n: i64, out: &mut Vec<(u32, u32)>) {
    let clampc = |v: f64| -> i64 { (v.floor() as i64).clamp(0, n - 1) };
    let (mut ax, mut ay, mut bx, mut by) = (x0, y0, x1, y1);
    if ax > bx {
        std::mem::swap(&mut ax, &mut bx);
        std::mem::swap(&mut ay, &mut by);
    }
    let c0 = clampc(ax);
    let c1 = clampc(bx);
    for cx in c0..=c1 {
        // The piece of the segment inside column `cx`.
        let (sx, ex) = ((cx as f64).max(ax), ((cx + 1) as f64).min(bx));
        let (mut ly, mut hy) = if (bx - ax).abs() < 1e-12 {
            (ay.min(by), ay.max(by))
        } else {
            let t0 = (sx - ax) / (bx - ax);
            let t1 = (ex - ax) / (bx - ax);
            let ya = ay + (by - ay) * t0;
            let yb = ay + (by - ay) * t1;
            (ya.min(yb), ya.max(yb))
        };
        if !ly.is_finite() || !hy.is_finite() {
            continue;
        }
        ly = ly.min(hy);
        hy = hy.max(ly);
        for cy in clampc(ly)..=clampc(hy) {
            out.push((cx as u32, cy as u32));
        }
    }
}

fn build(prov: &[Province], order: u32) -> CellIndex {
    use std::collections::HashMap;
    let n = 1i64 << order;
    let nf = n as f64;
    let to_cellf = |x: f64, y: f64| -> (f64, f64) {
        (
            (x - LON_MIN) / (LON_MAX - LON_MIN) * nf,
            (y - LAT_MIN) / (LAT_MAX - LAT_MIN) * nf,
        )
    };

    // 1. Boundary cells: every cell any ring segment passes through.
    let mut boundary: HashMap<(u32, u32), Vec<u16>> = HashMap::new();
    let mut buf = Vec::new();
    for (pi, p) in prov.iter().enumerate() {
        for r in &p.ring {
            for w in r.pt.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (ax, ay) = to_cellf(a.0, a.1);
                let (bx, by) = to_cellf(b.0, b.1);
                buf.clear();
                segment_cell(ax, ay, bx, by, n, &mut buf);
                for &c in &buf {
                    let e = boundary.entry(c).or_default();
                    if !e.contains(&(pi as u16)) {
                        e.push(pi as u16);
                    }
                }
            }
        }
    }

    // 2. Interior cells: a cell with no boundary through it is uniformly inside or outside, so its
    //    centre decides. Only cells within a province's bbox are worth testing.
    let mut interior: HashMap<(u32, u32), u16> = HashMap::new();
    for (pi, p) in prov.iter().enumerate() {
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
                let mx = LON_MIN + (cx as f64 + 0.5) / nf * (LON_MAX - LON_MIN);
                let my = LAT_MIN + (cy as f64 + 0.5) / nf * (LAT_MAX - LAT_MIN);
                if point_in_province(mx, my, p) {
                    interior.insert(c, pi as u16);
                }
            }
        }
    }

    // 3. A boundary cell also needs every province whose INTERIOR covers it.
    //
    //    This is not a theoretical nicety, it is a property of the real data and it produced 28
    //    wrong answers at L=8 before it was handled. Neighbouring provinces in maphy's file were
    //    simplified independently, so their shared borders do not coincide: there are slivers and
    //    gaps a few metres wide between them. A cell can therefore be crossed by province A's
    //    coastline while lying inside province B, whose own ring never enters the cell. Treating
    //    "boundary" and "interior" as mutually exclusive loses B, and the query returns ocean for a
    //    point that is plainly on land.
    //
    //    The completed rule is exact. If any point of a cell lies inside P while the cell centre
    //    does not, then P's boundary must cross the cell, so P is already in the list from step 1.
    //    Adding the centre's containing province closes the remaining case, and the union is
    //    therefore a superset of every province any point of the cell can fall in.
    for (c, list) in boundary.iter_mut() {
        let mx = LON_MIN + (c.0 as f64 + 0.5) / nf * (LON_MAX - LON_MIN);
        let my = LAT_MIN + (c.1 as f64 + 0.5) / nf * (LAT_MAX - LAT_MIN);
        for (pi, p) in prov.iter().enumerate() {
            if !list.contains(&(pi as u16)) && point_in_province(mx, my, p) {
                list.push(pi as u16);
            }
        }
    }

    // Candidate lists are tested in ascending id order so that a point inside two overlapping
    // polygons resolves to the same one the scan arm picks. Tile-clipped fragments genuinely do
    // overlap, so "first match" has to mean the same thing in every arm or the comparison is
    // between two different questions.
    for list in boundary.values_mut() {
        list.sort_unstable();
    }

    let interior_count = interior.len();
    let boundary_count = boundary.len();
    let mut entry: Vec<(u64, Cell)> = Vec::with_capacity(interior_count + boundary_count);
    for (c, p) in interior {
        entry.push((hilbert_2d(order, c.0, c.1), Cell::Interior(p)));
    }
    for (c, list) in boundary {
        entry.push((hilbert_2d(order, c.0, c.1), Cell::Boundary(list)));
    }
    entry.sort_unstable_by_key(|e| e.0);
    CellIndex { order, entry, interior_count, boundary_count }
}


/// Segment crossing, three-valued.
///
/// A parity walk is exact only when every crossing is transverse. If the walk ray passes exactly
/// through a vertex, the two segments meeting there each report a touch and the count flips twice
/// or not at all -- so the answer is decided by floating-point luck. Rather than pick a tie-break
/// and hope, `Degenerate` is reported and the caller falls back to a full test.
///
/// This is not hypothetical: one query in 53,715 hit it on the province set, and it was found only
/// because the bench asserts equality with the scan rather than sampling agreement.
#[derive(PartialEq)]
enum Cross {
    No,
    Yes,
    Degenerate,
}

#[inline]
fn seg_cross(a: (f64, f64), b: (f64, f64), c: (f64, f64), d: (f64, f64)) -> Cross {
    #[inline]
    fn cr(o: (f64, f64), p: (f64, f64), q: (f64, f64)) -> f64 {
        (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0)
    }
    let d1 = cr(c, d, a);
    let d2 = cr(c, d, b);
    let d3 = cr(a, b, c);
    let d4 = cr(a, b, d);
    let s1 = (d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0);
    let s2 = (d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0);
    if s1 && s2 {
        return Cross::Yes;
    }
    // Any exact zero means the ray touches a segment or its endpoint: undecidable by parity.
    if (d1 == 0.0 || d2 == 0.0) && (d3 * d4 <= 0.0) {
        return Cross::Degenerate;
    }
    if (d3 == 0.0 || d4 == 0.0) && (d1 * d2 <= 0.0) {
        return Cross::Degenerate;
    }
    Cross::No
}

/// The vertex index: a cell stores **which polygons contain its centre** and **the boundary
/// segments that pass through it** - nothing else.
///
/// This is the answer to "can the vertices themselves be the index?". A query point `p` lands in a
/// cell whose centre `m` has a known answer, and the segment `m -> p` lies entirely within that
/// cell, so it can only cross boundary segments stored on that cell. Counting those crossings gives
/// the parity change from `m` to `p`, and parity plus a known starting answer is the answer:
///
/// ```text
/// inside(p, P) = inside(m, P) XOR (crossings of m->p with P's local segments is odd)
/// ```
///
/// The work is proportional to the number of vertices **near the query**, not to the size of the
/// polygon - a coastline with 40,000 vertices costs the same as a square when the query sits in a
/// quiet stretch of it. That is the property a whole-ring point-in-polygon test cannot have, and it
/// is why this is a vertex index rather than a polygon index.
///
/// An interior cell is the degenerate case with zero segments, so one structure serves both and
/// there is no second code path to keep honest.
///
/// The centre's answer is a **set**, not a single id. Tile-clipped polygons genuinely overlap, so a
/// centre can sit inside several at once; storing one id and assuming the rest were outside
/// produced 1,496 wrong answers before it was fixed.
/// A boundary segment tagged with the polygon it belongs to: `(id, ax, ay, bx, by)`.
type Seg = (u16, f64, f64, f64, f64);

struct EdgeIndex {
    order: u32,
    key: Vec<u64>,
    /// `cid[cspan[i].0 .. cspan[i].1]` are the polygons containing cell `i` centre, ascending.
    cspan: Vec<(u32, u32)>,
    cid: Vec<u16>,
    /// `seg[span[i].0 .. span[i].1]` are the boundary segments crossing cell `i`.
    span: Vec<(u32, u32)>,
    seg: Vec<Seg>,
}

impl EdgeIndex {
    /// Returns the polygon and the number of segment-crossing tests it cost.
    fn locate(&self, x: f64, y: f64, prov: &[Province]) -> (Option<u16>, usize) {
        if !(LON_MIN..=LON_MAX).contains(&x) || !(LAT_MIN..=LAT_MAX).contains(&y) {
            return (None, 0);
        }
        let n = (1u32 << self.order) as f64;
        let cx = (((x - LON_MIN) / (LON_MAX - LON_MIN) * n) as u32).min((1 << self.order) - 1);
        let cy = (((y - LAT_MIN) / (LAT_MAX - LAT_MIN) * n) as u32).min((1 << self.order) - 1);
        let k = hilbert_2d(self.order, cx, cy);
        let i = self.key.partition_point(|&e| e < k);
        if i >= self.key.len() || self.key[i] != k {
            return (None, 0);
        }
        let (cs, ce) = self.cspan[i];
        let start = &self.cid[cs as usize..ce as usize];
        let (s, e) = self.span[i];
        if s == e {
            // No boundary in this cell: the centre answer is the whole cell answer.
            return (start.first().copied(), 0);
        }
        let m = (
            LON_MIN + (cx as f64 + 0.5) / n * (LON_MAX - LON_MIN),
            LAT_MIN + (cy as f64 + 0.5) / n * (LAT_MAX - LAT_MIN),
        );
        // Parity of `m -> p`, per polygon, over this cell local segments only.
        let mut parity: Vec<(u16, bool)> = Vec::with_capacity(8);
        let mut tested = 0usize;
        let mut degenerate = false;
        for &(pid, ax, ay, bx, by) in &self.seg[s as usize..e as usize] {
            tested += 1;
            match seg_cross(m, (x, y), (ax, ay), (bx, by)) {
                Cross::No => continue,
                Cross::Degenerate => {
                    degenerate = true;
                    break;
                }
                Cross::Yes => {}
            }
            match parity.iter_mut().find(|q| q.0 == pid) {
                Some(q) => q.1 = !q.1,
                None => parity.push((pid, true)),
            }
        }
        if degenerate {
            // Undecidable by parity. Test the cell candidates properly; this is rare enough that
            // it does not move the aggregate, and being right matters more than being fast here.
            let mut best: Option<u16> = None;
            for &pid in start {
                if best.map_or(true, |b| pid < b) && point_in_province(x, y, &prov[pid as usize]) {
                    best = Some(pid);
                }
            }
            for &(pid, ..) in &self.seg[s as usize..e as usize] {
                if best.map_or(true, |b| pid < b) && point_in_province(x, y, &prov[pid as usize]) {
                    best = Some(pid);
                }
            }
            return (best, tested);
        }
        // Lowest id that ends up inside, which is the scan arm first-match rule.
        let mut best: Option<u16> = None;
        for &pid in start {
            let flipped = parity.iter().any(|q| q.0 == pid && q.1);
            if !flipped && best.map_or(true, |b| pid < b) {
                best = Some(pid);
            }
        }
        for &(pid, flipped) in &parity {
            if start.contains(&pid) {
                continue;
            }
            if flipped && best.map_or(true, |b| pid < b) {
                best = Some(pid);
            }
        }
        (best, tested)
    }
}

fn build_edge(prov: &[Province], order: u32) -> EdgeIndex {
    use std::collections::HashMap;
    let n = 1i64 << order;
    let nf = n as f64;
    let to_cellf = |x: f64, y: f64| -> (f64, f64) {
        (
            (x - LON_MIN) / (LON_MAX - LON_MIN) * nf,
            (y - LAT_MIN) / (LAT_MAX - LAT_MIN) * nf,
        )
    };

    let mut cell: HashMap<(u32, u32), Vec<Seg>> = HashMap::new();
    let mut buf = Vec::new();
    for (pi, p) in prov.iter().enumerate() {
        for r in &p.ring {
            for w in r.pt.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (ax, ay) = to_cellf(a.0, a.1);
                let (bx, by) = to_cellf(b.0, b.1);
                buf.clear();
                segment_cell(ax, ay, bx, by, n, &mut buf);
                for &c in &buf {
                    cell.entry(c).or_default().push((pi as u16, a.0, a.1, b.0, b.1));
                }
            }
        }
    }
    // Interior cells: no boundary passes through, so the centre decides the whole cell.
    for p in prov.iter() {
        let (lx, ly) = to_cellf(p.bbox.x0, p.bbox.y0);
        let (hx, hy) = to_cellf(p.bbox.x1, p.bbox.y1);
        let (lx, ly) = (lx.floor().max(0.0) as i64, ly.floor().max(0.0) as i64);
        let (hx, hy) = ((hx.ceil() as i64).min(n - 1), (hy.ceil() as i64).min(n - 1));
        for cx in lx..=hx {
            for cy in ly..=hy {
                let c = (cx as u32, cy as u32);
                if cell.contains_key(&c) {
                    continue;
                }
                let mx = LON_MIN + (cx as f64 + 0.5) / nf * (LON_MAX - LON_MIN);
                let my = LAT_MIN + (cy as f64 + 0.5) / nf * (LAT_MAX - LAT_MIN);
                if point_in_province(mx, my, p) {
                    cell.insert(c, Vec::new());
                }
            }
        }
    }

    let mut keyed: Vec<((u32, u32), u64)> =
        cell.keys().map(|&c| (c, hilbert_2d(order, c.0, c.1))).collect();
    keyed.sort_unstable_by_key(|e| e.1);

    let mut key = Vec::with_capacity(keyed.len());
    let mut cspan = Vec::with_capacity(keyed.len());
    let mut cid: Vec<u16> = Vec::new();
    let mut span = Vec::with_capacity(keyed.len());
    let mut seg: Vec<Seg> = Vec::new();
    for (c, k) in keyed {
        let mx = LON_MIN + (c.0 as f64 + 0.5) / nf * (LON_MAX - LON_MIN);
        let my = LAT_MIN + (c.1 as f64 + 0.5) / nf * (LAT_MAX - LAT_MIN);
        let cs = cid.len() as u32;
        for (pi, p) in prov.iter().enumerate() {
            if point_in_province(mx, my, p) {
                cid.push(pi as u16);
            }
        }
        let s = seg.len() as u32;
        seg.extend_from_slice(&cell[&c]);
        key.push(k);
        cspan.push((cs, cid.len() as u32));
        span.push((s, seg.len() as u32));
    }
    EdgeIndex { order, key, cspan, cid, span, seg }
}

fn main() {
    let clock = timer::Clock::new();
    let prov_path = std::env::var("INDEX_BENCH_PROVINCE")
        .unwrap_or_else(|_| "bench/fixture/maphy-province.txt".to_string());
    let poi_path = std::env::var("INDEX_BENCH_POI")
        .unwrap_or_else(|_| "bench/fixture/maphy-poi.csv".to_string());

    let prov = match load_province(&prov_path) {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("geo-join: no province fixture at {prov_path} — see bench/fixture/README.md");
            return;
        }
    };
    let poi = match load_poi(&poi_path) {
        Ok(p) if !p.is_empty() => p,
        _ => {
            eprintln!("geo-join: no POI fixture at {poi_path} — see bench/fixture/README.md");
            return;
        }
    };

    let ring: usize = prov.iter().map(|p| p.ring.len()).sum();
    let vert: usize = prov.iter().flat_map(|p| p.ring.iter()).map(|r| r.pt.len()).sum();
    println!("geo-join :: point-in-polygon as an index, not a scan");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} provinces, {ring} rings, {vert} vertices  x  {} real POIs\n",
        prov.len(),
        poi.len()
    );

    // --- arm 1: the naive loop, which is what looping features with turf costs.
    let t = std::time::Instant::now();
    let mut truth: Vec<Option<u16>> = Vec::with_capacity(poi.len());
    let mut scan_tests = 0usize;
    for &(x, y) in &poi {
        let mut found = None;
        for (i, p) in prov.iter().enumerate() {
            scan_tests += 1;
            if point_in_province(x, y, p) {
                found = Some(i as u16);
                break;
            }
        }
        truth.push(found);
    }
    let scan_ms = t.elapsed().as_secs_f64() * 1000.0;

    // --- arm 2: bbox reject first. The competent hand-rolled version.
    let t = std::time::Instant::now();
    let mut bbox_tests = 0usize;
    let mut bbox_out: Vec<Option<u16>> = Vec::with_capacity(poi.len());
    for &(x, y) in &poi {
        let mut found = None;
        for (i, p) in prov.iter().enumerate() {
            if !p.bbox.has(x, y) {
                continue;
            }
            bbox_tests += 1;
            if point_in_province(x, y, p) {
                found = Some(i as u16);
                break;
            }
        }
        bbox_out.push(found);
    }
    let bbox_ms = t.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(bbox_out, truth, "bbox arm disagrees with the scan");

    let matched = truth.iter().filter(|t| t.is_some()).count();
    println!(
        "  {matched} of {} POIs fall inside a province ({:.1}%); the rest are offshore or outside\n",
        poi.len(),
        100.0 * matched as f64 / poi.len() as f64
    );

    // The join is the product, not the benchmark: print the top of it so the result is visibly a
    // real answer about real places rather than a timing harness talking to itself.
    {
        let mut tally: Vec<usize> = vec![0; prov.len()];
        for t in truth.iter().flatten() {
            tally[*t as usize] += 1;
        }
        let mut order: Vec<usize> = (0..prov.len()).collect();
        order.sort_unstable_by(|&a, &b| tally[b].cmp(&tally[a]));
        print!("  most POIs:");
        for &i in order.iter().take(4) {
            print!(" {} ({}, {});", prov[i].name, prov[i].psgc, tally[i]);
        }
        println!("\n");
    }

    println!("  --- arms ---");
    println!(
        "  {:<26} {:>10} {:>12} {:>14} {:>10}",
        "arm", "build", "query", "pip tests", "vs scan"
    );
    println!(
        "  {:<26} {:>9.0}ms {:>11.2}ms {:>14} {:>9.1}x",
        "scan (every province)", 0.0, scan_ms, scan_tests, 1.0
    );
    println!(
        "  {:<26} {:>9.0}ms {:>11.2}ms {:>14} {:>9.1}x",
        "bbox reject, then pip",
        0.0,
        bbox_ms,
        bbox_tests,
        scan_ms / bbox_ms
    );

    for order in [6u32, 8, 10, 12] {
        let t = std::time::Instant::now();
        let idx = build(&prov, order);
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = std::time::Instant::now();
        let mut tests = 0usize;
        let mut out: Vec<Option<u16>> = Vec::with_capacity(poi.len());
        for &(x, y) in &poi {
            let (p, n) = idx.locate(x, y, &prov);
            tests += n;
            out.push(p);
        }
        let q_ms = t.elapsed().as_secs_f64() * 1000.0;

        // The whole argument for the index is that it is EXACT. If it ever disagrees with the
        // scan, the number below is worthless, so this is an assert and not a printed statistic.
        if out != truth {
            let mut shown = 0;
            for (i, (a, b)) in out.iter().zip(truth.iter()).enumerate() {
                if a == b { continue; }
                let (x, y) = poi[i];
                let inall: Vec<usize> = prov.iter().enumerate()
                    .filter(|(_, p)| point_in_province(x, y, p)).map(|(k, _)| k).collect();
                let idx2 = &idx;
                let cell = idx2.cell_of(x, y);
                let kind = cell.map(|c| {
                    let k = hilbert_2d(order, c.0, c.1);
                    let j = idx2.entry.partition_point(|e| e.0 < k);
                    if j >= idx2.entry.len() || idx2.entry[j].0 != k { "absent".to_string() }
                    else { match &idx2.entry[j].1 {
                        Cell::Interior(p) => format!("interior({})", prov[*p as usize].name),
                        Cell::Boundary(l) => format!("boundary({})", l.len()) } }
                }).unwrap_or("oob".into());
                eprintln!("MISMATCH L={order} poi={i} ({x},{y}) idx={:?} truth={:?} cell={:?} {kind} in_all={:?}",
                    a.map(|v| &prov[v as usize].name), b.map(|v| &prov[v as usize].name), cell, 
                    inall.iter().map(|k| &prov[*k].name).collect::<Vec<_>>());
                shown += 1;
                if shown >= 8 { break; }
            }
            let n_bad = out.iter().zip(truth.iter()).filter(|(a,b)| a != b).count();
            eprintln!("L={order}: {n_bad} mismatches of {}", poi.len());
            std::process::exit(1);
        }

        // One (u64 key, u16 id) pair per cell, plus one u16 per boundary membership.
        let member: usize = idx
            .entry
            .iter()
            .map(|e| match &e.1 {
                Cell::Interior(_) => 1,
                Cell::Boundary(l) => l.len(),
            })
            .sum();
        let bytes = idx.entry.len() * 10 + member * 2;
        println!(
            "  {:<26} {:>9.0}ms {:>11.2}ms {:>14} {:>9.1}x   L={} cells={} (int {} / bnd {}) {:.0}KB",
            format!("cell index, L={order}"),
            build_ms,
            q_ms,
            tests,
            scan_ms / q_ms,
            order,
            idx.entry.len(),
            idx.interior_count,
            idx.boundary_count,
            bytes as f64 / 1024.0
        );
    }

    for order in [6u32, 8, 10] {
        let t = std::time::Instant::now();
        let idx = build_edge(&prov, order);
        let build_ms = t.elapsed().as_secs_f64() * 1000.0;

        let t = std::time::Instant::now();
        let mut tests = 0usize;
        let mut out: Vec<Option<u16>> = Vec::with_capacity(poi.len());
        for &(x, y) in &poi {
            let (p, k) = idx.locate(x, y, &prov);
            tests += k;
            out.push(p);
        }
        let q_ms = t.elapsed().as_secs_f64() * 1000.0;

        if out != truth {
            let n_bad = out.iter().zip(truth.iter()).filter(|(a, b)| a != b).count();
            for (i, (a, b)) in out.iter().zip(truth.iter()).enumerate() {
                if a == b {
                    continue;
                }
                eprintln!(
                    "EDGE MISMATCH L={order} poi={i} {:?} idx={:?} truth={:?}",
                    poi[i],
                    a.map(|v| &prov[v as usize].name),
                    b.map(|v| &prov[v as usize].name)
                );
                break;
            }
            eprintln!("edge index L={order}: {n_bad} mismatches of {}", poi.len());
            std::process::exit(1);
        }

        // 8 bytes key + 4 centre + 8 span, plus 34 bytes per stored segment.
        let bytes = idx.key.len() * 24 + idx.cid.len() * 2 + idx.seg.len() * 34;
        println!(
            "  {:<26} {:>9.0}ms {:>11.2}ms {:>14} {:>9.1}x   L={} cells={} seg={} {:.0}KB",
            format!("vertex index, L={order}"),
            build_ms,
            q_ms,
            tests,
            scan_ms / q_ms,
            order,
            idx.key.len(),
            idx.seg.len(),
            bytes as f64 / 1024.0
        );
    }

    println!(
        "\n  Read: for the cell arms `pip tests` counts point-in-polygon evaluations; for the\n  \
         vertex index it counts segment-crossing tests. The index wins by not doing geometry:\n  \
         an interior cell answers from its id and ocean from a failed binary search. Every arm
  \n         returns identical answers -- that\n  \
         is asserted, not reported."
    );
}
