# P70 — a query parser, and the bug its own test found

**Tier:** T1 · **Files:** `crates/index-text/src/query.rs` (new, 287 lines)
**Status: SHIPPED, 2026-09-06. No index access, no new dependency.**

`p45` shipped phrase queries and named the gap plainly:

> `search_phrase` treats the **whole query** as one phrase, so `red "ice cream"` was inexpressible.

`parse()` now separates bare terms, quoted phrases and `-exclusions` into a clause set the existing
`FacetClause`/phrase machinery already knows how to answer. It is a **pure function over a string**:
no index access, and nothing to configure.

Its own fuzz loop asserts `parse()` never panics over generated input — which matters more here than
in most places, because its input is *whatever someone typed into a search box*.

## The bug, found by the worker's own test

`flush` cleared the negation marker unconditionally. An opening quote also flushes. So this:

```
-"ice cream"
```

lost its marker before the quoted run was read, and was filed as a **required phrase instead of an
exclusion** — the exact inversion of what the user asked for, silently.

The fix: the marker is consumed only by a clause that actually files, and the whitespace arm clears
it separately.

```rust
c if c.is_whitespace() => {
    if in_phrase { buf.push(c); } else {
        flush(&mut q, &mut buf, &mut negated, false);
        // A marker separated from its clause by a space marks nothing: `- ice` is the
        // term `ice`. Cleared HERE rather than inside `flush`, because an opening quote
        // also flushes and the marker must survive that one -- `-"ice cream"`.
        negated = false;
    }
}
```

Two rules that look like one and are not: **a marker touching a quote excludes the phrase; a marker
separated by a space marks nothing.** A single clearing site cannot express both.

## Still open

- **Nothing calls it from the ABI yet.** `parse()` exists and is tested; `idx_searcher_*` still takes
  a pre-split clause set. Wiring it is a small change and has not been made.
- **No field-qualified terms, no OR keyword, no parentheses.** The grammar is deliberately the three
  things a search box actually receives.
