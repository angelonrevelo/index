# P45 — phrase queries

**Tier:** T1 · **Bin:** `phrase-cost` · **API:** `IndexBuilder::with_position`, `Index::search_phrase`
**Status: SHIPPED, 2026-09-06. ABI 10 -> 11, 49 symbols. Format IDXTEXT5 -> IDXTEXT6. 132 tests.**

The oldest open item in the file. `p43` named it while refactoring around it:

> ... because phrase support is next and would have made it nine.

It did not make it nine. `Scan` absorbed it as one more field, which is the second time that struct
has paid for itself.

## The engine had no positions at all

Not a gap in the query layer — a gap in the format. A posting is deliberately **eight bytes**
(`doc: u32`, `sat: f32`) with the per-field term frequencies moved out, because retrieval at a
million documents is memory-bound and inline frequencies halved the postings per cache line. There
was nowhere for a token offset to live, and no cheap way to make one.

So positions are a **separate, opt-in section**, and the default costs exactly zero bytes —
asserted, not asserted-in-prose (`an_index_without_positions_costs_no_position_bytes`).

## The field is packed into the position, and that is the whole field-boundary rule

```
position = field << 16 | token_index
```

Consecutive positions can only be adjacent if they share a field, so `"Ice Cream"` **cannot** match
a row whose name ends in *Ice* and whose brand begins with *Cream*. There is no separate check for
that; it falls out of the packing. `doc_len` is already `u16`, so sixteen bits is not a new limit.

The test corpus is built so a bag-of-words search **cannot tell the cases apart** — all four rows
contain both words, and only adjacency separates them:

| row | `search "Ice Cream"` | `search_phrase "Ice Cream"` |
|---|---|---|
| `Vanilla Ice Cream Tub` | hit | **hit** |
| `Ice Crushed Cream Soda` | hit | — words apart |
| `Cream Ice Bar` | hit | — wrong order |
| name `Chocolate Ice`, brand `Cream Co` | hit | — **crosses a field** |

## Three decisions, each the safe direction of a rule this repo already has

**A phrase is exact.** No token is typo-corrected, prefix-expanded or compound-split. Quoting is the
user asserting *these words*; correcting inside a phrase answers a different question than the one
asked. A token absent from the dictionary therefore matches **nothing** — the same direction as an
include clause whose values are all unknown (`p43`): a constraint nobody can satisfy is empty, never
everything.

**An index without positions returns nothing, rather than falling back.** A fallback to a term
search would return bag-of-words rows that are indistinguishable from phrase rows once they are in
the result buffer. There is no way for a host to detect that it happened, which makes it worse than
an empty answer. Asserted in Rust, through the ABI, and from both hosts.

**The phrase is a filter, not a ranking signal.** It is applied after scoring and before admission,
at exactly the two sites the facet clause uses, so it touches no pruning bound — a phrase hit
carries the score the same query would have given it. `search_phrase` is deliberately *not* a
different ranker.

## What it costs, measured on 25,979 real rows

`phrase-cost` builds the blead fixture twice from identical rows, differing only in positions:

| | bytes |
|---|---|
| without positions | 2,378,890 |
| **with positions** | **4,156,942** (**+74.7 %**) |
| delta | 1,778,052 |

**And the delta is not where it was expected.** 140,640 token occurrences at one `u32` each is
~563 KB. The other **~1.2 MB is the offset array** — one `u64` per posting, and there are ~152,000
postings. **The addressing costs 2.2x the data it addresses**, because most (term, document) pairs
carry exactly one position, so an eight-byte offset points at four bytes of payload.

That is the honest headline: positions on short-name corpora are dominated by their index, not by
themselves. Delta-and-varint encoding, or eliding the offset for single-occurrence postings, is the
obvious next move and is a pure win with no API change. Left undone rather than guessed at.

Query cost, same corpus, 184 phrases drawn from the corpus itself:

| | p50 | p99 |
|---|---|---|
| `search` | 10.4 us | 149.7 us |
| **`search_phrase`** | **10.4 us** | **312.9 us** |

**The median is identical and the tail roughly doubles**, which is the predicted shape: verification
runs only on documents that already scored, and its cost is `occurrences of the phrase's rarest term
x phrase length` — anchoring on the rarest rather than the first term is what keeps `"the toothpaste"`
from walking every occurrence of *the*.

## The correctness arm shares no code with the engine

`p11` left this project its sharpest methodological lesson:

> **two agreeing implementations that share an input parser agree about the parser, not about the
> answer.**

So the reference here does not use the index. It normalizes the **raw fixture line** and looks for
the phrase as a word-boundary substring — no tokenizer, no dictionary, no postings, no verifier.
It is *cruder* than the engine (it knows nothing about aliases or folding), so it is used **one-way**,
in the direction where crudeness cannot raise a false alarm: every document the engine returns must
literally contain the phrase. The reverse direction is reported as analyzer drift, never asserted.

**980 hits over 184 real phrases: 0 hits that did not contain the phrase, 0 phrases returning
nothing, 0 drift.**

## The format bump, and the assertion that forced it

`IDXTEXT5` -> `IDXTEXT6`, section table 15 spans -> 17. `every_offset_in_the_table_is_u64` failed
the moment the spans were added, which is the third time that assertion has forced a magic bump
rather than letting the format grow silently.

The reader validates `position_at` hard — one entry per posting plus a terminator, non-decreasing,
in bounds, ending exactly at the position count — because **every failure here is silent**. An
offset array one entry short makes `position_of` hand back the *next* posting's run, and the
verifier agrees with whatever it is given. `a_truncated_position_offset_array_is_refused` shortens
the span by exactly one `u64` and requires an error.

## Still open

- **The offset array dominates the cost** (above). Varint/delta encoding is the fix.
- **No quoted-substring query syntax.** `search_phrase(q, k)` treats the *whole* query as the
  phrase; there is no `red "ice cream"` mixed query. That needs a query parser, which this engine
  has deliberately never had, and is a larger decision than a phrase verifier.
- **Positions are all-or-nothing across fields.** There is no way to store them for the title and
  not for the description, which is what a prose corpus would want.
