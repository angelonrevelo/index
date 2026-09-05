//! `fuzzy-term` — bench/roadmap/p5-fuzzy-term-feasibility.md
//!
//! **The question this answers:** can a typo-tolerant term dictionary meet the byte budget and
//! latency budget that the real consumers impose, using `tantivy-fst` + `levenshtein_automata`
//! rather than a from-scratch structure?
//!
//! The consumers set the thresholds (see `docs/research/demand.md`):
//!
//! - **profstopick** ships an 11,949-entry index to the *browser*, where the JSON form occupies
//!   **95.6% of the 5 MB localStorage quota**, and measured **109 of 267 real searches returning
//!   nothing** because the matcher is prefix-only. Budget: the term dictionary must be a small
//!   fraction of a megabyte, and a fuzzy lookup must be far under one keystroke frame.
//! - **presyo** has ~260 K canonical products / ~296 K aliases and a frozen 60-query fixture whose
//!   `typo` and `joined` categories (`coka cola`, `colgaye`, `nescaffe`, `cocacola`) are exactly
//!   what a prefix matcher and a `pg_trgm` threshold miss.
//!
//! **Policy under test** (the convergent production design — Typesense / Meilisearch / Algolia /
//! Lucene all landed here independently, see `docs/research/relevance.md` §7):
//! `≤2 edits hard-capped · length gates at ~4 and ~8 · first character protected (prefix anchor)
//! · numeric tokens exempt · fired lazily only when exact match underdelivers.`
//!
//! **The thresholds come from the consumers, not from taste** (`docs/research/demand.md`):
//!   1. **≤ 8 bytes/key** for the term dictionary. `fst`'s published figure is 2.7 B/key at 119 K
//!      dictionary words; ours are longer and mixed-script, so 8 is a deliberately loose bar. The
//!      binding constraint is profstopick's browser budget: its 2.5 MB JSON shard already eats
//!      95.6 % of the 5 MB localStorage quota.
//!   2. **exact lookup p99 ≤ 1 000 ns.** This is the 90 % path — production engines only fire
//!      fuzzy on the deficit — so it must be effectively free.
//!   3. **fuzzy p99 ≤ 5 000 000 ns (5 ms).** One keystroke frame is 16.7 ms and profstopick's
//!      *entire* post-fix match pass measures 0.43 ms. Fuzzy fires only when exact underdelivers,
//!      so a 5 ms ceiling on the fallback path keeps the worst keystroke inside one frame with
//!      3× headroom. (An earlier draft of this bench used 0.1 ms, which was a number picked
//!      because it looked tidy — it had no consumer behind it and is not used.)
//!   4. **typo recall must at least double** versus exact-only. profstopick measured 109 of 267
//!      real searches returning nothing; a fuzzy layer that does not move that number is not
//!      worth its bytes.
//!
//! **Refuted hypothesis, kept because it was measured.** This bench was written expecting
//! prefix-anchoring to be a *speed* lever (the intuition: protecting the first character prunes
//! the automaton early). It does not — anchored and unanchored ED-2 measure within noise of each
//! other on a token dictionary, because when the query *is* a whole token the prefix constraint
//! removes almost nothing. First-character protection is therefore a **precision and typeahead**
//! rule, not a performance one, and the roadmap must not claim it as the latter. The comparison
//! is still reported on every run so the claim stays falsifiable.

mod timer;

use levenshtein_automata::{Distance, LevenshteinAutomatonBuilder, DFA, SINK_STATE};
use std::collections::BTreeSet;
use tantivy_fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer};

/// `levenshtein_automata::DFA` does not implement `tantivy_fst::Automaton` (the `fst_automaton`
/// feature targets the upstream `fst` crate, which is a *different type* from `tantivy-fst`'s).
/// Tantivy solves this with the same three-line wrapper.
struct Dfa(DFA);

impl Automaton for Dfa {
    type State = u32;
    #[inline]
    fn start(&self) -> u32 {
        self.0.initial_state()
    }
    #[inline]
    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }
    #[inline]
    fn can_match(&self, state: &u32) -> bool {
        *state != SINK_STATE
    }
    #[inline]
    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

// ---------------------------------------------------------------------------------------------
// Corpus. Deterministic, dependency-free, and shaped like the two real corpora rather than like
// random bytes — an FST's size is dominated by shared prefixes/suffixes, so random strings would
// flatter nothing and measure nothing.
// ---------------------------------------------------------------------------------------------

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn pick<'a>(pool: &[&'a str], state: &mut u64) -> &'a str {
    pool[(splitmix64(state) % pool.len() as u64) as usize]
}

/// Philippine surname / given-name components — profstopick's shape (`SURNAME, GIVEN M.`).
/// Heavy shared-prefix structure is the point: it is what an FST exploits and a hash map cannot.
const SURNAME: &[&str] = &[
    "dela cruz", "delos santos", "reyes", "santos", "bautista", "ocampo", "mercado", "aquino",
    "villanueva", "gonzales", "ramos", "mendoza", "torres", "flores", "castillo", "rivera",
    "domingo", "salvador", "navarro", "pascual", "aguilar", "morales", "fernandez", "rodriguez",
    "hernandez", "martinez", "garcia", "lopez", "perez", "sanchez", "ramirez", "cruz", "tolentino",
    "manalo", "dizon", "espiritu", "javier", "lazaro", "marasigan", "nicolas", "obispo", "padilla",
    "quijano", "rosales", "soriano", "trinidad", "urbano", "valdez", "yalung", "zamora",
    "penaflorida", "manalastas", "buenaventura", "concepcion", "evangelista", "magbanua",
];
const GIVEN: &[&str] = &[
    "maria", "jose", "juan", "ana", "antonio", "rosa", "carlos", "elena", "miguel", "teresa",
    "ricardo", "cristina", "eduardo", "patricia", "francisco", "angelica", "roberto", "jasmine",
    "gabriel", "kristine", "emmanuel", "michelle", "raymond", "katrina", "nathaniel", "bianca",
    "reginald", "clarissa", "sebastian", "veronica", "leonardo", "margarita", "benedict",
];

/// Grocery product components — presyo's shape (`BRAND VARIANT SIZE`).
const BRAND: &[&str] = &[
    "coca cola", "pepsi", "nestle", "bear brand", "alaska", "milo", "nescafe", "kopiko", "lucky me",
    "pancit canton", "argentina", "century tuna", "555", "ligo", "sardinas", "colgate", "closeup",
    "safeguard", "palmolive", "sunsilk", "creamsilk", "head and shoulders", "surf", "tide", "ariel",
    "downy", "joy", "zonrox", "domex", "datu puti", "silver swan", "mang tomas", "knorr", "maggi",
    "magnolia", "purefoods", "cdo", "swift", "san miguel", "red horse", "gatorade", "zesto",
    "yakult", "jack n jill", "oishi", "chippy", "piattos", "nova", "rebisco", "skyflakes",
];
const VARIANT: &[&str] = &[
    "original", "classic", "regular", "sweet", "spicy", "hot", "extra", "plus", "lite", "zero",
    "powdered", "condensed", "evaporated", "fresh", "chilled", "family pack", "value pack",
    "reseal", "sachet", "refill", "twin pack", "big pack", "mini", "jumbo", "premium",
];
const UNIT: &[&str] = &["g", "ml", "l", "kg", "pcs", "s", "pack"];

/// Filipino-ish CV syllables. A closed pool of 56 surnames yields 115 distinct tokens, which is
/// not a term dictionary — it is a rounding error, and it makes the FST look better than it is.
/// Real vocabularies have a long tail of rare tokens (profstopick ~11.9 K labels, presyo ~296 K
/// aliases), and fuzzy cost scales with **vocabulary size, not document count**. So every label
/// also carries a generated rare token, which is what actually loads the automaton.
const SYL: &[&str] = &[
    "ba", "ka", "da", "ga", "ha", "la", "ma", "na", "pa", "ra", "sa", "ta", "wa", "ya", "bi", "ki",
    "di", "gi", "hi", "li", "mi", "ni", "pi", "ri", "si", "ti", "wi", "yi", "bo", "ko", "do", "go",
    "ho", "lo", "mo", "no", "po", "ro", "so", "to", "bu", "ku", "du", "gu", "hu", "lu", "mu", "nu",
    "pu", "ru", "su", "tu", "be", "ke", "de", "ge", "le", "me", "ne", "pe", "re", "se", "te",
];

/// A rare token of 3–5 syllables — long enough to fall in the 8+ length gate (2 edits allowed),
/// which is the expensive case the bench exists to price.
fn rare_token(st: &mut u64) -> String {
    let n = 3 + (splitmix64(st) % 3) as usize;
    (0..n).map(|_| pick(SYL, st)).collect()
}

fn gen_name_corpus(n: usize, seed: u64) -> Vec<String> {
    let mut st = seed;
    let mut out = BTreeSet::new();
    let mut guard = 0usize;
    while out.len() < n && guard < n * 40 {
        guard += 1;
        let initial = (b'a' + (splitmix64(&mut st) % 26) as u8) as char;
        out.insert(format!(
            "{}, {} {}. {}",
            pick(SURNAME, &mut st),
            pick(GIVEN, &mut st),
            initial,
            rare_token(&mut st)
        ));
    }
    out.into_iter().collect()
}

fn gen_product_corpus(n: usize, seed: u64) -> Vec<String> {
    let mut st = seed;
    let mut out = BTreeSet::new();
    let mut guard = 0usize;
    while out.len() < n && guard < n * 40 {
        guard += 1;
        let size = 1 + splitmix64(&mut st) % 2000;
        out.insert(format!(
            "{} {} {} {}{}",
            pick(BRAND, &mut st),
            pick(VARIANT, &mut st),
            rare_token(&mut st),
            size,
            pick(UNIT, &mut st)
        ));
    }
    out.into_iter().collect()
}

/// The term dictionary holds *tokens*, not whole labels. Both are measured: a label-keyed FST is
/// what profstopick's browser shard would become, a token-keyed FST is what a BM25 index needs.
fn tokenize(labels: &[String]) -> Vec<String> {
    let mut set = BTreeSet::new();
    for l in labels {
        for t in l.split(|c: char| !c.is_alphanumeric()) {
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set.into_iter().collect()
}

/// Returns the built map and the exact byte length of its serialized FST — the number the
/// browser byte budget is spent from.
fn build_map(keys: &[String]) -> (Map<Vec<u8>>, usize) {
    let mut b = MapBuilder::memory();
    for (i, k) in keys.iter().enumerate() {
        b.insert(k.as_bytes(), i as u64).expect("keys must be sorted & unique");
    }
    let bytes = b.into_inner().expect("fst build");
    let len = bytes.len();
    (Map::from_bytes(bytes).expect("valid fst"), len)
}

// ---------------------------------------------------------------------------------------------

/// Corrupt a term the way a real user does — the corruption kinds the production engines'
/// length gates are calibrated for. Never corrupts a digit (the numeric-token exemption).
fn corrupt(term: &str, st: &mut u64) -> String {
    let ch: Vec<char> = term.chars().collect();
    if ch.len() < 4 {
        return term.to_string();
    }
    // Never touch index 0: first-character protection is the rule under test.
    let i = 1 + (splitmix64(st) % (ch.len() as u64 - 1)) as usize;
    if ch[i].is_ascii_digit() {
        return term.to_string();
    }
    let mut out: Vec<char> = ch.clone();
    match splitmix64(st) % 3 {
        0 => {
            out.remove(i);
        } // deletion
        1 => out[i] = ((b'a' + (splitmix64(st) % 26) as u8) as char).to_ascii_lowercase(), // substitution
        _ => {
            if i + 1 < out.len() {
                out.swap(i, i + 1)
            }
        } // transposition
    }
    out.into_iter().collect()
}

/// The production length gate, verbatim from `docs/research/relevance.md` §7.
#[inline]
fn max_edit_for(term: &str) -> u8 {
    match term.chars().count() {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

struct Stat {
    p50: f64,
    p99: f64,
    hit: usize,
}

fn run_probe(
    clock: &timer::Clock,
    map: &Map<Vec<u8>>,
    probe: &[String],
    mode: &str,
    b1: &LevenshteinAutomatonBuilder,
    b2: &LevenshteinAutomatonBuilder,
) -> Stat {
    let mut hit = 0usize;
    let mut ns = clock.time_each(probe.len(), |i| {
        let q = &probe[i];
        let n = match mode {
            "exact" => map.get(q.as_bytes()).map_or(0u64, |_| 1),
            _ => {
                let d = max_edit_for(q);
                if d == 0 {
                    map.get(q.as_bytes()).map_or(0u64, |_| 1)
                } else {
                    let bld = if d == 1 { b1 } else { b2 };
                    // "anchored" = the DFA also requires the query to be a prefix, which is what
                    // first-character protection plus typeahead semantics buy you.
                    let dfa = if mode == "anchored" {
                        Dfa(bld.build_prefix_dfa(q))
                    } else {
                        Dfa(bld.build_dfa(q))
                    };
                    let mut s = map.search(&dfa).into_stream();
                    let mut c = 0u64;
                    while s.next().is_some() {
                        c += 1;
                    }
                    c
                }
            }
        };
        if n > 0 {
            hit += 1;
        }
        n
    });
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Stat { p50: timer::percentile(&ns, 0.50), p99: timer::percentile(&ns, 0.99), hit }
}

const BYTES_PER_KEY_MAX: f64 = 8.0;
const EXACT_P99_NS_MAX: f64 = 1_000.0;
/// 5 ms. One keystroke frame is 16.7 ms; fuzzy is the lazy fallback path only.
const FUZZY_P99_NS_MAX: f64 = 5_000_000.0;
/// The fuzzy layer must at least double typo recall or it is not worth its bytes.
const RECALL_GAIN_MIN: f64 = 2.0;

fn main() {
    let clock = timer::Clock::new();
    println!("p5-fuzzy-term :: typo-tolerant term dictionary (tantivy-fst + levenshtein_automata)");
    println!("clock backend: {} ({:.3} cycles/ns)\n", clock.backend(), clock.cycles_per_ns());
    println!("policy under test: <=2 edits capped | gates 0-3:0 4-7:1 8+:2 | first char protected");
    println!("                   | digits never corrupted | lazy (exact first)\n");

    let b1 = LevenshteinAutomatonBuilder::new(1, true); // transpositions = Damerau
    let b2 = LevenshteinAutomatonBuilder::new(2, true);

    let scale: Vec<usize> = std::env::var("INDEX_BENCH_N")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(|n| vec![n])
        .unwrap_or_else(|| vec![11_949, 260_000, 1_000_000]);

    let mut all_pass = true;

    for &n in &scale {
        for (shape, labels) in [
            ("name/profstopick", gen_name_corpus(n, 0xC0FFEE)),
            ("product/presyo", gen_product_corpus(n, 0xBADF00D)),
        ] {
            let got = labels.len();
            let token = tokenize(&labels);

            let t0 = std::time::Instant::now();
            let (_label_map, label_byte) = build_map(&labels);
            let label_build = t0.elapsed();
            let t1 = std::time::Instant::now();
            let (token_map, token_byte) = build_map(&token);
            let token_build = t1.elapsed();

            let label_bpk = label_byte as f64 / got as f64;
            let token_bpk = token_byte as f64 / token.len() as f64;
            let raw: usize = labels.iter().map(|s| s.len()).sum();

            println!("[{shape}]  labels={got}  distinct_token={}", token.len());
            println!(
                "  label FST : {:>9} B  {:>6.2} B/key  build {:>7.0} ms  (raw utf8 {} B, {:.2}x)",
                label_byte,
                label_bpk,
                label_build.as_secs_f64() * 1e3,
                raw,
                raw as f64 / label_byte as f64
            );
            println!(
                "  token FST : {:>9} B  {:>6.2} B/key  build {:>7.0} ms",
                token_byte,
                token_bpk,
                token_build.as_secs_f64() * 1e3
            );

            // Probe set: 2 000 real tokens, each corrupted by one edit away from index 0.
            let mut st = 0x5EED_u64;
            let probe_n = 2_000.min(token.len());
            let clean: Vec<String> =
                (0..probe_n).map(|_| token[(splitmix64(&mut st) as usize) % token.len()].clone()).collect();
            let dirty: Vec<String> = clean.iter().map(|t| corrupt(t, &mut st)).collect();

            let exact_clean = run_probe(&clock, &token_map, &clean, "exact", &b1, &b2);
            let exact_dirty = run_probe(&clock, &token_map, &dirty, "exact", &b1, &b2);
            let anch = run_probe(&clock, &token_map, &dirty, "anchored", &b1, &b2);
            let unanch = run_probe(&clock, &token_map, &dirty, "unanchored", &b1, &b2);

            println!(
                "  exact   (clean q): p50 {:>8.0} ns  p99 {:>9.0} ns   recall {}/{}",
                exact_clean.p50, exact_clean.p99, exact_clean.hit, probe_n
            );
            println!(
                "  exact   (typo  q): p50 {:>8.0} ns  p99 {:>9.0} ns   recall {}/{}   <- the bug today",
                exact_dirty.p50, exact_dirty.p99, exact_dirty.hit, probe_n
            );
            println!(
                "  fuzzy anchored   : p50 {:>8.0} ns  p99 {:>9.0} ns   recall {}/{}",
                anch.p50, anch.p99, anch.hit, probe_n
            );
            println!(
                "  fuzzy unanchored : p50 {:>8.0} ns  p99 {:>9.0} ns   recall {}/{}",
                unanch.p50, unanch.p99, unanch.hit, probe_n
            );

            let recall_gain = anch.hit as f64 / exact_dirty.hit.max(1) as f64;
            let c1 = token_bpk <= BYTES_PER_KEY_MAX;
            let c2 = exact_clean.p99 <= EXACT_P99_NS_MAX;
            let c3 = anch.p99 <= FUZZY_P99_NS_MAX;
            let c4 = recall_gain >= RECALL_GAIN_MIN;
            println!(
                "  -> bytes/key: {}   exact p99: {}   fuzzy p99: {}   recall gain: {}",
                pf(c1),
                pf(c2),
                pf(c3),
                pf(c4)
            );
            println!(
                "  -> typo recall {} -> {} ({:.1}x)   |   anchor effect on p50: {:+.1}% (expected ~0)",
                exact_dirty.hit,
                anch.hit,
                recall_gain,
                100.0 * (unanch.p50 - anch.p50) / anch.p50.max(1.0)
            );
            println!();
            all_pass &= c1 && c2 && c3 && c4;
        }
    }

    println!("OVERALL: {}", if all_pass { "PASS" } else { "FAIL" });
    if !all_pass {
        std::process::exit(1);
    }
}

fn pf(b: bool) -> &'static str {
    if b {
        "PASS"
    } else {
        "FAIL"
    }
}
