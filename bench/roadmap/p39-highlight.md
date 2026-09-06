# P39 — highlighting: showing why a result matched

**Tier:** T1 · **API:** `Index::highlight` / `Searcher::highlight` / `idx_highlight`
**Status: SHIPPED, 2026-09-06. ABI 7 → 8, 42 symbols. Rust, JavaScript and Python.**

Every shopping result shows *why* it matched — bolded terms, a matching fragment. There was no API
to produce that, and it was one of the two things named as blocking a real deployment.

## The shape: the caller passes the text back

The index does not store field text. So highlighting cannot be "give me the snippet for doc 7"; it
is **"here is the row I already have — which parts of it matched?"**

```rust
pub fn highlight(&self, query: &str, text: &str) -> Vec<(usize, usize)>
```

That is the right shape for an embedded engine rather than a concession. The application owns the
rows; keeping a second copy purely to highlight them is exactly the duplication this engine exists
to avoid, and it is what every hosted search product charges for.

**Spans are computed through the same analysis the index used**, which is the whole point: a token
matches when its term id is one the query *planned*, including every typo expansion the search
itself considered. So `highlight("Colgte", "Colgate Total Toothpaste")` marks **Colgate**. An
alias-rewritten token marks the text that was actually written.

## Byte offsets, and why folding made that hard

`fold()` is not length-preserving — a diacritic is dropped, `ß` lowercases to two characters — so an
offset into the folded string says nothing about the caller's text. Two additions:

- **`fold_with_origin`** returns the folded string *plus the original byte offset each folded
  character came from*. Kept separate from `fold`, which runs on every field of every document at
  build time and must not allocate a second vector to serve a query-time feature.
- **`tokenize_span`** produces the same tokens as `tokenize` plus each token's `[start, end)` in the
  original text, including extending the span across a quantity merge so `500` + `ml` underlines
  `500 ml` as one.

**Two implementations of the tokenizing rules can drift, and a highlight that disagrees with the
index marks the wrong words.** `tokenize_span_matches_tokenize` pins them together over thirteen
inputs chosen for where the rules bend: empty, whitespace, `1.5L`, `MATH 30-23`, combining marks,
`ß`, `a,b.c-d`, a bare digit. `fold_agrees_with_fold_with_origin` does the same for the folder.

## Tested

Rust (119 total), and the same scenario through the real ABI from both hosts:

```
PASS  highlight marks the matching word
PASS  a typo highlights the corrected word
PASS  multiple spans, in order
PASS  a term absent from this text marks nothing
PASS  byte offsets stay correct with multi-byte text before the match
```

Also asserted: spans are ascending and non-overlapping so a caller can wrap each one without
producing `<b>Col</b><b>gate</b>`; highlighting text the index never saw returns empty rather than
panicking; and every span lands on a UTF-8 character boundary.

## A use-after-free I wrote, and how it surfaced

The first draft of the JavaScript check reused `fh`, a handle **closed 25 lines earlier**. Calling
into a freed handle is a use-after-free inside the module, and it did not trap — `node` simply
produced no output and hung until the command timed out.

That is the failure mode this ABI's error model is designed to avoid everywhere else: bad input
returns a sentinel, it does not trap. A freed *handle* is outside what the module can check, because
the pointer is the host's to keep valid. **It is the one class of mistake a host can still make**,
and the C header says so; this is the first time it was made here. The block now builds its own
index and closes it.

## Honest limits

- **Spans, not snippets.** No windowing, no ellipsis, no "…matched text…" extraction. A caller with
  a 2 KB description gets offsets and has to decide what window to show.
- **Byte offsets, not UTF-16.** A JavaScript host must slice the encoded bytes, not the string —
  `String.prototype.slice` is wrong for anything outside the BMP. The Python wrapper returns decoded
  fragments instead of offsets for exactly this reason; the JavaScript one does not, and should.
- **No field awareness.** `highlight` takes text, not a field index, so a caller highlighting three
  fields calls it three times and the query is planned three times.
- **`Searcher::highlight` needs the global ordinal** to pick the right segment's dictionary, because
  typo expansion resolves against the vocabulary that decided the hit.
- **Not measured.** Cost is one query plan plus one tokenization of the passed text; that is
  obviously small and has not been benchmarked.

## Reproduce

```sh
cargo test -p index-text highlight
cargo test -p index-text tokenize_span
node js/smoke.mjs
python host/python/index_ffi.py
```
