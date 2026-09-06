//! `index-accel` — onegrid's `AccelModule` ABI, implemented.
//!
//! `docs/research/demand.md` Finding 4: onegrid ratified this ABI, wrote the JavaScript reference
//! implementation, wrote the differential harness that proves an accelerated backend identical to
//! it, and shipped **no module** — `packages/wasm/crate/` does not exist and the tests run against
//! `createFakeAccelModule()`. **The socket was cut and empty.** This fills it.
//!
//! The JavaScript in `packages/wasm/src/fake-module.ts` is the specification, and this file is a
//! faithful port of it. Where the two could differ, the JavaScript wins.
//!
//! # The contract, and why this crate is `no_std`
//!
//! **The JavaScript host owns the heap.** `og_heap_base()` reports the first byte above this
//! module's static data, and the host bump-allocates everything above it. Every pointer parameter
//! is an absolute byte offset into linear memory, chosen by the host. So this crate must never
//! allocate — a Rust allocator running in the same linear memory would hand out addresses the host
//! believes it owns. `no_std` makes that structural rather than a promise.
//!
//! # The missing-value model
//!
//! A row is missing when its validity bit is clear **or** its value is NaN. The host folds both
//! into the packed presence bitmap it passes in, so this side reads bits only. onegrid's rationale
//! is worth restating because it is what makes a differential harness possible at all: a comparator
//! returning 0 for every NaN pair is not a total order, and a sort built on a non-total order may
//! legitimately produce different permutations in different implementations — so JS and Rust could
//! disagree while both being correct. Declaring NaN missing restores a total order and obliges the
//! two to agree element for element.

//! # Building
//!
//! `no_std` **on wasm32 only**. On the host the crate compiles against `std` so the kernels can be
//! exercised natively against buffers the caller allocates — see [`AccelPtr`] for the one thing
//! that differs.
//!
//! **A host run is not the same run the browser makes**, and the earlier wording here said it was.
//! The kernel bodies are identical, but pointers are 64-bit natively and 32-bit in wasm, so a
//! defect that depends on 32-bit wraparound cannot surface on the host. Native coverage
//! (`cargo run -p index-bench --release --bin accel-kernel`) is additional to, not a substitute
//! for, the consumer's own differential harness, which is what proves the shipped artifact.
//! `docs/integration.md` and `bench/roadmap/p33-accel-kernel.md` record both.
//!
//! ```sh
//! cargo build -p index-accel --release --target wasm32-unknown-unknown
//! cargo test  -p index-accel
//! ```

#![cfg_attr(target_arch = "wasm32", no_std)]

/// Aborts. There is no unwinding in a `no_std` cdylib, and a kernel that has read past its buffer
/// has already lost — trapping is the honest outcome.
#[cfg(target_arch = "wasm32")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

/// The pointer type the ABI passes.
///
/// **`u32` on wasm32 — the ratified `AccelModule` signature, unchanged.** Linear memory is 32-bit
/// there, so a host-chosen byte offset is exactly a `u32` and the cast to a pointer is exact.
///
/// **`usize` on the host**, where this crate is built only so the kernels can be unit-tested
/// natively. A 64-bit host allocates above 4 GiB routinely, and `u32 as *const T` would truncate
/// such an address into a different, valid-looking one — silent corruption rather than a crash.
/// The existing host tests passed only because their buffers were small enough to land low;
/// `bench/roadmap/p33-accel-kernel.md` records how a 1 M-row bench found it.
#[cfg(target_arch = "wasm32")]
pub type AccelPtr = u32;
/// See [`AccelPtr`].
#[cfg(not(target_arch = "wasm32"))]
pub type AccelPtr = usize;

/// Must equal `ACCEL_ABI_VERSION` in `packages/wasm/src/abi.ts`. The host checks it at bind time so
/// a stale binary fails loudly instead of silently computing `lt` where the caller asked for `gt`.
const ABI_VERSION: u32 = 1;

#[cfg(target_arch = "wasm32")]
extern "C" {
    /// Provided by `wasm-ld`: the first byte above all static data.
    static __heap_base: u8;
}

#[no_mangle]
pub extern "C" fn og_abi_version() -> u32 {
    ABI_VERSION
}

/// First byte the host's bump allocator may use.
#[no_mangle]
pub extern "C" fn og_heap_base() -> u32 {
    #[cfg(target_arch = "wasm32")]
    {
        // SAFETY: taking the address of a linker-provided symbol; never dereferenced.
        core::ptr::addr_of!(__heap_base) as u32
    }
    // On the host there is no linear memory to partition; the host tests own their own buffers.
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

// ---------------------------------------------------------------------------------------------
// Raw linear-memory access. On wasm32 a pointer *is* a byte offset, so the host's integers can be
// used directly. Every accessor is `#[inline(always)]` because these are the innermost operations
// in every kernel.
// ---------------------------------------------------------------------------------------------

#[inline(always)]
unsafe fn f64_at(ptr: AccelPtr, i: usize) -> f64 {
    *((ptr as *const f64).add(i))
}
#[inline(always)]
unsafe fn set_f64(ptr: AccelPtr, i: usize, v: f64) {
    *((ptr as *mut f64).add(i)) = v;
}
#[inline(always)]
unsafe fn i32_at(ptr: AccelPtr, i: usize) -> i32 {
    *((ptr as *const i32).add(i))
}
#[inline(always)]
unsafe fn set_i32(ptr: AccelPtr, i: usize, v: i32) {
    *((ptr as *mut i32).add(i)) = v;
}
#[inline(always)]
unsafe fn u8_at(ptr: AccelPtr, i: usize) -> u8 {
    *((ptr as *const u8).add(i))
}
#[inline(always)]
unsafe fn set_u8(ptr: AccelPtr, i: usize, v: u8) {
    *((ptr as *mut u8).add(i)) = v;
}

/// Bit `i` of a packed, LSB-first bitmap — the layout `bit.ts` writes.
#[inline(always)]
unsafe fn is_present(presence: AccelPtr, i: usize) -> bool {
    (u8_at(presence, i >> 3) & (1u8 << (i & 7))) != 0
}

#[inline(always)]
fn byte_length_for(bits: usize) -> usize {
    bits.div_ceil(8)
}

/// Normalize `-0.0` to `0.0` so it hashes and matches identically to `0.0`.
#[inline(always)]
fn norm(v: f64) -> f64 {
    if v == 0.0 {
        0.0
    } else {
        v
    }
}

/// Key order for one sort level. Returns negative, zero or positive.
///
/// Missing rows compare equal to each other and sort to one end; this never inspects a value for a
/// missing row, which is what lets NaN payloads be arbitrary.
#[inline(always)]
unsafe fn key_compare(
    value: AccelPtr,
    presence: AccelPtr,
    a: usize,
    b: usize,
    descending: u32,
    missing_first: u32,
) -> i32 {
    let pa = is_present(presence, a);
    let pb = is_present(presence, b);
    if !pa || !pb {
        if pa == pb {
            return 0;
        }
        if !pa {
            return if missing_first != 0 { -1 } else { 1 };
        }
        return if missing_first != 0 { 1 } else { -1 };
    }
    let va = f64_at(value, a);
    let vb = f64_at(value, b);
    let d = if va < vb {
        -1
    } else if va > vb {
        1
    } else {
        0
    };
    if descending != 0 {
        -d
    } else {
        d
    }
}

/// Key order plus the index tiebreak — the total order `top_k` needs to emit a final ordering.
#[inline(always)]
unsafe fn total_compare(
    value: AccelPtr,
    presence: AccelPtr,
    a: usize,
    b: usize,
    descending: u32,
    missing_first: u32,
) -> i32 {
    let d = key_compare(value, presence, a, b, descending, missing_first);
    if d != 0 {
        d
    } else {
        (a as i64 - b as i64).signum() as i32
    }
}

/// A hash for an `f64` key. Any function that maps equal keys to equal slots is correct here:
/// group codes are assigned in **first-appearance order**, not slot order, so the emitted codes do
/// not depend on the hash at all. Only the probe count — and therefore the table-full return — can
/// differ, which is why the host sizes tables at load factor 0.5.
#[inline(always)]
fn hash_f64(v: f64) -> usize {
    let bits = v.to_bits();
    let mut z = bits ^ (bits >> 32);
    z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z ^= z >> 29;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 32;
    z as usize
}

#[inline(always)]
fn hash_pair(a: i32, b: i32) -> usize {
    let k = ((a as u32 as u64) << 32) | (b as u32 as u64);
    let mut z = k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z ^= z >> 29;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 32;
    z as usize
}

// ---------------------------------------------------------------------------------------------
// Kernels
// ---------------------------------------------------------------------------------------------

/// One stable sort pass, refining an existing permutation in place.
///
/// Bottom-up merge sort, stable by construction: on a tie the merge takes from the left run, which
/// is the run that came first. Stability is what makes a multi-level sort work by running this once
/// per level from the least significant key upward.
///
/// # Safety
/// All pointers must address host-owned buffers of the documented length.
#[no_mangle]
pub unsafe extern "C" fn og_sort_pass(
    value_ptr: AccelPtr,
    presence_ptr: AccelPtr,
    length: u32,
    descending: u32,
    missing_first: u32,
    perm_ptr: AccelPtr,
    scratch_ptr: AccelPtr,
) {
    let n = length as usize;
    if n < 2 {
        return;
    }
    let mut width = 1usize;
    while width < n {
        let mut lo = 0usize;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut a, mut b, mut out) = (lo, mid, lo);
            while a < mid && b < hi {
                let ia = i32_at(perm_ptr, a) as usize;
                let ib = i32_at(perm_ptr, b) as usize;
                if key_compare(value_ptr, presence_ptr, ib, ia, descending, missing_first) < 0 {
                    set_i32(scratch_ptr, out, ib as i32);
                    b += 1;
                } else {
                    set_i32(scratch_ptr, out, ia as i32);
                    a += 1;
                }
                out += 1;
            }
            while a < mid {
                set_i32(scratch_ptr, out, i32_at(perm_ptr, a));
                a += 1;
                out += 1;
            }
            while b < hi {
                set_i32(scratch_ptr, out, i32_at(perm_ptr, b));
                b += 1;
                out += 1;
            }
            lo += 2 * width;
        }
        for i in 0..n {
            set_i32(perm_ptr, i, i32_at(scratch_ptr, i));
        }
        width *= 2;
    }
}

/// Evaluate a predicate over a column, writing a packed bitmask.
///
/// Missing rows never satisfy a comparison — only `isNull` (10) and `isNotNull` (11) observe them.
///
/// # Safety
/// All pointers must address host-owned buffers of the documented length.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn og_filter_mask(
    value_ptr: AccelPtr,
    presence_ptr: AccelPtr,
    length: u32,
    op: u32,
    operand: f64,
    upper: f64,
    set_ptr: AccelPtr,
    set_length: u32,
    out_ptr: AccelPtr,
) {
    let n = length as usize;
    for i in 0..byte_length_for(n) {
        set_u8(out_ptr, i, 0);
    }
    for i in 0..n {
        let present = is_present(presence_ptr, i);
        let hit = if op == 10 {
            !present
        } else if op == 11 {
            present
        } else if present {
            let v = f64_at(value_ptr, i);
            match op {
                0 => v == operand,
                1 => v != operand,
                2 => v < operand,
                3 => v <= operand,
                4 => v > operand,
                5 => v >= operand,
                6 | 7 => {
                    let in_range = v >= operand && v <= upper;
                    if op == 6 {
                        in_range
                    } else {
                        !in_range
                    }
                }
                _ => {
                    let mut found = false;
                    for s in 0..set_length as usize {
                        if f64_at(set_ptr, s) == v {
                            found = true;
                            break;
                        }
                    }
                    if op == 8 {
                        found
                    } else {
                        !found
                    }
                }
            }
        } else {
            false
        };
        if hit {
            let byte = i >> 3;
            set_u8(out_ptr, byte, u8_at(out_ptr, byte) | (1u8 << (i & 7)));
        }
    }
}

/// Factorise one column into dense codes. Returns the cardinality, or `-1` if the table filled.
///
/// All missing rows share one code, allocated at their first appearance, which mirrors how
/// `@onegrid/data` groups nulls together.
///
/// # Safety
/// All pointers must address host-owned buffers of the documented length; `slot_capacity` must be
/// a power of two.
#[no_mangle]
pub unsafe extern "C" fn og_group_code(
    value_ptr: AccelPtr,
    presence_ptr: AccelPtr,
    length: u32,
    out_ptr: AccelPtr,
    slot_key_ptr: AccelPtr,
    slot_code_ptr: AccelPtr,
    slot_capacity: u32,
) -> i32 {
    let cap = slot_capacity as usize;
    // The kernel owns table initialisation — the host supplies bytes, not state.
    for s in 0..cap {
        set_i32(slot_code_ptr, s, -1);
    }
    let step = cap - 1;
    let mut next = 0i32;
    let mut missing_code = -1i32;
    for i in 0..length as usize {
        if !is_present(presence_ptr, i) {
            if missing_code < 0 {
                missing_code = next;
                next += 1;
            }
            set_i32(out_ptr, i, missing_code);
            continue;
        }
        let v = norm(f64_at(value_ptr, i));
        let mut slot = hash_f64(v) & step;
        let mut probe = 0usize;
        loop {
            if probe >= cap {
                return -1;
            }
            let existing = i32_at(slot_code_ptr, slot);
            if existing < 0 {
                set_f64(slot_key_ptr, slot, v);
                set_i32(slot_code_ptr, slot, next);
                set_i32(out_ptr, i, next);
                next += 1;
                break;
            }
            if f64_at(slot_key_ptr, slot) == v {
                set_i32(out_ptr, i, existing);
                break;
            }
            slot = (slot + 1) & step;
            probe += 1;
        }
    }
    next
}

/// Renumber `(a[i], b[i])` code pairs into dense codes. Returns the cardinality, or `-1`.
///
/// # Safety
/// All pointers must address host-owned buffers; `slot_capacity` must be a power of two.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn og_group_combine(
    a_ptr: AccelPtr,
    b_ptr: AccelPtr,
    length: u32,
    out_ptr: AccelPtr,
    slot_a_ptr: AccelPtr,
    slot_b_ptr: AccelPtr,
    slot_code_ptr: AccelPtr,
    slot_capacity: u32,
) -> i32 {
    let cap = slot_capacity as usize;
    for s in 0..cap {
        set_i32(slot_code_ptr, s, -1);
    }
    let step = cap - 1;
    let mut next = 0i32;
    for i in 0..length as usize {
        let a = i32_at(a_ptr, i);
        let b = i32_at(b_ptr, i);
        let mut slot = hash_pair(a, b) & step;
        let mut probe = 0usize;
        loop {
            if probe >= cap {
                return -1;
            }
            let existing = i32_at(slot_code_ptr, slot);
            if existing < 0 {
                set_i32(slot_a_ptr, slot, a);
                set_i32(slot_b_ptr, slot, b);
                set_i32(slot_code_ptr, slot, next);
                set_i32(out_ptr, i, next);
                next += 1;
                break;
            }
            if i32_at(slot_a_ptr, slot) == a && i32_at(slot_b_ptr, slot) == b {
                set_i32(out_ptr, i, existing);
                break;
            }
            slot = (slot + 1) & step;
            probe += 1;
        }
    }
    next
}

/// Reduce a column, optionally over a subset of rows. Writes an `f64` to `out_ptr`.
/// Returns 1 when a value exists, 0 for null.
///
/// A negative `index_length` means "every row". Missing rows are skipped by every aggregate, so
/// `avg` of an all-missing column is null rather than NaN.
///
/// # Safety
/// All pointers must address host-owned buffers; `slot_capacity` must be a power of two when
/// `op` is `countDistinct`.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn og_aggregate(
    op: u32,
    value_ptr: AccelPtr,
    presence_ptr: AccelPtr,
    length: u32,
    index_ptr: AccelPtr,
    index_length: i32,
    slot_key_ptr: AccelPtr,
    slot_state_ptr: AccelPtr,
    slot_capacity: u32,
    out_ptr: AccelPtr,
) -> u32 {
    let n = length as usize;
    let count = if index_length < 0 { n } else { index_length as usize };
    let cap = slot_capacity as usize;
    if op == 3 {
        for s in 0..cap {
            set_i32(slot_state_ptr, s, 0);
        }
    }
    let step = cap.wrapping_sub(1);

    let mut sum = 0.0f64;
    let mut seen = 0u32;
    let mut acc = 0.0f64;
    let mut has_acc = false;
    let mut distinct = 0u32;

    for p in 0..count {
        let row = if index_length < 0 { p as i32 } else { i32_at(index_ptr, p) };
        if row < 0 || row as usize >= n {
            continue;
        }
        let row = row as usize;
        if !is_present(presence_ptr, row) {
            continue;
        }
        let v = f64_at(value_ptr, row);
        seen += 1;
        match op {
            0 | 1 => sum += v,
            4 => {
                if !has_acc || v < acc {
                    acc = v;
                    has_acc = true;
                }
            }
            5 => {
                if !has_acc || v > acc {
                    acc = v;
                    has_acc = true;
                }
            }
            6 => {
                if !has_acc {
                    acc = v;
                    has_acc = true;
                }
            }
            7 => {
                acc = v;
                has_acc = true;
            }
            3 => {
                let key = norm(v);
                let mut slot = hash_f64(key) & step;
                let mut probe = 0usize;
                while probe < cap {
                    if i32_at(slot_state_ptr, slot) == 0 {
                        set_f64(slot_key_ptr, slot, key);
                        set_i32(slot_state_ptr, slot, 1);
                        distinct += 1;
                        break;
                    }
                    if f64_at(slot_key_ptr, slot) == key {
                        break;
                    }
                    slot = (slot + 1) & step;
                    probe += 1;
                }
            }
            _ => {}
        }
    }

    match op {
        0 => {
            set_f64(out_ptr, 0, sum);
            1
        }
        1 => {
            if seen == 0 {
                0
            } else {
                set_f64(out_ptr, 0, sum / seen as f64);
                1
            }
        }
        2 => {
            set_f64(out_ptr, 0, seen as f64);
            1
        }
        3 => {
            set_f64(out_ptr, 0, distinct as f64);
            1
        }
        _ => {
            if has_acc {
                set_f64(out_ptr, 0, acc);
                1
            } else {
                0
            }
        }
    }
}

/// Boolean combination of two packed bitmaps. Bits at or beyond `bit_length` in the final byte are
/// cleared, so two bitmaps of the same logical length always compare byte-equal.
///
/// # Safety
/// All pointers must address host-owned buffers of at least `byte_length_for(bit_length)` bytes.
#[no_mangle]
pub unsafe extern "C" fn og_bitmap_op(
    op: u32,
    a_ptr: AccelPtr,
    b_ptr: AccelPtr,
    bit_length: u32,
    out_ptr: AccelPtr,
) {
    let byte_count = byte_length_for(bit_length as usize);
    for i in 0..byte_count {
        let a = u8_at(a_ptr, i);
        let b = u8_at(b_ptr, i);
        let r = match op {
            0 => a & b,
            1 => a | b,
            2 => !a,
            3 => a & !b,
            _ => a ^ b,
        };
        set_u8(out_ptr, i, r);
    }
    let tail = (bit_length & 7) as u8;
    if tail != 0 && byte_count > 0 {
        let last = byte_count - 1;
        set_u8(out_ptr, last, u8_at(out_ptr, last) & ((1u8 << tail) - 1));
    }
}

/// Select the `k` best rows, returning their indices in final order.
///
/// A bounded max-heap of the *worst* survivor, so the scan is O(n log k) rather than a full sort.
/// The survivors are then selection-sorted into the output: `k` is small by definition, so O(k²) is
/// the cheap choice and keeps a second sort implementation out of the kernel.
///
/// Taken survivors are marked with `i32::MIN` in the scratch heap rather than in a side array,
/// because this crate must not allocate — the host owns the heap.
///
/// # Safety
/// All pointers must address host-owned buffers; `scratch_ptr` must hold at least `k` `i32`s.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn og_top_k(
    value_ptr: AccelPtr,
    presence_ptr: AccelPtr,
    length: u32,
    k: u32,
    descending: u32,
    missing_first: u32,
    out_ptr: AccelPtr,
    scratch_ptr: AccelPtr,
) -> u32 {
    let n = length as usize;
    let want = (k as usize).min(n);
    if want == 0 {
        return 0;
    }
    let worse = |x: usize, y: usize| -> bool {
        total_compare(value_ptr, presence_ptr, x, y, descending, missing_first) > 0
    };

    let mut size = 0usize;
    for i in 0..n {
        if size < want {
            set_i32(scratch_ptr, size, i as i32);
            let mut c = size;
            size += 1;
            while c > 0 {
                let parent = (c - 1) >> 1;
                if !worse(i32_at(scratch_ptr, c) as usize, i32_at(scratch_ptr, parent) as usize) {
                    break;
                }
                let t = i32_at(scratch_ptr, c);
                set_i32(scratch_ptr, c, i32_at(scratch_ptr, parent));
                set_i32(scratch_ptr, parent, t);
                c = parent;
            }
            continue;
        }
        if !worse(i32_at(scratch_ptr, 0) as usize, i) {
            continue;
        }
        set_i32(scratch_ptr, 0, i as i32);
        let mut p = 0usize;
        loop {
            let l = 2 * p + 1;
            let r = l + 1;
            let mut big = p;
            if l < size && worse(i32_at(scratch_ptr, l) as usize, i32_at(scratch_ptr, big) as usize)
            {
                big = l;
            }
            if r < size && worse(i32_at(scratch_ptr, r) as usize, i32_at(scratch_ptr, big) as usize)
            {
                big = r;
            }
            if big == p {
                break;
            }
            let t = i32_at(scratch_ptr, p);
            set_i32(scratch_ptr, p, i32_at(scratch_ptr, big));
            set_i32(scratch_ptr, big, t);
            p = big;
        }
    }

    // Heap order is not sorted order. Emit the survivors worst-last by repeatedly taking the best
    // remaining, marking it consumed in place.
    const TAKEN: i32 = i32::MIN;
    for out in 0..size {
        let mut best = -1i32;
        let mut best_slot = usize::MAX;
        for j in 0..size {
            let candidate = i32_at(scratch_ptr, j);
            if candidate == TAKEN {
                continue;
            }
            if best < 0 || worse(best as usize, candidate as usize) {
                best = candidate;
                best_slot = j;
            }
        }
        if best_slot != usize::MAX {
            set_i32(scratch_ptr, best_slot, TAKEN);
        }
        set_i32(out_ptr, out, best);
    }
    size as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **There are deliberately no host unit tests for the kernels themselves, and the reason is
    /// structural rather than laziness.**
    ///
    /// Every pointer in this ABI is a `u32`, because on wasm32 a pointer *is* a byte offset into
    /// linear memory. On a 64-bit host an address does not fit in 32 bits, so a test that passed
    /// `vec.as_mut_ptr() as u32` would silently truncate and the kernel would read a garbage
    /// address — which is exactly what the first version of this module did, faulting with
    /// `STATUS_ACCESS_VIOLATION` rather than failing an assertion. Simulating linear memory with an
    /// arena would mean giving every kernel a base parameter the real ABI does not have, i.e.
    /// testing a different function from the one that ships.
    ///
    /// So correctness is proven **where the module actually runs**, against a harness written by
    /// the consumer rather than by this repo: onegrid's `differential.property.test.ts` drives
    /// `createWasmBackend(<this module>)` and its JavaScript reference backend over fast-check
    /// generators weighted toward NaN, `-0`, infinities, ties, duplicates and zero-length columns,
    /// and asserts they compute the same function. See `docs/integration.md`.
    ///
    /// ```sh
    /// cargo build -p index-accel --release --target wasm32-unknown-unknown
    /// cd ../onegrid-index-eval/packages/wasm && npx vitest run
    /// ```
    ///
    /// What can be checked here is the part that needs no pointers: the version the host bind-time
    /// check compares against.
    #[test]
    fn abi_version_matches_the_host_constant() {
        // If this changes, `packages/wasm/src/abi.ts` must change with it, or the host rejects the
        // module at bind time — which is the designed behaviour, not a bug.
        assert_eq!(og_abi_version(), 1);
    }

    #[test]
    fn byte_length_rounds_up() {
        assert_eq!(byte_length_for(0), 0);
        assert_eq!(byte_length_for(1), 1);
        assert_eq!(byte_length_for(8), 1);
        assert_eq!(byte_length_for(9), 2);
    }

    /// `-0.0` and `0.0` must hash and group identically; onegrid's model requires it.
    #[test]
    fn negative_zero_normalizes() {
        assert_eq!(norm(-0.0).to_bits(), 0.0f64.to_bits());
        assert_eq!(hash_f64(norm(-0.0)), hash_f64(norm(0.0)));
    }
}
