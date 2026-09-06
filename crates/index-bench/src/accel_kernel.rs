//! p33-accel-kernel :: are the analytics kernels correct, and how fast?
//!
//! `index-accel` implements onegrid's ratified `AccelModule` ABI — seven kernels covering sort,
//! filter, group, aggregate, bitmap and top-k. It is the **analytics half** of this project's goal,
//! and until now it had 3 unit tests, no bench, and no roadmap document, against 111 tests and 33
//! documents for the text engine. That asymmetry is the reason this bin exists.
//!
//! Every kernel is checked **differentially against an independent reference written here**, over
//! randomised inputs that deliberately include the cases that break analytics code: missing values,
//! NaN, `-0.0`, duplicate keys, empty selections, and `k` larger than the input.
//!
//! The reference implementations are written from the ABI's documented semantics, not by reading
//! the kernel. Where they disagree, this bin reports it rather than deciding who is right --
//! `p11` recorded what happens when two implementations share a helper: they agree about the
//! helper, not about the answer.

use index_accel::*;

mod timer;

/// Deterministic PRNG — a bench that cannot be replayed cannot be debugged.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn value(&mut self) -> f64 {
        // Small integers dominate so duplicates and ties are common, which is where group-by and
        // stable sort actually go wrong. Specials are salted in on purpose.
        match self.next() % 32 {
            0 => f64::NAN,
            1 => -0.0,
            2 => 0.0,
            3 => f64::INFINITY,
            4 => f64::NEG_INFINITY,
            _ => (self.next() % 50) as f64 - 25.0,
        }
    }
}

/// A host address, as the ABI's `AccelPtr`.
///
/// This is the line that found the defect `p33` reports. It first asserted the address fit in a
/// `u32`, because the ABI is 32-bit -- and the assertion fired immediately: a 1 M-row buffer on
/// 64-bit Windows lands above 4 GiB, where `u32 as *const T` truncates to a different, valid-looking
/// address. `index-accel` now uses `AccelPtr`, which is `u32` on wasm32 (the ratified ABI, byte for
/// byte) and `usize` on the host.
fn ptr_of<T>(v: &[T]) -> index_accel::AccelPtr {
    v.as_ptr() as index_accel::AccelPtr
}

fn present_bit(bits: &[u8], i: usize) -> bool {
    bits[i >> 3] & (1u8 << (i & 7)) != 0
}

/// The missing-value model, restated from the ABI docs: a row is missing when its validity bit is
/// clear OR its value is NaN, and the host folds both into the bitmap it passes.
fn make_presence(value: &[f64], rng: &mut Rng) -> Vec<u8> {
    let mut bits = vec![0u8; value.len().div_ceil(8)];
    for (i, v) in value.iter().enumerate() {
        let valid = rng.next() % 8 != 0;
        if valid && !v.is_nan() {
            bits[i >> 3] |= 1u8 << (i & 7);
        }
    }
    bits
}

/// Total order used by sort and top-k: present values ascending, missing at one end.
fn cmp_key(value: &[f64], bits: &[u8], a: usize, b: usize, desc: bool, missing_first: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let (pa, pb) = (present_bit(bits, a), present_bit(bits, b));
    match (pa, pb) {
        (false, false) => Equal,
        (false, true) => {
            if missing_first {
                Less
            } else {
                Greater
            }
        }
        (true, false) => {
            if missing_first {
                Greater
            } else {
                Less
            }
        }
        (true, true) => {
            let (x, y) = (value[a], value[b]);
            let o = x.partial_cmp(&y).unwrap_or(Equal);
            if desc {
                o.reverse()
            } else {
                o
            }
        }
    }
}

fn main() {
    let clock = timer::Clock::new();
    println!("p33-accel-kernel :: the analytics half, checked against an independent reference");
    println!("clock backend: {}", clock.backend());
    println!("  ABI version {}\n", og_abi_version());

    let mut fail = 0usize;
    let mut rng = Rng(0x5EED_1234_ABCD_EF01);

    // ---- 1. bitmap ops -----------------------------------------------------------------------
    // The simplest kernel, and the one whose contract is easiest to get subtly wrong: bits at or
    // beyond `bit_length` in the final byte MUST be cleared, or two bitmaps of the same logical
    // length stop comparing byte-equal.
    let mut wrong = 0usize;
    for trial in 0..400 {
        let n = 1 + rng.below(200);
        let bytes = n.div_ceil(8);
        let a: Vec<u8> = (0..bytes).map(|_| rng.next() as u8).collect();
        let b: Vec<u8> = (0..bytes).map(|_| rng.next() as u8).collect();
        let mut out = vec![0u8; bytes];
        let op = (trial % 5) as u32;
        unsafe { og_bitmap_op(op, ptr_of(&a), ptr_of(&b), n as u32, ptr_of(&out)) };
        for i in 0..n {
            let (x, y) = (present_bit(&a, i), present_bit(&b, i));
            let want = match op {
                0 => x && y,
                1 => x || y,
                2 => !x,
                3 => x && !y,
                _ => x != y,
            };
            if present_bit(&out, i) != want {
                wrong += 1;
                break;
            }
        }
        // Tail bits must be zero.
        if n % 8 != 0 && out[bytes - 1] >> (n % 8) != 0 {
            wrong += 1;
        }
        let _ = &mut out;
    }
    println!("  --- bitmap_op ---");
    println!("  400 randomised trials over 5 ops, {wrong} wrong (tail bits checked)");
    fail += usize::from(wrong > 0);

    // ---- 2. filter_mask ----------------------------------------------------------------------
    let mut wrong = 0usize;
    for trial in 0..400 {
        let n = 1 + rng.below(300);
        let value: Vec<f64> = (0..n).map(|_| rng.value()).collect();
        let bits = make_presence(&value, &mut rng);
        let set: Vec<f64> = (0..4).map(|_| (rng.next() % 50) as f64 - 25.0).collect();
        let operand = (rng.next() % 50) as f64 - 25.0;
        let upper = operand + (rng.next() % 10) as f64;
        let op = (trial % 12) as u32;
        let out = vec![0u8; n.div_ceil(8)];
        unsafe {
            og_filter_mask(
                ptr_of(&value),
                ptr_of(&bits),
                n as u32,
                op,
                operand,
                upper,
                ptr_of(&set),
                set.len() as u32,
                ptr_of(&out),
            )
        };
        for (i, &v) in value.iter().enumerate().take(n) {
            let p = present_bit(&bits, i);
            let want = match op {
                10 => !p,
                11 => p,
                _ if !p => false,
                0 => v == operand,
                1 => v != operand,
                2 => v < operand,
                3 => v <= operand,
                4 => v > operand,
                5 => v >= operand,
                6 => v >= operand && v <= upper,
                7 => !(v >= operand && v <= upper),
                8 => set.contains(&v),
                _ => !set.contains(&v),
            };
            if present_bit(&out, i) != want {
                wrong += 1;
                break;
            }
        }
    }
    println!("\n  --- filter_mask ---");
    println!("  400 randomised trials over 12 ops, {wrong} wrong");
    fail += usize::from(wrong > 0);

    // ---- 3. sort_pass ------------------------------------------------------------------------
    // Must be STABLE and must be a permutation. Both are checked: a sort that loses or duplicates
    // an index can still look ordered.
    let mut wrong = 0usize;
    for trial in 0..300 {
        let n = 1 + rng.below(300);
        let value: Vec<f64> = (0..n).map(|_| rng.value()).collect();
        let bits = make_presence(&value, &mut rng);
        let desc = trial % 2 == 0;
        let missing_first = trial % 3 == 0;
        let perm: Vec<i32> = (0..n as i32).collect();
        let scratch = vec![0i32; n];
        unsafe {
            og_sort_pass(
                ptr_of(&value),
                ptr_of(&bits),
                n as u32,
                u32::from(desc),
                u32::from(missing_first),
                ptr_of(&perm),
                ptr_of(&scratch),
            )
        };
        let mut want: Vec<i32> = (0..n as i32).collect();
        want.sort_by(|&a, &b| {
            cmp_key(&value, &bits, a as usize, b as usize, desc, missing_first).then(a.cmp(&b))
        });
        if perm != want {
            wrong += 1;
            if wrong <= 2 {
                println!("    SORT mismatch n={n} desc={desc} missing_first={missing_first}");
            }
            continue;
        }
        let mut seen = vec![false; n];
        for &p in &perm {
            if p < 0 || p as usize >= n || seen[p as usize] {
                wrong += 1;
                break;
            }
            seen[p as usize] = true;
        }
    }
    println!("\n  --- sort_pass ---");
    println!("  300 randomised trials, {wrong} wrong (stability and permutation both checked)");
    fail += usize::from(wrong > 0);

    // ---- 4. top_k ----------------------------------------------------------------------------
    let mut wrong = 0usize;
    for trial in 0..300 {
        let n = 1 + rng.below(200);
        let value: Vec<f64> = (0..n).map(|_| rng.value()).collect();
        let bits = make_presence(&value, &mut rng);
        let k = rng.below(n + 5);
        let desc = trial % 2 == 0;
        let missing_first = trial % 3 == 0;
        let out = vec![0i32; n + 5];
        let scratch = vec![0i32; n + 5];
        let got = unsafe {
            og_top_k(
                ptr_of(&value),
                ptr_of(&bits),
                n as u32,
                k as u32,
                u32::from(desc),
                u32::from(missing_first),
                ptr_of(&out),
                ptr_of(&scratch),
            )
        } as usize;
        let mut want: Vec<i32> = (0..n as i32).collect();
        want.sort_by(|&a, &b| {
            cmp_key(&value, &bits, a as usize, b as usize, desc, missing_first).then(a.cmp(&b))
        });
        want.truncate(k.min(n));
        if got != want.len() || out[..got] != want[..] {
            wrong += 1;
            if wrong <= 2 {
                println!("    TOPK mismatch n={n} k={k} desc={desc}: got {got}, want {}", want.len());
            }
        }
    }
    println!("\n  --- top_k ---");
    println!("  300 randomised trials including k > n, {wrong} wrong");
    fail += usize::from(wrong > 0);

    // ---- 5. group_code -----------------------------------------------------------------------
    // Codes are assigned in first-appearance order; equal keys must share a code, and -0.0 must
    // collide with 0.0 or a group-by splits a bucket a user sees as one.
    let mut wrong = 0usize;
    for _ in 0..300 {
        let n = 1 + rng.below(200);
        let value: Vec<f64> = (0..n).map(|_| rng.value()).collect();
        let bits = make_presence(&value, &mut rng);
        let cap = 256usize;
        let out = vec![0i32; n];
        let slot_key = vec![0f64; cap];
        let slot_code = vec![0i32; cap];
        let groups = unsafe {
            og_group_code(
                ptr_of(&value),
                ptr_of(&bits),
                n as u32,
                ptr_of(&out),
                ptr_of(&slot_key),
                ptr_of(&slot_code),
                cap as u32,
            )
        };
        // Reference partition. The comparison below is numbering-agnostic, so this only has to
        // get "which rows share a group" right: one group per distinct normalised key, plus one
        // group for every missing row. `-0.0` normalises to `0.0` or a group-by would split a
        // bucket the user sees as one.
        //
        // An earlier version of this reference gave a REPEATED key the index into its key list
        // rather than the code that key was first assigned, which is a different partition whenever
        // a missing row appears before a repeat. It reported 266 of 300 trials wrong -- against a
        // kernel that was right. The lesson is `p11`'s in a new costume: the oracle needs checking
        // as hard as the thing it checks.
        let mut code: Vec<i32> = vec![-1; n];
        let mut key_of: Vec<f64> = Vec::new();
        const MISSING: i32 = i32::MIN;
        for i in 0..n {
            if !present_bit(&bits, i) {
                code[i] = MISSING;
                continue;
            }
            let k = if value[i] == 0.0 { 0.0 } else { value[i] };
            code[i] = match key_of.iter().position(|&x| x == k) {
                Some(j) => j as i32,
                None => {
                    key_of.push(k);
                    (key_of.len() - 1) as i32
                }
            };
        }
        // Compare as a PARTITION, not as literal codes: the kernel and the reference may number
        // groups differently, and only the grouping is contractual.
        let mut map: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
        let mut ok = groups >= 0;
        if ok {
            for i in 0..n {
                let g = out[i];
                match map.entry(g) {
                    std::collections::hash_map::Entry::Occupied(e) => {
                        if *e.get() != code[i] {
                            ok = false;
                            break;
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(code[i]);
                    }
                }
            }
            // And injectively: two kernel groups must not fold into one reference group.
            let mut inv: std::collections::HashSet<i32> = std::collections::HashSet::new();
            for v in map.values() {
                if !inv.insert(*v) {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            wrong += 1;
        }
    }
    println!("\n  --- group_code ---");
    println!("  300 randomised trials, {wrong} wrong (compared as a partition, both directions)");
    fail += usize::from(wrong > 0);

    // ---- 6. group_combine ---------------------------------------------------------------------
    // The composite key of a two-column group-by. Codes are assigned in first-appearance order over
    // the PAIR, so `(a, b)` and `(b, a)` are different groups and equal pairs share a code.
    //
    // Fed deliberately from `og_group_code` output rather than from synthetic integers, because
    // that is the only way it is ever called -- and it is where a wrong assumption about code
    // ranges or negative codes would show up.
    let mut wrong = 0usize;
    let mut saturated = 0usize;
    for _ in 0..300 {
        let n = 1 + rng.below(200);
        let cap = 256usize;

        // Two independent code columns, produced by the kernel under the same conditions a host
        // would produce them.
        let mut col = Vec::new();
        for _ in 0..2 {
            let value: Vec<f64> = (0..n).map(|_| rng.value()).collect();
            let bits = make_presence(&value, &mut rng);
            let code = vec![0i32; n];
            let slot_key = vec![0f64; cap];
            let slot_code = vec![0i32; cap];
            let groups = unsafe {
                og_group_code(
                    ptr_of(&value),
                    ptr_of(&bits),
                    n as u32,
                    ptr_of(&code),
                    ptr_of(&slot_key),
                    ptr_of(&slot_code),
                    cap as u32,
                )
            };
            if groups < 0 {
                saturated += 1;
            }
            col.push(code);
        }
        let (a, b) = (&col[0], &col[1]);

        let out = vec![0i32; n];
        let slot_a = vec![0i32; cap];
        let slot_b = vec![0i32; cap];
        let slot_code = vec![0i32; cap];
        let groups = unsafe {
            og_group_combine(
                ptr_of(a),
                ptr_of(b),
                n as u32,
                ptr_of(&out),
                ptr_of(&slot_a),
                ptr_of(&slot_b),
                ptr_of(&slot_code),
                cap as u32,
            )
        };
        if groups < 0 {
            // Table saturation is a legitimate, signalled outcome -- not a wrong answer.
            saturated += 1;
            continue;
        }

        // Reference: first-appearance codes over the pair, compared as a partition in both
        // directions so numbering differences pass and a fold or a split fails.
        let mut want: Vec<i32> = vec![-1; n];
        let mut seen_pair: Vec<(i32, i32)> = Vec::new();
        for i in 0..n {
            let p = (a[i], b[i]);
            want[i] = match seen_pair.iter().position(|&q| q == p) {
                Some(j) => j as i32,
                None => {
                    seen_pair.push(p);
                    (seen_pair.len() - 1) as i32
                }
            };
        }
        if groups as usize != seen_pair.len() {
            wrong += 1;
            if wrong <= 2 {
                println!("    COMBINE group count: got {groups}, want {}", seen_pair.len());
            }
            continue;
        }
        let mut fwd: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
        let mut rev: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
        let mut ok = true;
        for i in 0..n {
            if *fwd.entry(out[i]).or_insert(want[i]) != want[i]
                || *rev.entry(want[i]).or_insert(out[i]) != out[i]
            {
                ok = false;
                break;
            }
        }
        if !ok {
            wrong += 1;
        }
    }
    // Deliberate saturation. The random trials above never filled the table (cap 256, n <= 200),
    // so the `-1` path -- "this table cannot hold the answer" -- was untested. A hash table that
    // silently wrapped instead of signalling would CORRUPT a group-by rather than fail it, which is
    // the worst outcome available to an analytics kernel: a wrong number that looks like a number.
    let mut saturation_ok = 0usize;
    for cap in [8usize, 16, 32] {
        let n = cap * 4;
        let a: Vec<i32> = (0..n as i32).collect();
        let b: Vec<i32> = (0..n as i32).collect();
        let out = vec![0i32; n];
        let slot_a = vec![0i32; cap];
        let slot_b = vec![0i32; cap];
        let slot_code = vec![0i32; cap];
        let r = unsafe {
            og_group_combine(
                ptr_of(&a),
                ptr_of(&b),
                n as u32,
                ptr_of(&out),
                ptr_of(&slot_a),
                ptr_of(&slot_b),
                ptr_of(&slot_code),
                cap as u32,
            )
        };
        // `n` distinct pairs into a `cap`-slot table must refuse, not wrap.
        if r < 0 {
            saturation_ok += 1;
        }
    }
    if saturation_ok != 3 {
        println!("    SATURATION: {saturation_ok}/3 refused; a full table must return -1, not wrap");
        fail += 1;
    }

    println!("\n  --- group_combine ---");
    println!("  300 randomised trials over real group_code output, {wrong} wrong ({saturated} saturated)");
    println!("  {saturation_ok}/3 deliberately over-filled tables returned -1 rather than wrapping");
    fail += usize::from(wrong > 0);

    // ---- 7. aggregate ------------------------------------------------------------------------
    let mut wrong = 0usize;
    for trial in 0..400 {
        let n = 1 + rng.below(300);
        let value: Vec<f64> = (0..n).map(|_| rng.value()).collect();
        let bits = make_presence(&value, &mut rng);
        let op = (trial % 8) as u32;
        let cap = 512usize;
        let slot_key = vec![0f64; cap];
        let slot_state = vec![0i32; cap];
        let out = vec![0f64; 1];
        let written = unsafe {
            og_aggregate(
                op,
                ptr_of(&value),
                ptr_of(&bits),
                n as u32,
                0,
                -1,
                ptr_of(&slot_key),
                ptr_of(&slot_state),
                cap as u32,
                ptr_of(&out),
            )
        };
        let live: Vec<f64> =
            (0..n).filter(|&i| present_bit(&bits, i)).map(|i| value[i]).collect();
        let (want_written, want) = match op {
            0 => (1u32, live.iter().sum::<f64>()),
            1 => {
                if live.is_empty() {
                    (0, 0.0)
                } else {
                    (1, live.iter().sum::<f64>() / live.len() as f64)
                }
            }
            2 => (1, live.len() as f64),
            3 => {
                let mut d: Vec<f64> = live.iter().map(|&v| if v == 0.0 { 0.0 } else { v }).collect();
                d.sort_by(|a, b| a.partial_cmp(b).unwrap());
                d.dedup();
                (1, d.len() as f64)
            }
            4 => match live.iter().cloned().fold(None::<f64>, |m, v| Some(m.map_or(v, |m| m.min(v)))) {
                Some(v) => (1, v),
                None => (0, 0.0),
            },
            5 => match live.iter().cloned().fold(None::<f64>, |m, v| Some(m.map_or(v, |m| m.max(v)))) {
                Some(v) => (1, v),
                None => (0, 0.0),
            },
            6 => match live.first() {
                Some(&v) => (1, v),
                None => (0, 0.0),
            },
            _ => match live.last() {
                Some(&v) => (1, v),
                None => (0, 0.0),
            },
        };
        let got = out[0];
        // `sum` over a column containing both +inf and -inf is NaN, legitimately and in both
        // implementations -- but `NaN == NaN` is false, so a naive comparison reports every such
        // trial as a mismatch. The first run of this bin did exactly that: 86 "failures" whose own
        // diagnostic line read `got NaN, want NaN`.
        let same = |a: f64, b: f64| {
            (a.is_nan() && b.is_nan())
                || a == b
                || (a - b).abs() <= 1e-9 * b.abs().max(1.0)
        };
        let agree = written == want_written && (written == 0 || same(got, want));
        if !agree {
            wrong += 1;
            if wrong <= 3 {
                println!("    AGG op={op} n={n}: got {got} (written {written}), want {want} ({want_written})");
            }
        }
    }
    println!("\n  --- aggregate ---");
    println!("  400 randomised trials over 8 ops, {wrong} wrong");
    fail += usize::from(wrong > 0);

    // ---- 8. throughput -----------------------------------------------------------------------
    // One million rows, the scale the rest of this project is measured at.
    const N: usize = 1_000_000;
    let value: Vec<f64> = (0..N).map(|_| rng.value()).collect();
    let bits = make_presence(&value, &mut rng);
    let mask = vec![0u8; N.div_ceil(8)];
    let perm: Vec<i32> = (0..N as i32).collect();
    let scratch = vec![0i32; N];
    let out_i = vec![0i32; N];
    let slot_key = vec![0f64; 4096];
    let slot_state = vec![0i32; 4096];
    let slot_code = vec![0i32; 4096];
    let out_f = vec![0f64; 1];

    let ms = |ns: f64| ns / 1e6;
    let filter_ns = clock.measure_ns(|| {
        unsafe {
            og_filter_mask(ptr_of(&value), ptr_of(&bits), N as u32, 4, 0.0, 0.0, 0, 0, ptr_of(&mask))
        };
        0
    });
    let agg_ns = clock.measure_ns(|| {
        unsafe {
            og_aggregate(
                0,
                ptr_of(&value),
                ptr_of(&bits),
                N as u32,
                0,
                -1,
                ptr_of(&slot_key),
                ptr_of(&slot_state),
                4096,
                ptr_of(&out_f),
            )
        };
        0
    });
    let group_ns = clock.measure_ns(|| {
        unsafe {
            og_group_code(
                ptr_of(&value),
                ptr_of(&bits),
                N as u32,
                ptr_of(&out_i),
                ptr_of(&slot_key),
                ptr_of(&slot_code),
                4096,
            )
        };
        0
    });
    let topk_ns = clock.measure_ns(|| {
        unsafe {
            og_top_k(
                ptr_of(&value),
                ptr_of(&bits),
                N as u32,
                100,
                1,
                0,
                ptr_of(&out_i),
                ptr_of(&scratch),
            )
        };
        0
    });
    let sort_ns = clock.measure_ns(|| {
        unsafe {
            og_sort_pass(
                ptr_of(&value),
                ptr_of(&bits),
                N as u32,
                0,
                0,
                ptr_of(&perm),
                ptr_of(&scratch),
            )
        };
        0
    });

    // `bitmap_op` touches N/8 bytes, not N values, and vectorises. A single shot measured 0.00 ms
    // and printed 215,519 M rows/s -- a number below the timer's resolution, which is noise wearing
    // a result's clothes. Averaged over `REP` runs it is a measurement.
    const REP: usize = 200;
    let bitmap_ns = clock.measure_ns(|| {
        for _ in 0..REP {
            unsafe { og_bitmap_op(0, ptr_of(&bits), ptr_of(&mask), N as u32, ptr_of(&mask)) };
        }
        0
    }) / REP as f64;

    println!("\n  --- throughput at {} rows ---", N);
    println!("  {:<14} {:>10} {:>16}", "kernel", "ms", "M rows/s");
    for (name, ns) in [
        ("filter_mask", filter_ns),
        ("aggregate", agg_ns),
        ("group_code", group_ns),
        ("top_k(100)", topk_ns),
        ("sort_pass", sort_ns),
        ("bitmap_op", bitmap_ns),
    ] {
        println!("  {:<14} {:>10.3} {:>16.1}", name, ms(ns), N as f64 / ns * 1e3);
    }

    println!("\nOVERALL: {}", if fail == 0 { "PASS" } else { "FAIL" });
    std::process::exit(i32::from(fail != 0));
}
