# P41 — exact whole-field match, and the ranking it fixed

**Tier:** T1 · **API:** `INEXACT_FIELD_KEEP`, `Index::exact_field_factor`
**Status: SHIPPED, 2026-09-06. No corpus regressed; two improved; exactness unchanged at 0.00 %.**

`p40` found that a short exact name loses to a longer relative: `session` to `session_resource`,
`user` to `user_agent`, `change_order` to `change_order_line`. BM25 sums term frequency across
fields and cannot see that **the first field is exactly the query** — which is a distinct intent,
and usually the one a developer typing a table name or a shopper typing a product name has.

## The signal

```rust
/// `1.0` when field 0 is exactly the query, `INEXACT_FIELD_KEEP` (0.9) otherwise.
fn exact_field_factor(&self, doc: u32, bucket: u32, group_count: usize) -> f32
```

"Exactly" means **the same token count in field 0** *and* **every query group matched**, which is
what `bucket == 0` already asserts. Both halves are needed: token count alone would fire on a
same-length document that matched nothing.

**A demotion of the inexact, never a boost of the exact.** This is the third signal in this engine
shaped that way, after static priors and prefix anchoring, and always for the same reason:
`max_score` bounds are computed assuming a factor of 1.0, so any factor in `(0, 1]` leaves every
pruning bound a valid upper bound **by construction**. A boost above 1.0 would silently invalidate
MaxScore and the failure would appear as missing results, not as an error.

Deliberately mild at 0.9. It is a tiebreak between comparable matches, not a filter — a demoted
document with a clearly better BM25 score still wins, which keeps an exact-but-worse row from
burying a better one.

Order is **not** checked: `"Toothpaste Colgate"` counts as exact for `"Colgate Toothpaste"`. The
tokenizer is bag-of-words everywhere else and pretending otherwise in one place would be
inconsistent.

## The bug the consistency tests caught

The factor has to be applied at **three** scoring sites: `score_doc`, `search_exhaustive_unpooled`
and `search_exhaustive`. I added it to two and missed the third — the **hot scan loop in
`search_opt`, which scores inline rather than calling `score_doc`.**

`maxscore_agrees_with_exhaustive_or` failed immediately with a score ratio of exactly `10/9`, which
named the missing factor precisely. The inline site also computed `bucket` *after* the score, so the
fix required reordering: the bucket is now computed first, because the exact-field factor depends
on it.

**A ranking signal added to two of three paths is worse than one added to none** — it makes the
oracle disagree with the engine for a reason unrelated to what the oracle is testing. The tests that
exist to guard pruning caught a scoring bug, which is what a good invariant does.

## Measured on every corpus

| bench | before | after |
|---|---|---|
| **`booted-schema` exact name rank-1** | 86.4 % | **100.0 %** |
| **`profstopick-dept` learned expansion** | 98.3 % (**−0.8 pt**, a loss) | **100.0 % (+0.8 pt)** |
| `presyo-catalog` expansion | 97.0 %, +21.8 pt over a 75.3 % baseline | 97.0 %, **+21.0 pt over a 76.0 %** baseline |
| `blead-industry` | 77.4 % → 95.6 % | **unchanged** |
| `pool-audit`, all six query sets | 0.00 % | **0.00 %** |
| `facet-shop` | PASS, 0 wrong everywhere | **PASS** |
| `real-corpus`, `sisia-catalog` | PASS | **PASS** |
| `scale` typo p99 @ 1 M | ~17 ms | **12.3 ms** (still over the 5 ms bar) |

**Nothing regressed. Two things improved, and one of them was a documented loss.** `p20` recorded
that learned expansion made profstopick *worse* by 0.8 points and could not explain why; the exact
field factor removes that loss entirely. The presyo baseline rose from 75.3 % to 76.0 %, which
shrinks the credit expansion gets — the same accounting `p21` had to do, and the direction is
unchanged.

## Honest limits

- **0.9 is chosen, not derived.** No sweep. It was picked to be mild enough not to overturn a clear
  BM25 winner and firm enough to break a tie; both corpora that moved went to 100 %, so the sweep
  has no signal to find here, and a corpus where it matters would be needed to fit it properly.
- **Field 0 only.** A match that is exactly a document's *brand* field, or exactly its category,
  gets nothing. Extending it per-field is straightforward and unmeasured.
- **`doc_len` is `u16`.** A field over 65,535 tokens saturates, so its exactness test is unreliable.
  No corpus here comes close.
- **It changes ranking for every query**, since almost every document is demoted. Relative order is
  unaffected among equally-inexact documents, exactly as with `UNANCHORED_KEEP` — but the absolute
  scores in any previously recorded output are now 10 % lower.
- **`profstopick` reaching 100 % is a saturated result**, and `p20` already warns that a saturated
  corpus cannot test a condition. The improvement is real; the corpus can no longer measure further.

## Reproduce

```sh
cargo run -p index-bench --release --bin booted-schema
cargo run -p index-bench --release --bin profstopick-dept
cargo run -p index-bench --release --bin pool-audit
```
