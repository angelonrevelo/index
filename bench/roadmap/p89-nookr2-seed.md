# P89 — nookr2's two comboboxes: the separator trap is in the categories, not the members

**Tier:** T1 · **Check:** `cargo run -p index-bench --release --bin nookr2-seed`
· **Files:** `crates/index-bench/src/nookr2_seed.rs`
**Status: MEASURED, 2026-09-13. The fifth and last p84 candidate with data on disk, and the first
consumer of the `--sql` dump reader outside the CLI itself: the corpus is nookr2's own
`supabase/seed.sql`, parsed by `SqlDumpReader`. On the category combobox their predicate returns
NOTHING for 7 of 26 natural queries and 23 of 26 corrupted ones; the member combobox survives
natural input at this seed and collapses only on typos (0/7). Engine: 100 % top-3 on every
family.**

## The consumer

nookr2 filters both of its comboboxes with the estate's most common predicate
(`MemberSearchCombobox.tsx`, `CategoryCombobox.tsx`, and an inline copy in `MembersPage.tsx`):

```ts
c.name.toLowerCase().includes(search.toLowerCase()) ||
c.code.toLowerCase().includes(search.toLowerCase())
```

The seed holds 7 profiles (full_name + email) and 12 income + 14 expense categories (name +
code) — hobbycat scale, and this document states the size verdict up front rather than
discovering it late: **at 33 rows there is no latency or scale story, and none is claimed.**
What IS measurable at any size is the defect class `p86` (trin's apostrophe) and `p87`
(wheresthefx's `7-Eleven`) already exposed on their own data: **the stored form carries
separators the typed form does not.** nookr2's categories are full of them — `'Salaries & Wages'`,
`'Electricity (MERALCO)'`, `'Contribution/Donation'`, and codes like `CAR_STICKER` or
`POOL_RENTAL` that a user types as two words.

## Measured, deterministic queries derived from the seed itself

| family | member combobox (7) | category combobox (26) |
|---|---|---|
| verbatim lowercase — sanity, both must sweep | theirs 7/7 · engine 7/7 | theirs 26/26 · engine 26/26 |
| **natural typing** (separators folded: `salaries wages`, `car sticker`) | **theirs 7/7 — survives** | **theirs 19/26 — 7 zero-results** · engine 26/26 |
| one corrupted letter | theirs 0/7 · engine 7/7 | theirs 3/26 — **23 zero-results** · engine 26/26 |

Engine latency is meaningless here and printed only for completeness (p50 ~1.5 µs). Index: 538 B
for the whole category surface.

Three things worth carrying:

1. **The trap is in ONE surface, and the bench says which.** Member names at this seed carry no
   separators, and an email is pasted rather than re-typed — so their predicate survives natural
   input there, and the bench prints that as a NOTE rather than manufacturing a miss nobody can
   produce (its first draft did exactly that by space-folding emails, which no user types). The
   category surface is where `&`, `/`, parens and underscores live, and there **27 % of natural
   queries about their own seeded rows return an empty dropdown**.
2. **The typo class is the same one every estate repo shows**: one corrupted letter zeroes their
   member combobox entirely and leaves 3 of 26 categories reachable. That is profstopick's
   109/267 finding at combobox scale.
3. **The engine's 100 % comes with no recall cost to police**: top-3 of a 26-row surface, no
   tuning, the same analyzer that serves the other six measured repos.

## The `--sql` reader's first consumer

`index-cli` became a library so that a second consumer would read rows through the SAME code the
tool runs. This bench is that consumer: `SqlDumpReader` parses `seed.sql` — `TRUNCATE`s and
`auth.users` inserts skipped, `OVERRIDING SYSTEM VALUE` and `ON CONFLICT` tails handled, the
three wanted tables filtered by `--table`-style name — and the bench never holds a second parser
that could drift from the tool.

## Still open

- The seed is what is on disk; nookr2's production data lives in Supabase and needs an export
  before scale claims could be made. The bench re-runs against the seed's growth.
- The estate coverage map's p84 candidate list is now fully dispositioned: yclap, hobbycat, trin,
  wheresthefx measured; orsem skipped honestly (no matcher to reproduce); **nookr2 measured
  here**; openbid still needs an export.
