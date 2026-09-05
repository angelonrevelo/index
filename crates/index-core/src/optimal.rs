//! Optimal piecewise-linear approximation — the convex-hull algorithm (O'Rourke 1981),
//! as used by the PGM-index. Produces the **minimum** number of segments such that every
//! point is within `±epsilon` of its segment's line.
//!
//! Geometry (orientation tests, slope comparisons) is done in exact `i128` integer arithmetic
//! so the ε-bound is decided exactly, not subject to float drift. Only the final slope/intercept
//! of each closed segment is materialized as `f64`.

/// A point on the error-corridor boundary: `y` is a position offset by `±epsilon`, so it can
/// be negative — hence `i64`.
#[derive(Clone, Copy)]
struct Pt {
    x: u64,
    y: i64,
}

/// `a - b` as an exact `(dx, dy)` slope numerator/denominator pair.
#[inline]
fn sub(a: Pt, b: Pt) -> (i128, i128) {
    (a.x as i128 - b.x as i128, a.y as i128 - b.y as i128)
}

/// `s < t` for slopes, via cross-multiplication. Valid because every `dx` here is > 0
/// (keys are strictly increasing and hull points always precede the current point in x).
#[inline]
fn slope_lt(s: (i128, i128), t: (i128, i128)) -> bool {
    s.1 * t.0 < s.0 * t.1
}

#[inline]
fn slope_gt(s: (i128, i128), t: (i128, i128)) -> bool {
    s.1 * t.0 > s.0 * t.1
}

/// 2D cross product of `(a-o)` and `(b-o)`.
#[inline]
fn cross(o: Pt, a: Pt, b: Pt) -> i128 {
    let oa = sub(a, o);
    let ob = sub(b, o);
    oa.0 * ob.1 - oa.1 * ob.0
}

struct OptimalPla {
    epsilon: i64,
    lower: Vec<Pt>,
    upper: Vec<Pt>,
    first_x: u64,
    last_x: u64,
    lower_start: usize,
    upper_start: usize,
    points_in_hull: usize,
    rect: [Pt; 4],
}

impl OptimalPla {
    fn new(epsilon: i64) -> Self {
        let z = Pt { x: 0, y: 0 };
        OptimalPla {
            epsilon,
            lower: Vec::new(),
            upper: Vec::new(),
            first_x: 0,
            last_x: 0,
            lower_start: 0,
            upper_start: 0,
            points_in_hull: 0,
            rect: [z; 4],
        }
    }

    /// Try to extend the current segment with `(x, y)`. Returns `false` if it cannot stay
    /// within `±epsilon`; the caller then closes the segment and re-adds the point.
    fn add_point(&mut self, x: u64, y: i64) -> bool {
        self.last_x = x;
        let p1 = Pt { x, y: y + self.epsilon }; // upper corridor point
        let p2 = Pt { x, y: y - self.epsilon }; // lower corridor point

        if self.points_in_hull == 0 {
            self.first_x = x;
            self.rect[0] = p1;
            self.rect[1] = p2;
            self.upper.clear();
            self.lower.clear();
            self.upper.push(p1);
            self.lower.push(p2);
            self.upper_start = 0;
            self.lower_start = 0;
            self.points_in_hull = 1;
            return true;
        }

        if self.points_in_hull == 1 {
            self.rect[2] = p2;
            self.rect[3] = p1;
            self.upper.push(p1);
            self.lower.push(p2);
            self.points_in_hull = 2;
            return true;
        }

        let slope1 = sub(self.rect[2], self.rect[0]); // min feasible slope
        let slope2 = sub(self.rect[3], self.rect[1]); // max feasible slope

        let outside_line1 = slope_lt(sub(p1, self.rect[2]), slope1);
        let outside_line2 = slope_gt(sub(p2, self.rect[3]), slope2);
        if outside_line1 || outside_line2 {
            self.points_in_hull = 0; // signal close; rect still holds the finished segment
            return false;
        }

        // Upper side: does p1 tighten the max slope?
        if slope_lt(sub(p1, self.rect[1]), slope2) {
            let mut min = sub(self.lower[self.lower_start], p1);
            let mut min_i = self.lower_start;
            for i in (self.lower_start + 1)..self.lower.len() {
                let val = sub(self.lower[i], p1);
                if slope_gt(val, min) {
                    break;
                }
                min = val;
                min_i = i;
            }
            self.rect[1] = self.lower[min_i];
            self.rect[3] = p1;
            self.lower_start = min_i;

            let mut end = self.upper.len();
            while end >= self.upper_start + 2 && cross(self.upper[end - 2], self.upper[end - 1], p1) <= 0 {
                end -= 1;
            }
            self.upper.truncate(end);
            self.upper.push(p1);
        }

        // Lower side: does p2 tighten the min slope?
        if slope_gt(sub(p2, self.rect[0]), slope1) {
            let mut max = sub(self.upper[self.upper_start], p2);
            let mut max_i = self.upper_start;
            for i in (self.upper_start + 1)..self.upper.len() {
                let val = sub(self.upper[i], p2);
                if slope_lt(val, max) {
                    break;
                }
                max = val;
                max_i = i;
            }
            self.rect[0] = self.upper[max_i];
            self.rect[2] = p2;
            self.upper_start = max_i;

            let mut end = self.lower.len();
            while end >= self.lower_start + 2 && cross(self.lower[end - 2], self.lower[end - 1], p2) >= 0 {
                end -= 1;
            }
            self.lower.truncate(end);
            self.lower.push(p2);
        }

        self.points_in_hull += 1;
        true
    }

    /// Slope + intercept of the closed segment, anchored at `first_x`:
    /// `pos ≈ slope * (key - first_x) + intercept`.
    fn segment_line(&self) -> (u64, f64, f64) {
        if self.points_in_hull == 1 {
            // single-point segment: the corridor center is exactly the position
            let pos = (self.rect[0].y + self.rect[1].y) as f64 / 2.0;
            return (self.first_x, 0.0, pos);
        }
        let p0 = self.rect[0];
        let p1 = self.rect[1];
        let s1 = sub(self.rect[2], self.rect[0]); // min slope (dx, dy)
        let s2 = sub(self.rect[3], self.rect[1]); // max slope
        let min_slope = s1.1 as f64 / s1.0 as f64;
        let max_slope = s2.1 as f64 / s2.0 as f64;
        let slope = 0.5 * (min_slope + max_slope);

        let a = s1.0 * s2.1 - s1.1 * s2.0; // determinant of the two bounding lines
        let (ix, iy) = if a == 0 {
            // parallel bounds: anchor at the midpoint of the two left corners
            (
                (p0.x as f64 + p1.x as f64) * 0.5,
                (p0.y as f64 + p1.y as f64) * 0.5,
            )
        } else {
            let dx10 = p1.x as i128 - p0.x as i128;
            let dy10 = p1.y as i128 - p0.y as i128;
            let b = (dx10 * s2.1 - dy10 * s2.0) as f64 / a as f64;
            (p0.x as f64 + b * s1.0 as f64, p0.y as f64 + b * s1.1 as f64)
        };
        let intercept = iy - (ix - self.first_x as f64) * slope;
        (self.first_x, slope, intercept)
    }
}

/// Build the optimal ε-bounded segments over sorted, unique `keys`.
/// Each tuple is `(first_key, slope, intercept)`.
pub fn build_segments(keys: &[u64], epsilon: usize) -> Vec<(u64, f64, f64)> {
    let mut pla = OptimalPla::new(epsilon as i64);
    let mut segments = Vec::new();
    for (i, &k) in keys.iter().enumerate() {
        if !pla.add_point(k, i as i64) {
            segments.push(pla.segment_line()); // rect holds the just-closed segment
            let ok = pla.add_point(k, i as i64); // points_in_hull == 0 → reinitialize
            debug_assert!(ok);
        }
    }
    if pla.points_in_hull > 0 {
        segments.push(pla.segment_line());
    }
    segments
}
