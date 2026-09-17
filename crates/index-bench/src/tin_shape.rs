//! `tin-shape` — **PlanetScale TIN's published workload, at a size this machine can hold.**
//!
//! TIN (2026-09-16) published QPS / p99 for conjunction, disjunction and phrase queries, top-10 and
//! `COUNT(*)`, on Stack Exchange (150 M docs) and Wikipedia (8.0 GB). Their query trace is not
//! published, only its recipe, which this reproduces:
//!
//! - **queries**: substrings of 2 to 15 consecutive terms sampled from the corpus itself, each
//!   interpreted three ways — conjunction, disjunction, phrase;
//! - **load**: a fixed number of concurrent clients (theirs: 8 vCPU) looping for a fixed duration,
//!   after one warm-up pass over every query;
//! - **reported**: QPS, p50, p99 per workload, which is their table's shape.
//!
//! What it does NOT reproduce, stated so no number here is read as more than it is:
//!
//! - **no Postgres**: TIN's latency includes the executor and heap fetch of matched rows; this is
//!   the engine alone, in process. That favours this side.
//! - **no MB/query**: the index is resident, so bytes read per query is not a meaningful column.
//! - **ranking differs**: disjunction top-10 ranks all-terms-matched documents first (the typo
//!   bucket), then BM25F; TIN ranks by BM25 alone. Conjunction is the bucket-0 prefix of the same
//!   ranking, so it is exact top-10 of the all-terms set. Typo expansion is capped at 1 so a word
//!   present in the corpus matches only itself.
//!
//! Run (corpus is `title<TAB>text` per line; `scripts/tin-corpus.py` makes it from Wikipedia):
//!     INDEX_TIN_TSV=wiki-0.tsv cargo run -p index-bench --release --bin tin-shape
//!
//! Knobs: `INDEX_TIN_LIMIT` (docs), `INDEX_TIN_QUERY` (substrings; x3 queries, default 573 = 1,719
//! like theirs), `INDEX_TIN_THREAD` (default 8), `INDEX_TIN_SECOND` (per workload, default 30),
//! `INDEX_TIN_SEED`.

use index_text::{tokenize, Doc, Field, Index, IndexBuilder, Schema};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// A 2..=15-term run of consecutive tokens from `text`, starting at a random word boundary.
fn substring(text: &str, rng: &mut Rng) -> Option<String> {
    let want = 2 + rng.below(14);
    let start = rng.below(text.len());
    let from = text[..]
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| i >= start && text[..i].ends_with(' '))
        .unwrap_or(0);
    // A raw window generous enough for 15 tokens; tokenizing it all would cost a full document.
    let to = text[from..].char_indices().nth(want * 24).map_or(text.len(), |(i, _)| from + i);
    let tok = tokenize(&text[from..to]);
    (tok.len() > want).then(|| tok[..want].iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" "))
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    And,
    Or,
    Phrase,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::And => "conjunction",
            Kind::Or => "disjunction",
            Kind::Phrase => "phrase",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Top10,
    Count,
}

/// One query; returns the number of hits (top-10) or the count.
fn run(ix: &Index, shape: Shape, kind: Kind, q: &str) -> usize {
    match (shape, kind) {
        (Shape::Top10, Kind::Or) => ix.search_capped(q, 10, 1).len(),
        // Ranking is typo-bucket first, and bucket 0 means every term matched exactly, so the
        // bucket-0 prefix of the top-10 IS the top-10 of the conjunction.
        (Shape::Top10, Kind::And) => ix.search_capped(q, 10, 1).iter().filter(|h| h.typo_bucket == 0).count(),
        (Shape::Top10, Kind::Phrase) => ix.search_phrase(q, 10).len(),
        (Shape::Count, Kind::Or) => ix.count_any(q),
        (Shape::Count, Kind::And) => ix.count_all(q),
        (Shape::Count, Kind::Phrase) => unreachable!("no phrase count"),
    }
}

fn pct(sorted: &[u64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let i = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i] as f64 / 1e6
}

fn main() {
    let path = std::env::var("INDEX_TIN_TSV").expect("INDEX_TIN_TSV=<title\\ttext file>[,file...]");
    let limit = env_usize("INDEX_TIN_LIMIT", usize::MAX);
    let query_count = env_usize("INDEX_TIN_QUERY", 573);
    let thread = env_usize("INDEX_TIN_THREAD", 8);
    let second = env_usize("INDEX_TIN_SECOND", 30);
    let mut rng = Rng(env_usize("INDEX_TIN_SEED", 0x5eed) as u64 | 1);

    // ---- build ------------------------------------------------------------------------------
    let t0 = Instant::now();
    let schema = Schema::new(vec![Field::new("title", 2.0, 0.4), Field::new("text", 1.0, 0.75)]);
    let mut b = IndexBuilder::new(schema);
    assert!(b.set_position(), "positions are required for phrase queries");
    // Reservoir sample of substrings, so every document is equally likely to seed a query without
    // knowing the corpus size up front or keeping the text.
    let mut sample: Vec<String> = Vec::with_capacity(query_count);
    let (mut doc, mut byte) = (0usize, 0usize);
    'file: for p in path.split(',') {
        let f = std::fs::File::open(p).unwrap_or_else(|e| panic!("{p}: {e}"));
        for line in BufReader::with_capacity(1 << 20, f).lines() {
            if doc >= limit {
                break 'file;
            }
            let line = line.expect("utf-8 line");
            let (title, text) = line.split_once('\t').unwrap_or(("", &line));
            b.add(&Doc::new([title, text]));
            byte += line.len();
            doc += 1;
            let slot = if sample.len() < query_count { Some(sample.len()) } else { Some(rng.below(doc)) };
            if let Some(s) = slot.filter(|&s| s < query_count) {
                if let Some(q) = substring(text, &mut rng) {
                    match s < sample.len() {
                        true => sample[s] = q,
                        false => sample.push(q),
                    }
                }
            }
        }
    }
    let ix = b.build().expect("build");
    let build = t0.elapsed();
    let stored = match std::env::var("INDEX_TIN_SIZE").as_deref() {
        Ok("0") => None,
        _ => Some(ix.to_bytes().len()),
    };
    println!("# tin-shape\n");
    println!("| corpus | docs | terms | build | index bytes | threads | seconds/workload |");
    println!("|---|---|---|---|---|---|---|");
    println!(
        "| {:.1} MB | {} | {} | {:.1} s | {} | {} | {} |\n",
        byte as f64 / 1e6,
        doc,
        ix.term_count(),
        build.as_secs_f64(),
        stored.map_or("—".into(), |n| format!("{:.1} MB", n as f64 / 1e6)),
        thread,
        second
    );

    // ---- workloads, in the order of TIN's tables -------------------------------------------
    let workload: [(&str, Shape, &[Kind]); 5] = [
        ("conjunction+disjunction+phrase; top-10", Shape::Top10, &[Kind::And, Kind::Or, Kind::Phrase]),
        ("conjunction+phrase; top-10", Shape::Top10, &[Kind::And, Kind::Phrase]),
        ("disjunction; top-10", Shape::Top10, &[Kind::Or]),
        ("conjunction+disjunction; COUNT", Shape::Count, &[Kind::And, Kind::Or]),
        ("disjunction; COUNT", Shape::Count, &[Kind::Or]),
    ];
    println!("| workload | queries | QPS | p50 ms | p99 ms | per kind: p99 ms (zero-result %) |");
    println!("|---|---|---|---|---|---|");
    for (name, shape, kinds) in workload {
        let query: Vec<(Kind, &str)> =
            sample.iter().flat_map(|q| kinds.iter().map(move |&k| (k, q.as_str()))).collect();
        // Warm-up: every query once, which also yields the zero-result rate.
        let zero: Vec<(Kind, usize)> = kinds
            .iter()
            .map(|&k| (k, query.iter().filter(|(qk, q)| *qk == k && run(&ix, shape, k, q) == 0).count()))
            .collect();

        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let start = Instant::now();
        let lat: Vec<Vec<(Kind, u64)>> = std::thread::scope(|s| {
            let worker: Vec<_> = (0..thread)
                .map(|_| {
                    s.spawn(|| {
                        let mut out = Vec::new();
                        while !stop.load(Ordering::Relaxed) {
                            let (k, q) = query[next.fetch_add(1, Ordering::Relaxed) % query.len()];
                            let t = Instant::now();
                            std::hint::black_box(run(&ix, shape, k, q));
                            out.push((k, t.elapsed().as_nanos() as u64));
                        }
                        out
                    })
                })
                .collect();
            std::thread::sleep(Duration::from_secs(second as u64));
            stop.store(true, Ordering::Relaxed);
            worker.into_iter().map(|w| w.join().expect("worker")).collect()
        });
        let wall = start.elapsed().as_secs_f64();
        let all: Vec<(Kind, u64)> = lat.into_iter().flatten().collect();
        let mut ns: Vec<u64> = all.iter().map(|x| x.1).collect();
        ns.sort_unstable();
        let per: Vec<String> = kinds
            .iter()
            .map(|&k| {
                let mut v: Vec<u64> = all.iter().filter(|x| x.0 == k).map(|x| x.1).collect();
                v.sort_unstable();
                let z = zero.iter().find(|x| x.0 == k).map_or(0, |x| x.1);
                format!("{} {:.2} ({:.0} %)", k.label(), pct(&v, 0.99), 100.0 * z as f64 / sample.len() as f64)
            })
            .collect();
        println!(
            "| {} | {} | {:.0} | {:.2} | {:.2} | {} |",
            name,
            query.len(),
            ns.len() as f64 / wall,
            pct(&ns, 0.50),
            pct(&ns, 0.99),
            per.join("; ")
        );
    }
}
