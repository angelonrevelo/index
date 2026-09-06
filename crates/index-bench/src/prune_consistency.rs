//! `prune-consistency` — **is the pool defect expansion-specific, or has it always been there?**
//!
//! `bench/roadmap/p21-pool-eviction.md` found that learned expansion could lose correct results,
//! worked around it by widening the candidate pool, and then named the underlying defect:
//!
//! > **The engine prunes by score and ranks by `typo_bucket`-then-score. Those two disagree.**
//!
//! That diagnosis makes a prediction bigger than expansion: **any** query where a perfect-bucket
//! document scores low, while many worse-bucket documents score high, should lose the good document
//! — expansion or not. If that is true, the defect predates `learn_expansion` by however long
//! block-max MaxScore has been in the engine, and affects ordinary typo queries.
//!
//! `p21` said the differential test should be written before the fix is attempted. This is it.
//!
//! # Why this is a bench and not a unit test
//!
//! It is **expected to fail** while the defect stands. `bench/roadmap/` is where this repo keeps
//! specs for unfinished work — a roadmap's red tests are a feature, a gate's red tests are a bug —
//! so it lives here and is excluded from CI, exactly like the other roadmap bins.
//!
//! # Ground truth
//!
//! Neither `search` nor `search_exhaustive` can serve as the reference: **both apply the same pool**
//! (`search_exhaustive` says so — *"Mirror `search`'s pool semantics exactly"*). So the reference is
//! computed here by brute force: score every document, sort by the engine's own comparator, take
//! the top k. No pool, no pruning, no skipping.

mod timer;

use index_text::{Doc, Field, Index, IndexBuilder, Schema};

/// A corpus built to make the inconsistency visible.
///
/// - **one** document matches the whole query but is long, so BM25 length-normalizes its score down;
/// - **many** documents match only part of the query but are short and repeat the common term, so
///   they score high while sitting in a worse `typo_bucket`.
///
/// The ranking rule says the first document wins. The pruning rule never lets it into the pool.
fn corpus(decoys: usize) -> (Index, usize) {
    let schema = Schema::new(vec![Field::new("text", 1.0, 0.75)]);
    let mut b = IndexBuilder::new(schema);
    let pad = (0..150).map(|i| format!("filler{i}")).collect::<Vec<_>>().join(" ");

    // Two earlier constructions failed for the SAME reason, and it is the finding:
    //
    //   1. `beta` in one document  -> maximal IDF -> the "low-scoring" full match scored high.
    //   2. `beta` in 400 documents -> still far rarer than `alpha` -> same outcome.
    //
    // To be bucket 0 a document must match every group, and the group the decoys MISS is precisely
    // the one carrying the IDF — matching it is what makes the full match score well. So the third
    // term is made genuinely COMMON, present in thousands of unrelated documents, leaving the full
    // match almost nothing to gain from it while the decoys stay short.
    for i in 0..3000 {
        b.add(&Doc::new([format!("gamma noise{i}").as_str()]));
    }

    // Full matches: all three groups, long. Bucket 0, score dragged down by length.
    let target_id = 3000usize;
    for _ in 0..200 {
        b.add(&Doc::new([format!("alpha beta gamma {pad}").as_str()]));
    }

    // Decoys: short, missing only the low-IDF `gamma`. Worse bucket, much higher score.
    for _ in 0..decoys {
        b.add(&Doc::new(["alpha beta"]));
    }
    (b.build().expect("build"), target_id)
}

/// Brute force: score every document, order by the engine's comparator, take the top `k`.
fn truth(ix: &Index, query: &str, k: usize) -> Vec<u32> {
    let mut hit = ix.search_exhaustive_unpooled(query, ix.doc_count());
    hit.truncate(k);
    hit.iter().map(|h| h.doc).collect()
}

fn main() {
    let clock = timer::Clock::new();
    println!("prune-consistency :: does pruning agree with ranking?");
    println!("clock backend: {}", clock.backend());
    println!(
        "  A corpus where the ONLY full match is long (low score, bucket 0) and the decoys are\n  \
         short term-stuffed partial matches (high score, worse bucket). No expansion involved.\n"
    );

    println!("  {:>8} {:>14} {:>16} {:>10}", "decoys", "truth rank 1", "search() rank 1", "agree");
    let mut first_bad = None;
    for decoys in [4usize, 16, 32, 64, 128, 512, 2048] {
        let (ix, target) = corpus(decoys);
        let t = truth(&ix, "alpha beta gamma", 10);
        let got: Vec<u32> = ix.search("alpha beta gamma", 10).iter().map(|h| h.doc).collect();
        let t1 = t.first().copied();
        let g1 = got.first().copied();
        let agree = t1 == g1;
        if !agree && first_bad.is_none() {
            first_bad = Some(decoys);
        }
        println!(
            "  {:>8} {:>14} {:>16} {:>10}",
            decoys,
            t1.map(|d| d.to_string()).unwrap_or("-".into()),
            g1.map(|d| d.to_string()).unwrap_or("-".into()),
            if agree { "yes" } else { "NO" }
        );
        let _ = target;
    }

    println!("\n  --- verdict ---");
    match first_bad {
        Some(n) => {
            println!(
                "  FAIL  pruning and ranking disagree from {n} decoys onward, with NO expansion in\n  \
                 play. The defect is GENERAL: it predates `learn_expansion` and affects any query\n  \
                 where a perfect-bucket document scores below the pool's worst score.\n\n  \
                 `p21`'s pool multiplier only hides it for expanded queries. Ordinary typo queries\n  \
                 have the same shape whenever an exact match is long and the near-misses are short."
            );
        }
        None => {
            println!(
                "  PASS  no disagreement at any decoy count. All THREE pruning sites are now\n  \
                 gated: the block skip and the non-essential bail on `prune_is_sound` (`p24`),\n  \
                 and the essential/non-essential partition on the bucket floor (`p25`).\n  \
                 `pool-audit` reads 0.00 % on all six real query sets, so for the first time\n  \
                 green here and green there mean the same thing."
            );
        }
    }
}
