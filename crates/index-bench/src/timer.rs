// Each bin compiles this module separately and uses a different subset of the API.
#![allow(dead_code)]

//! High-resolution per-operation timing.
//!
//! `std::time::Instant` on Windows quantizes to ~100 ns, which is the same order as a single
//! lookup — so it can't resolve a real p99. We use the x86 timestamp counter (`rdtsc`) instead:
//! sub-nanosecond resolution, with measurement overhead calibrated and subtracted. On non-x86
//! targets we fall back to `Instant` (and say so).

/// A calibrated clock that turns op latencies into nanoseconds.
pub struct Clock {
    cycles_per_ns: f64,
    overhead_cycles: u64,
    backend: &'static str,
}

impl Clock {
    pub fn backend(&self) -> &'static str {
        self.backend
    }
    pub fn cycles_per_ns(&self) -> f64 {
        self.cycles_per_ns
    }
    pub fn overhead_cycles(&self) -> u64 {
        self.overhead_cycles
    }

    /// Measure `op(i)` for `i in 0..iters`, returning per-op nanoseconds **sorted ascending**
    /// (ready for percentile extraction). Overhead is subtracted; results floor at 0.
    pub fn time_each(&self, iters: usize, op: impl FnMut(usize) -> u64) -> Vec<f64> {
        #[cfg(target_arch = "x86_64")]
        {
            self.time_each_tsc(iters, op)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            self.time_each_instant(iters, op)
        }
    }

    /// Time a single op in nanoseconds, overhead subtracted. Unlike `time_each` this preserves
    /// call order (needed to see adaptive structures converge over successive queries).
    pub fn measure_ns(&self, op: impl FnOnce() -> u64) -> f64 {
        #[cfg(target_arch = "x86_64")]
        {
            self.measure_ns_tsc(op)
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            self.measure_ns_instant(op)
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod tsc {
    use super::Clock;
    use core::arch::x86_64::{_mm_lfence, _rdtsc};
    use std::time::{Duration, Instant};

    /// Serialized timestamp read (fences stop the CPU reordering the counter read across the op).
    #[inline]
    unsafe fn rd() -> u64 {
        _mm_lfence();
        let t = _rdtsc();
        _mm_lfence();
        t
    }

    impl Clock {
        pub fn new() -> Self {
            // Frequency: count cycles across a fixed wall-clock window.
            let t0 = Instant::now();
            let c0 = unsafe { rd() };
            while t0.elapsed() < Duration::from_millis(300) {
                std::hint::spin_loop();
            }
            let c1 = unsafe { rd() };
            let elapsed_ns = t0.elapsed().as_nanos() as f64;
            let cycles_per_ns = (c1.wrapping_sub(c0)) as f64 / elapsed_ns;

            // Overhead: minimum cycles between two back-to-back reads (the cost of measuring).
            let mut overhead = u64::MAX;
            for _ in 0..2000 {
                let a = unsafe { rd() };
                let b = unsafe { rd() };
                overhead = overhead.min(b.wrapping_sub(a));
            }

            Clock { cycles_per_ns, overhead_cycles: overhead, backend: "rdtsc" }
        }

        pub(super) fn time_each_tsc(
            &self,
            iters: usize,
            mut op: impl FnMut(usize) -> u64,
        ) -> Vec<f64> {
            let mut out = Vec::with_capacity(iters);
            for i in 0..iters {
                let s = unsafe { rd() };
                std::hint::black_box(op(i));
                let e = unsafe { rd() };
                let raw = e.wrapping_sub(s);
                let net = raw.saturating_sub(self.overhead_cycles);
                out.push(net as f64 / self.cycles_per_ns);
            }
            out.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
            out
        }

        pub(super) fn measure_ns_tsc(&self, op: impl FnOnce() -> u64) -> f64 {
            let s = unsafe { rd() };
            std::hint::black_box(op());
            let e = unsafe { rd() };
            e.wrapping_sub(s).saturating_sub(self.overhead_cycles) as f64 / self.cycles_per_ns
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
mod portable {
    use super::Clock;
    use std::time::Instant;

    impl Clock {
        pub fn new() -> Self {
            Clock { cycles_per_ns: 1.0, overhead_cycles: 0, backend: "instant(coarse)" }
        }

        pub(super) fn time_each_instant(
            &self,
            iters: usize,
            mut op: impl FnMut(usize) -> u64,
        ) -> Vec<f64> {
            let mut out = Vec::with_capacity(iters);
            for i in 0..iters {
                let t = Instant::now();
                std::hint::black_box(op(i));
                out.push(t.elapsed().as_nanos() as f64);
            }
            out.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
            out
        }

        pub(super) fn measure_ns_instant(&self, op: impl FnOnce() -> u64) -> f64 {
            let t = Instant::now();
            std::hint::black_box(op());
            t.elapsed().as_nanos() as f64
        }
    }
}

/// Percentile from a sorted slice (nearest-rank).
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * (sorted.len() - 1) as f64).round() as usize;
    sorted[rank]
}
