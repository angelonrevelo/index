// Literal-first, fuzzy-fallback: profstopick's matcher and `index`, merged.
//
// # Why mix them at all
//
// The two are complementary in a way that is structural rather than incidental, and neither can
// absorb the other by tuning:
//
//   - **Their matcher does INFIX, `index` cannot.** `gracia` must reach `DIVINAGRACIA, GERALD G.`
//     A term dictionary stores `divinagracia` as one token; `gracia` is neither a prefix of it nor
//     within edit distance of it, so an FST walk can never arrive there. It offers `GARCIA` at
//     distance 1 instead -- a reasonable guess, and the wrong professor.
//   - **`index` does TYPOS, their matcher cannot.** `ABACNA` and `ABAAN` require the typed
//     characters NOT to appear in order, which is precisely what every one of their four routes
//     requires. Measured in their own production instrumentation: 109 of 267 real searches returned
//     nothing, 60 of them name-shaped, every one resolving to a professor already in the corpus.
//
// # The merge rule, and why this order
//
// Literal matches rank above fuzzy ones, always. A character the user actually typed is evidence;
// an edit the engine guessed is inference, and inference must never displace evidence. This is the
// same rule `index` applies internally -- `typo_bucket` is its PRIMARY sort key, ahead of BM25 --
// so the hybrid is applying the engine's own principle one level up.
//
// Within each group the source's own ranking is preserved untouched: their tier order, then
// `index`'s bucket-then-BM25 order. Neither is re-scored, because re-scoring across two systems
// whose scores mean different things is how a blended list becomes worse than both inputs.
//
// # Cost
//
// One extra pass over an already-folded array, plus one WASM call. Their matcher is the expensive
// half at this corpus size and it is the half that already runs today, so the marginal cost of
// adding `index` is a single `idx_search` -- tens of microseconds.

/**
 * @param {(q: string, k: number) => {label: string, tier: number}[]} literal
 *   The literal matcher -- `matchFolded` bound to a folded corpus.
 * @param {{search: (q: string, o: object) => {label: string, typoBucket: number}[]}} engine
 *   An open `SearchIndex`.
 */
export function makeHybrid(literal, engine) {
  /**
   * @param {string} query
   * @param {number} k
   * @returns {{label: string, from: 'literal'|'fuzzy', tier?: number, bucket?: number}[]}
   */
  return function hybrid(query, k = 10) {
    if (query.trim() === '') return [];
    const seen = new Set();
    const out = [];

    for (const r of literal(query, k)) {
      if (seen.has(r.label)) continue;
      seen.add(r.label);
      out.push({ label: r.label, from: 'literal', tier: r.tier });
    }

    // `prefix: true` is not optional here. Without it the last token is matched as a COMPLETE term,
    // so a half-typed query -- which is every query, until the user stops typing -- finds nothing:
    // `pen` returns 0 rather than 6, `gar` 0 rather than 10, `cru` 0 rather than 10.
    for (const r of engine.search(query, { k, prefix: true })) {
      if (seen.has(r.label)) continue;
      seen.add(r.label);
      out.push({ label: r.label, from: 'fuzzy', bucket: r.typoBucket });
    }

    return out.slice(0, k);
  };
}
