//! `scale` — bench/roadmap/p7-scale.md
//!
//! **The "a million rows, milliseconds" claim, tested.**
//!
//! Every other benchmark in this repo runs at thousands of documents. `real-corpus` proved the
//! engine correct on 1,322 and 1,940 real documents, and that is not evidence about 260 K products
//! or a million rows. This one answers the scaling question with a real corpus that is 30× larger,
//! then extends it by replication to a million.
//!
//! **Corpus: `blead/data/schools-masterlist.csv` — the DepEd school masterlist, 61,468 real
//! Philippine schools** with name, street address, barangay, municipality, division and region.
//! Real Filipino place names, real abbreviations (`ES`, `NHS`, `Brgy.`), real collisions — dozens of
//! schools share a name and differ only by locality, which is the hard case for ranking.
//!
//! **Above the real corpus, documents are synthesized by recombining real field values** — a real
//! school name with a different real barangay/municipality and a different real region, walked with
//! coprime strides so combinations do not repeat. This is stated in the output, never hidden.
//!
//! An earlier version of this bench **replicated whole documents** with a unique marker token, and
//! that was a methodological error worth recording: it produced ~16 near-identical copies of every
//! school, which is precisely the corpus shape that defeats top-k pruning — thousands of documents
//! tie near the threshold, so nothing can be skipped. It measured 19.5 ms p99 at 1 M and sent three
//! optimization attempts chasing a problem the data had invented. Recombination keeps every
//! document distinct while keeping every token real.
//!
//! What recombination still cannot simulate is genuinely new vocabulary from a new domain, so the
//! rows above 61,467 are a measurement of **posting-list and top-k scaling** — which is what decides
//! query latency — and not a claim about dictionary growth on unseen text.

mod timer;

use index_text::{Doc, Field, Index, IndexBuilder, Schema};
use std::path::PathBuf;

/// The claim under test: a million documents, still inside one keystroke frame.
const P99_NS_MAX: f64 = 5_000_000.0;
/// Serialized bytes per document. A 1 M-document index that does not fit on a disk is not a
/// product; this bounds it at roughly 200 MB.
const BYTE_PER_DOC_MAX: f64 = 200.0;


/// Bench-layer A/B switch for p90: the library reads no environment, so the bench does.
/// `INDEX_REORDER=1` builds every index in this bin with reordering ON — note that benches
/// whose truth maps results by ordinal (most of them) are then scoring against the wrong
/// rows; that harness limitation is recorded in the p90 document.
fn reorder_on() -> bool {
    std::env::var("INDEX_REORDER").as_deref() == Ok("1")
}

fn corpus_path() -> PathBuf {
    if let Ok(d) = std::env::var("INDEX_CORPUS_DIR") {
        return PathBuf::from(d).join("blead").join("data").join("schools-masterlist.csv");
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("blead")
        .join("data")
        .join("schools-masterlist.csv")
}

/// Minimal RFC-4180-ish CSV row splitter: honours double quotes and doubled quotes inside them.
/// The DepEd masterlist has addresses containing commas and embedded newlines, so a `split(',')`
/// would silently shred the corpus.
fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if in_quote && it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            '"' => in_quote = !in_quote,
            ',' if !in_quote => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

struct School {
    name: String,
    locality: String,
    region: String,
}

fn load(path: &std::path::Path) -> Option<Vec<School>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut line = text.lines();
    let header = split_csv(line.next()?);
    // Column names are looked up rather than positional — the masterlist's header is partly
    // mangled, and a positional read would silently index the wrong fields.
    let col = |want: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(want));
    let (c_name, c_street, c_muni, c_brgy, c_region, c_div) = (
        col("School Name")?,
        col("Street Address"),
        col("Municipality"),
        col("Barangay"),
        col("Region"),
        col("Division"),
    );
    let get = |f: &[String], i: Option<usize>| -> String {
        i.and_then(|i| f.get(i)).map(|s| s.trim().to_string()).unwrap_or_default()
    };

    let mut out = Vec::new();
    for l in line {
        let f = split_csv(l);
        if f.len() <= c_name {
            continue;
        }
        let name = f[c_name].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let locality = format!(
            "{} {} {}",
            get(&f, c_brgy),
            get(&f, c_muni),
            get(&f, c_street)
        )
        .trim()
        .to_string();
        let region = format!("{} {}", get(&f, c_region), get(&f, c_div)).trim().to_string();
        out.push(School { name, locality, region });
    }
    Some(out)
}

#[inline]
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One-character corruption, never at position 0 and never on a digit.
fn corrupt(q: &str, st: &mut u64) -> String {
    let ch: Vec<char> = q.chars().collect();
    let cand: Vec<usize> = (1..ch.len()).filter(|&i| ch[i].is_alphabetic()).collect();
    if cand.len() < 2 {
        return q.to_string();
    }
    let i = cand[(splitmix64(st) % cand.len() as u64) as usize];
    let mut out = ch.clone();
    match splitmix64(st) % 3 {
        0 => {
            out.remove(i);
        }
        1 => out[i] = char::from(b'a' + (splitmix64(st) % 26) as u8),
        _ => {
            if i + 1 < out.len() && out[i + 1].is_alphabetic() {
                out.swap(i, i + 1)
            } else {
                out.remove(i);
            }
        }
    }
    out.into_iter().collect()
}

fn build(school: &[School], target: usize) -> (Index, f64) {
    let schema = Schema::new(vec![
        Field::new("name", 3.0, 0.4),
        Field::new("locality", 1.5, 0.5),
        Field::new("region", 1.0, 0.6),
    ]);
    let mut b = IndexBuilder::new(schema).with_doc_reorder(reorder_on());
    let t0 = std::time::Instant::now();
    let n = school.len();
    // Coprime strides over the real corpus, so above `n` documents are distinct recombinations of
    // real text rather than copies. 7919 and 104729 are prime and coprime to any realistic `n`.
    for i in 0..target {
        let name = &school[i % n];
        let loc = &school[(i.wrapping_mul(7919)) % n];
        let reg = &school[(i.wrapping_mul(104729)) % n];
        b.add(&Doc::new([
            name.name.as_str(),
            loc.locality.as_str(),
            reg.region.as_str(),
        ]));
    }
    let ix = b.build().expect("build");
    (ix, t0.elapsed().as_secs_f64() * 1e3)
}

fn main() {
    let clock = timer::Clock::new();
    println!("p7-scale :: does 'a million rows, milliseconds' hold?");
    println!("clock backend: {} ({:.3} cycles/ns)\n", clock.backend(), clock.cycles_per_ns());

    let path = corpus_path();
    let Some(school) = load(&path) else {
        println!("SKIP: {} not found.", path.display());
        println!("      Set INDEX_CORPUS_DIR to the directory holding the blead/ checkout.");
        println!("OVERALL: NO CORPUS — refusing to report a verdict.");
        std::process::exit(2);
    };
    let real = school.len();
    println!("corpus: {} REAL Philippine schools (DepEd masterlist)\n", real);

    let ladder: Vec<usize> = match std::env::var("INDEX_BENCH_N").ok().and_then(|s| s.parse().ok()) {
        Some(n) => vec![n],
        None => vec![5_000, 20_000, real, 250_000, 1_000_000],
    };

    println!(
        "{:>9}  {:>8}  {:>9}  {:>10}  {:>8}  {:>9}  {:>9}  {:>9}",
        "docs", "terms", "build ms", "bytes", "B/doc", "exact p50", "typo p50", "typo p99"
    );
    println!("{}", "-".repeat(88));

    let mut all_pass = true;
    let mut worst_p99 = 0.0f64;

    for &n in &ladder {
        let (ix, build_ms) = build(&school, n);
        let bytes = ix.to_bytes();
        let byte_per_doc = bytes.len() as f64 / ix.doc_count() as f64;

        // Query set drawn from the real slice only, so queries are always real school names.
        let mut st = 0xC0FFEEu64;
        let probe: Vec<String> = (0..2_000)
            .map(|_| school[(splitmix64(&mut st) as usize) % real].name.clone())
            .collect();
        let dirty: Vec<String> = probe.iter().map(|q| corrupt(q, &mut st)).collect();

        let mut e = clock.time_each(probe.len(), |i| ix.search(&probe[i], 10).len() as u64);
        e.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut t = clock.time_each(dirty.len(), |i| ix.search(&dirty[i], 10).len() as u64);
        t.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let e50 = timer::percentile(&e, 0.50);
        let t50 = timer::percentile(&t, 0.50);
        let t99 = timer::percentile(&t, 0.99);
        worst_p99 = worst_p99.max(t99);

        if std::env::var("INDEX_DIAG").is_ok() && n >= 1_000_000 {
            // Find the actual tail: which queries are slow, and what work do they do?
            let mut worst: Vec<(f64, usize, u64, String)> = Vec::new();
            for q in dirty.iter().take(400) {
                let t0 = std::time::Instant::now();
                let (_, nterm, npost) = ix.search_stat(q, 10);
                worst.push((t0.elapsed().as_secs_f64() * 1e6, nterm, npost, q.clone()));
            }
            worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            println!("
  DIAG slowest typo queries at 1M (us, query_terms, postings_available):");
            for (us, nt, np, q) in worst.iter().take(5) {
                println!("    {us:>8.0}us  terms={nt:>3}  postings={np:>9}  {q:?}");
            }
            let med = &worst[worst.len() / 2];
            println!("    median: {:.0}us  terms={}  postings={}
", med.0, med.1, med.2);
        }
        // ---- Expansion-cap sweep (p29) --------------------------------------------------
        // The ONE remaining lever on the typo tail. `p26` made both pruning bounds exact and
        // `p27` showed they have no slack left, so the tail is the cost of ranking every
        // expansion of every token -- not a pruning failure. Capping expansion ranks fewer of
        // them, which is a correctness tradeoff and therefore has to be priced, not assumed.
        //
        // Agreement is measured against the DEFAULT (cap 16) answer, which `pool-audit` has
        // already shown is exact against brute force.
        if std::env::var("INDEX_CAP_SWEEP").is_ok() && n >= 1_000_000 {
            println!("
  CAP SWEEP at {n} docs -- trading recall for tail latency");
            println!(
                "    {:>4}  {:>9}  {:>9}  {:>11}  {:>11}",
                "cap", "typo p50", "typo p99", "top-10 same", "rank-1 same"
            );
            let truth: Vec<Vec<u32>> = dirty
                .iter()
                .map(|q| ix.search(q, 10).iter().map(|h| h.doc).collect())
                .collect();
            // Control: the SAME harness timing plain `search`. Kept permanently because it is
            // what caught the first version of this sweep reporting nanoseconds labelled `us` --
            // `time_each` returns ns and the main table divides by 1000. A sweep that cannot
            // reproduce the row above it is measuring something else.
            let mut ctl = clock.time_each(dirty.len(), |i| ix.search(&dirty[i], 10).len() as u64);
            ctl.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "    ctrl  {:>7.0}us  {:>7.0}us   (plain search through the sweep harness)",
                timer::percentile(&ctl, 0.50) / 1000.0,
                timer::percentile(&ctl, 0.99) / 1000.0
            );
            for cap in [16usize, 8, 4, 2, 1] {
                let mut c = clock.time_each(dirty.len(), |i| {
                    ix.search_capped(&dirty[i], 10, cap).len() as u64
                });
                c.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let mut same = 0usize;
                let mut same1 = 0usize;
                for (i, q) in dirty.iter().enumerate() {
                    let got: Vec<u32> = ix.search_capped(q, 10, cap).iter().map(|h| h.doc).collect();
                    if got == truth[i] {
                        same += 1;
                    }
                    if got.first() == truth[i].first() {
                        same1 += 1;
                    }
                }
                let pct = |x: usize| 100.0 * x as f64 / dirty.len() as f64;
                println!(
                    "    {:>4}  {:>7.0}us  {:>7.0}us  {:>10.2}%  {:>10.2}%",
                    cap,
                    timer::percentile(&c, 0.50) / 1000.0,
                    timer::percentile(&c, 0.99) / 1000.0,
                    pct(same),
                    pct(same1)
                );
            }
            println!();
        }

        let tag = if n <= real { "real" } else { "recombined" };
        println!(
            "{:>9}  {:>8}  {:>9.0}  {:>10}  {:>8.1}  {:>7.0}us  {:>7.0}us  {:>7.0}us   {}",
            ix.doc_count(),
            ix.term_count(),
            build_ms,
            bytes.len(),
            byte_per_doc,
            e50 / 1000.0,
            t50 / 1000.0,
            t99 / 1000.0,
            tag
        );

        // Serialization must survive at scale too — a format that only round-trips small indexes
        // is not a format.
        let reloaded = Index::from_bytes(&bytes).expect("reload at scale");
        assert_eq!(reloaded.doc_count(), ix.doc_count());
        assert_eq!(
            reloaded.search(&probe[0], 10),
            ix.search(&probe[0], 10),
            "a reloaded index must answer identically at {n} docs"
        );

        if t99 > P99_NS_MAX || byte_per_doc > BYTE_PER_DOC_MAX {
            all_pass = false;
        }
    }

    println!();
    println!(
        "  -> worst typo p99 {:.2} ms (bar {:.0} ms): {}",
        worst_p99 / 1e6,
        P99_NS_MAX / 1e6,
        if worst_p99 <= P99_NS_MAX { "PASS" } else { "FAIL" }
    );
    println!("  -> serialized index reloads identically at every scale: PASS");
    println!("\nOVERALL: {}", if all_pass { "PASS" } else { "FAIL" });
    if !all_pass {
        std::process::exit(1);
    }
}
