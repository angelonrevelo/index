//! `maphy-place` — **maphy's place search, on maphy's own data, against the defect it has.**
//!
//! `apps/web/src/components/map/lib/place-index.ts` implements `searchPlace()` as a **linear scan
//! over the whole loaded pool on every keystroke**: it scores every entry and every alias, collects
//! all non-zero hits, sorts them, and slices the top 24. The scoring function is reproduced exactly
//! in [`PlaceBaseline`] — it is four string predicates in priority order:
//!
//! ```text
//! norm === q                          -> 1000
//! norm.startsWith(q)                  ->  700 - min(len, 99)
//! some word in norm startsWith(q)     ->  400
//! norm.includes(q)                    ->  150
//! otherwise                           ->    0
//! ```
//!
//! **Every one of those four is an exact-substring test, so the score of a misspelling is zero.**
//! That is the defect this bin measures. A user who types `Zamboaga` or `Qezon` gets an empty
//! result list, not a near miss — and a place-name box is exactly where misspellings happen,
//! because the names are unfamiliar and long.
//!
//! # The corpus, and what is honestly missing from it
//!
//! `bench/fixture/maphy-place.txt` holds **1,067 real Philippine places** — 17 regions, 84
//! provinces, 966 municipalities with PSGC codes — decoded from maphy's own
//! `municipal_covid19_summary.pmtiles`. That is the shape of maphy's `top.json`, which
//! `scripts/export/place-index.ts` builds from `region + province + municity`.
//!
//! **maphy's second tier is not on disk.** `barangay.json` is ~42,000 more entries and
//! `apps/web/public/data/place/` is empty in this checkout, so the pool measured here is the one
//! that loads first, not the full one. Latency numbers below are therefore for a pool **~40x
//! smaller than the shipped worst case** and should be read as a floor on the gap, not a
//! measurement of it. The typo results do not depend on pool size.

mod timer;

use index_text::{Doc, Field, IndexBuilder, Schema};

/// A place name must be findable from its own exact text.
const EXACT_HIT1_MIN: f64 = 0.95;
/// The bar the baseline cannot clear, and the reason this bin exists.
const TYPO_HIT10_MIN: f64 = 0.80;
/// How strongly the administrative-level prior is applied, as a multiplier spread.
///
/// Overridable so the shape of the trade can be swept rather than asserted: at 0 the prior is off,
/// at 1.0 a region scores 5x a barangay on identical text.
fn prior_spread() -> f32 {
    std::env::var("INDEX_BENCH_PRIOR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.001)
}

/// Interactive budget for a per-keystroke typeahead.
const P99_NS_MAX: f64 = 5_000_000.0;

struct Place {
    level: String,
    name: String,
    parent: String,
}

/// maphy's shipped place matcher, reproduced.
///
/// Faithful to `place-index.ts`: the same four predicates, the same tie-break chain
/// (score, then level rank, then name length, then lexicographic), and the same linear scan over
/// the whole pool. No index, no typo tolerance.
struct PlaceBaseline {
    norm: Vec<String>,
    level_rank: Vec<u32>,
    len: Vec<usize>,
}

impl PlaceBaseline {
    /// `LEVEL_RANK` from `place-index.ts`: region 4, province 3, municity 2, barangay 1.
    fn rank_of(level: &str) -> u32 {
        match level {
            "region" => 4,
            "province" => 3,
            "municity" => 2,
            _ => 1,
        }
    }

    fn new(place: &[Place]) -> Self {
        PlaceBaseline {
            norm: place.iter().map(|p| normalize(&p.name)).collect(),
            level_rank: place.iter().map(|p| Self::rank_of(&p.level)).collect(),
            len: place.iter().map(|p| p.name.len()).collect(),
        }
    }

    fn score(norm: &str, q: &str) -> i32 {
        if norm == q {
            return 1000;
        }
        if norm.starts_with(q) {
            return 700 - norm.len().min(99) as i32;
        }
        if norm.split(' ').any(|w| w.starts_with(q)) {
            return 400;
        }
        if norm.contains(q) {
            return 150;
        }
        0
    }

    fn search(&self, query: &str, k: usize) -> Vec<u32> {
        let q = normalize(query);
        if q.is_empty() {
            return Vec::new();
        }
        let mut hit: Vec<(i32, u32, usize, u32)> = Vec::new();
        for i in 0..self.norm.len() {
            let s = Self::score(&self.norm[i], &q);
            if s > 0 {
                hit.push((s, self.level_rank[i], self.len[i], i as u32));
            }
        }
        // score desc, level desc, length asc, then index (standing in for localeCompare).
        hit.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3)));
        hit.into_iter().take(k).map(|h| h.3).collect()
    }
}

/// `normalizePlace` from maphy: lowercase, strip accents, collapse whitespace.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_space = true;
    for ch in s.chars() {
        let c = match ch {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'Á' | 'À' | 'Â' | 'Ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
            'ñ' | 'Ñ' => 'n',
            c => c.to_ascii_lowercase(),
        };
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out.trim().to_string()
}

/// Deterministic single-edit typos of the kind a person actually makes.
///
/// Four classes, chosen because they are what a keyboard produces: a dropped letter, a transposed
/// pair, a doubled letter, and a substituted letter. Applied at a position derived from the name
/// itself so the set is reproducible without a seed table.
fn typo(name: &str, kind: usize) -> String {
    let c: Vec<char> = name.chars().collect();
    if c.len() < 5 {
        return name.to_string();
    }
    // Bias away from the first character: a first-letter typo is a different (and much harder)
    // problem, and the engine's own policy deliberately protects the first character.
    let at = 1 + (c.len() * (kind + 2) / 7) % (c.len() - 2);
    let mut out: Vec<char> = c.clone();
    match kind % 4 {
        0 => {
            out.remove(at);
        }
        1 => out.swap(at, at + 1),
        2 => out.insert(at, c[at]),
        _ => out[at] = if c[at] == 'a' { 'e' } else { 'a' },
    }
    out.into_iter().collect()
}

#[derive(Default)]
struct Metric {
    n: usize,
    hit1: usize,
    hit10: usize,
    empty: usize,
}

impl Metric {
    fn observe(&mut self, rank: Option<usize>) {
        self.n += 1;
        match rank {
            Some(0) => {
                self.hit1 += 1;
                self.hit10 += 1;
            }
            Some(r) if r < 10 => self.hit10 += 1,
            Some(_) => {}
            None => self.empty += 1,
        }
    }
    fn hit1(&self) -> f64 {
        self.hit1 as f64 / self.n.max(1) as f64
    }
    fn hit10(&self) -> f64 {
        self.hit10 as f64 / self.n.max(1) as f64
    }
}

fn pct(x: f64) -> String {
    format!("{:.1}%", x * 100.0)
}
fn verdict(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}
fn rank_of(hit: &[index_text::Hit], want: usize) -> Option<usize> {
    hit.iter().position(|h| h.doc as usize == want)
}
fn rank_of_id(hit: &[u32], want: usize) -> Option<usize> {
    hit.iter().position(|&d| d as usize == want)
}

fn main() {
    let clock = timer::Clock::new();
    let path = std::env::var("INDEX_BENCH_PLACE")
        .unwrap_or_else(|_| "bench/fixture/maphy-place.txt".to_string());
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("maphy-place: no fixture at {path} — see bench/fixture/README.md");
        return;
    };

    let mut place: Vec<Place> = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 4 || f[1].is_empty() {
            continue;
        }
        place.push(Place {
            level: f[0].to_string(),
            name: f[1].to_string(),
            parent: f[3].to_string(),
        });
    }
    if place.is_empty() {
        eprintln!("maphy-place: fixture is empty");
        return;
    }

    // Two fields, mirroring what the UI shows: the place name, and its parent for disambiguation
    // ("San Isidro" exists in dozens of provinces). The parent is weak — it disambiguates, it does
    // not make a place match.
    let spread = prior_spread();
    let schema = Schema::new(vec![Field::new("name", 4.0, 0.5), Field::new("parent", 0.5, 0.75)]);
    let mut b = IndexBuilder::new(schema);
    for p in &place {
        // The static prior maphy encodes in `LEVEL_RANK` and BM25F previously could not express.
        // Same numbers, same meaning: a region outranks a province outranks a municipality.
        b.add_with_prior(
            &Doc::new([p.name.as_str(), p.parent.as_str()]),
            1.0 + spread * PlaceBaseline::rank_of(&p.level) as f32,
        );
    }
    let build_t = std::time::Instant::now();
    let ix = b.build().expect("build");
    let build_ms = build_t.elapsed().as_secs_f64() * 1000.0;
    let baseline = PlaceBaseline::new(&place);

    println!("maphy-place :: maphy's place search, on maphy's own data");
    println!("clock backend: {}", clock.backend());
    println!(
        "  {} places (17 regions, 84 provinces, 966 municipalities), index built in {:.0}ms",
        place.len(),
        build_ms
    );
    println!("  NOTE: barangay.json (~42k more) is not on disk, so this is the FIRST-LOAD pool.");

    // --- 1. exact name: can a place be found by typing it?
    //
    // Scored against the SET of places sharing that exact name, not against one chosen index.
    // 137 of 1,067 entries (12.8 %) are duplicate names — "Quezon" names six municipalities and
    // "San Isidro" six more — so demanding one specific row at rank 1 is demanding the impossible,
    // and would have scored a perfect ranker at 87 %. Measuring it that way first produced a
    // spurious FAIL at 91.8 %, which is almost exactly 100 % minus the duplicate rate.
    let mut by_name: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (i, p) in place.iter().enumerate() {
        by_name.entry(normalize(&p.name)).or_default().push(i);
    }
    let dup_entry: usize = by_name.values().filter(|v| v.len() > 1).map(|v| v.len()).sum();

    let mut e_exact = Metric::default();
    let mut b_exact = Metric::default();
    for p in place.iter() {
        let same = &by_name[&normalize(&p.name)];
        e_exact.observe(
            ix.search(&p.name, 10).iter().position(|h| same.contains(&(h.doc as usize))),
        );
        b_exact.observe(
            baseline.search(&p.name, 10).iter().position(|&d| same.contains(&(d as usize))),
        );
    }

    // --- 2. typeahead: the first few characters, which is what a keystroke actually sends.
    // A 5-character prefix is only a fair question when fewer than `k` places share it.
    //
    // 40 municipalities are named "CITY OF ...", so all 40 issue the IDENTICAL query "CITY ", and
    // all 17 regions issue "REGIO". No ranker can put 40 documents in 10 slots, so those queries
    // measure which arbitrary ten a tie-break happens to pick, not retrieval quality. Scoring them
    // is what produced the engine's apparent 85.2 % vs maphy's 90.4 % -- a difference entirely
    // inside a set of unanswerable queries. Colliding prefixes are counted and reported instead.
    let mut share: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in place.iter() {
        let n: String = normalize(&p.name).chars().take(5).collect();
        *share.entry(n).or_default() += 1;
    }

    let mut miss: Vec<(String, String, String, usize)> = Vec::new();
    let mut collide = 0usize;
    let mut e_pre = Metric::default();
    let mut b_pre = Metric::default();
    for (i, p) in place.iter().enumerate() {
        let n: String = p.name.chars().take(5).collect();
        if n.chars().count() < 5 {
            continue;
        }
        // Keyed exactly as the map was built: 5 chars of the NORMALIZED full name. Re-normalizing
        // the 5-char slice instead trims its trailing space, so "city " never matched "city" and
        // the filter silently did nothing.
        let key: String = normalize(&p.name).chars().take(5).collect();
        if share.get(&key).copied().unwrap_or(0) > 10 {
            collide += 1;
            continue;
        }
        let er = rank_of(&ix.search_prefix(&n, 10), i);
        let br = rank_of_id(&baseline.search(&n, 10), i);
        e_pre.observe(er);
        b_pre.observe(br);
        if let (None, Some(r)) = (er, br) {
            if miss.len() < 10 {
                miss.push((n.clone(), p.name.clone(), p.level.clone(), r));
            }
        }
    }

    // --- 3. THE POINT: a single-character typo.
    let mut e_typo = Metric::default();
    let mut b_typo = Metric::default();
    let mut example: Vec<(String, String, bool, bool)> = Vec::new();
    for (i, p) in place.iter().enumerate() {
        for kind in 0..4 {
            let q = typo(&p.name, kind);
            if q == p.name {
                continue;
            }
            let er = rank_of(&ix.search(&q, 10), i);
            let br = rank_of_id(&baseline.search(&q, 10), i);
            e_typo.observe(er);
            b_typo.observe(br);
            if example.len() < 6 && kind == 1 && br.is_none() && er == Some(0) {
                example.push((p.name.clone(), q, true, false));
            }
        }
    }

    println!(
        "  {} of {} entries share a name with another place ({}), so exact-name rank 1 is\n  \
         scored against the SET of same-named places rather than one chosen row.\n",
        dup_entry,
        place.len(),
        pct(dup_entry as f64 / place.len() as f64)
    );

    println!("  --- accuracy ---");
    println!(
        "  {:<28} {:>10} {:>10}   {:>10} {:>10}",
        "query class", "engine@1", "engine@10", "maphy@1", "maphy@10"
    );
    for (label, e, b) in [
        ("exact place name", &e_exact, &b_exact),
        ("typeahead (first 5 chars)", &e_pre, &b_pre),
        ("ONE-CHARACTER TYPO", &e_typo, &b_typo),
    ] {
        println!(
            "  {:<28} {:>10} {:>10}   {:>10} {:>10}",
            label,
            pct(e.hit1()),
            pct(e.hit10()),
            pct(b.hit1()),
            pct(b.hit10())
        );
    }
    if !miss.is_empty() {
        println!("\n  typeahead: found by maphy, missed by the engine:");
        for (q, name, level, r) in &miss {
            println!("    \"{q}\" -> {name} ({level}), maphy rank {}", r + 1);
        }
    }

    println!(
        "\n  {collide} of {} typeahead queries were EXCLUDED as unanswerable: more than 10 places\n  \
         share their 5-character prefix (40 are named \"CITY OF ...\", 17 begin \"REGION\"), so no\n  \
         ranker can return the right one in a top-10. Scoring them measures a tie-break, not\n  \
         retrieval, and doing so is what made an earlier version of this bench report a spurious\n  \
         engine loss on this row.",
        place.len()
    );

    println!(
        "\n  maphy returns an EMPTY list for {} of {} typo queries ({}).",
        b_typo.empty,
        b_typo.n,
        pct(b_typo.empty as f64 / b_typo.n.max(1) as f64)
    );
    if !example.is_empty() {
        println!("  found by the engine at rank 1, not found at all by maphy:");
        for (name, q, _, _) in &example {
            println!("    typed \"{q}\"  ->  {name}");
        }
    }

    // --- 4. latency, per keystroke.
    let query: Vec<String> = place
        .iter()
        .map(|p| p.name.chars().take(5).collect::<String>())
        .filter(|s| s.chars().count() == 5)
        .collect();
    let mut e_ns = clock.time_each(query.len(), |i| ix.search_prefix(&query[i], 24).len() as u64);
    let mut b_ns = clock.time_each(query.len(), |i| baseline.search(&query[i], 24).len() as u64);
    e_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    b_ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (e50, e99) = (timer::percentile(&e_ns, 0.50), timer::percentile(&e_ns, 0.99));
    let (b50, b99) = (timer::percentile(&b_ns, 0.50), timer::percentile(&b_ns, 0.99));

    println!("\n  --- latency per keystroke ({} queries) ---", query.len());
    println!("  {:<28} {:>12} {:>12}", "", "p50", "p99");
    println!("  {:<28} {:>10.1}us {:>10.1}us", "engine", e50 / 1000.0, e99 / 1000.0);
    println!("  {:<28} {:>10.1}us {:>10.1}us", "maphy (linear scan)", b50 / 1000.0, b99 / 1000.0);

    println!("\n  --- gate ---");
    let ok_exact = e_exact.hit1() >= EXACT_HIT1_MIN;
    let ok_typo = e_typo.hit10() >= TYPO_HIT10_MIN;
    let ok_p99 = e99 <= P99_NS_MAX;
    println!(
        "  {} engine finds an exact name at rank 1 >= {}  (got {})",
        verdict(ok_exact),
        pct(EXACT_HIT1_MIN),
        pct(e_exact.hit1())
    );
    println!(
        "  {} engine survives a one-character typo @10 >= {}  (got {})",
        verdict(ok_typo),
        pct(TYPO_HIT10_MIN),
        pct(e_typo.hit10())
    );
    println!(
        "  {} engine p99 <= {:.1}ms  (got {:.3}ms)",
        verdict(ok_p99),
        P99_NS_MAX / 1e6,
        e99 / 1e6
    );

    println!(
        "\n  Read: this pool is the one that loads FIRST. maphy's shipped worst case adds ~42,000\n  \
         barangays, where a per-keystroke linear scan costs ~40x what it costs here and the\n  \
         engine's cost is bounded by the postings it touches rather than by the pool."
    );
}
