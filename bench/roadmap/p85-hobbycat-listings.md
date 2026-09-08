# P85 — hobbycat's marketplace search, and what an 8-listing corpus decides

**Tier:** T1 · **Check:** `cargo run -p index-bench --release --bin hobbycat-listings` · **Files:** `crates/index-bench/src/hobbycat_listings.rs`
**Status: MEASURED, 2026-09-08. The engine beats the LIKE matcher on every corrupted term and
matches it on clean ones — and the corpus is 8 rows, so the verdict is the polkadoc one: measured
need absent, re-runnable the day it stops being absent.**

## The consumer

hobbycat's marketplace searches active listings through one SQL predicate (`src/db.rs`,
`list_cards`), bound to the user's single term:

> `AND (l.title LIKE ? OR l.summary LIKE ? OR l.description LIKE ?)` — each bound to `%<q>%`

Case-insensitive substring per column, OR'd. The engine indexes the same three text columns
(title boost 3). The corpus is exported from `db/hobbycat.db` with the recipe below and is
deliberately NOT vendored, like `real-million`'s input.

## Measured

| query family | engine | LIKE matcher |
|---|---|---|
| clean title terms (33) | top-50 hit **97.0 %** | 97.0 % |
| one corrupted letter (32) | top-50 hit **65.6 %** | 15.6 % — **27 of 32 queries return NOTHING** |
| latency | p50 1.3 µs, p99 2.7 µs | ~1 µs (8-row scan) |

**OVERALL: PASS** on the comparison gates; the third line is the verdict.

## The size is the finding

**8 active listings, 613 bytes of text, 74 dictionary terms.** A scan of 8 rows costs a
microsecond, so neither matcher has a latency story; and with 74 terms the dictionary is too small
to correct every corruption (the engine recovers 65.6 %, not 99 % — a corrupted term with no close
neighbour in a 74-term vocabulary has nothing to correct to). This is the `polkadoc` verdict with
fresh data: the engine would work today — 2 KB of index, typo recall where the LIKE predicate has
none — but there is no measured pain to close, and this project's governing rule is that a repo
adopts on a measurement it already takes, not on a capability looking for a workload.

What makes this different from a shrug is that **the bench is re-runnable against growth**: the
same binary on a 5,000-listing export answers whether the corpus outgrew the scan, which is the
only condition under which the adoption decision changes.

## Reproducing

```sh
python - <<'PY'
import sqlite3, csv
con = sqlite3.connect('file:C:/Users/maran/Code/hobbycat/db/hobbycat.db?mode=ro', uri=True)
rows = con.execute("""
SELECT l.listing_code, l.title, coalesce(l.summary,''), coalesce(l.description,''),
       coalesce(h.hobby_code,''), coalesce(l.price_amount,0)
FROM listing l LEFT JOIN hobby h ON h.id = l.hobby_id
WHERE l.listing_status='active' AND l.deleted_at IS NULL""").fetchall()
w = csv.writer(open('bench/fixture/hobbycat-listings.csv','w',newline='',encoding='utf-8'))
w.writerow(['listing_code','title','summary','description','hobby_code','price_amount'])
w.writerows(rows)
PY
cargo run -p index-bench --release --bin hobbycat-listings
```
