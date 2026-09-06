//! The term dictionary and the typo policy.
//!
//! Built on `tantivy-fst` + `levenshtein_automata` — the exact stack Meilisearch and Tantivy ship,
//! audited at 4.2 M and 4.4 M recent downloads in `docs/research/build-or-buy.md`. **We depend on
//! the automaton and build the policy**, because the policy is where every production engine's
//! actual behaviour lives and no crate provides it.
//!
//! The policy is the convergent production design. Typesense, Meilisearch, Algolia and Lucene
//! arrived at it independently (`docs/research/relevance.md` §7):
//!
//! > **≤ 2 edits hard-capped · length gates at 0–3 → 0, 4–7 → 1, 8+ → 2 · first character
//! > protected · numeric tokens exempt · fired lazily, only when exact match underdelivers.**
//!
//! Measured feasibility for this design is in `bench/roadmap/p5-fuzzy-term-feasibility.md`.

use levenshtein_automata::{Distance, LevenshteinAutomatonBuilder, DFA, SINK_STATE};
use tantivy_fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer};

/// `levenshtein_automata::DFA` does not implement `tantivy_fst::Automaton` — the `fst_automaton`
/// feature targets the upstream `fst` crate, which is a *different type* from `tantivy-fst`'s.
/// Tantivy solves this with the same small wrapper.
struct Dfa(DFA);

impl Automaton for Dfa {
    type State = u32;
    #[inline]
    fn start(&self) -> u32 {
        self.0.initial_state()
    }
    #[inline]
    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }
    #[inline]
    fn can_match(&self, state: &u32) -> bool {
        *state != SINK_STATE
    }
    #[inline]
    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

/// How many edits a term of this length may be matched with.
///
/// The gates are Typesense's (`min_len_1typo = 4`, `min_len_2typo = 7`) rounded to the interval all
/// four production engines agree on. **Capped at 2, always** — Lucene caps `FuzzyQuery` at
/// `maxEdits = 2` because the DFA construction is precomputed per distance and blows up beyond it.
#[inline]
pub fn max_edit_for(term: &str) -> u8 {
    match term.chars().count() {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

/// A term matched by a query token, with the edit distance it was matched at.
///
/// `distance` is the **typo bucket** — it is a ranking input, never a filter. Meilisearch's rule:
/// *"A document matching with 0 typos always ranks above one matching with 1 typo."*
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermMatch {
    /// Ordinal of the term in the dictionary — the postings-list key.
    pub term_id: u32,
    /// Edit distance from the query token. 0 = exact.
    pub distance: u8,
}

/// An immutable, FST-backed term dictionary.
///
/// The FST maps term bytes to a dense `term_id`, so postings can be a flat `Vec` indexed by id.
pub struct TermDict {
    map: Map<Vec<u8>>,
    /// Serialized FST length in bytes — the number the browser byte budget is spent from.
    byte_len: usize,
    term_count: usize,
    /// Prebuilt automata. Construction is the expensive part and is amortized across queries.
    ///
    /// `lev0` exists for **prefix matching under a zero edit budget**. Without it, a short token in
    /// prefix mode fell through to `lev1` and matched a prefix within one edit — so a one-character
    /// token like `"c"` matched essentially every term in the dictionary, since any single letter is
    /// one substitution away from `c`. The length gate has to hold in prefix mode too.
    lev0: LevenshteinAutomatonBuilder,
    lev1: LevenshteinAutomatonBuilder,
    lev2: LevenshteinAutomatonBuilder,
}

impl std::fmt::Debug for TermDict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermDict")
            .field("term_count", &self.term_count)
            .field("byte_len", &self.byte_len)
            .field("bytes_per_term", &self.bytes_per_term())
            .finish()
    }
}

impl TermDict {
    /// Build from a **sorted, unique** term list. `term_id` is the index into that list.
    pub fn build(sorted_unique_term: &[String]) -> Result<Self, String> {
        debug_assert!(
            sorted_unique_term.windows(2).all(|w| w[0] < w[1]),
            "terms must be sorted and unique"
        );
        let mut b = MapBuilder::memory();
        for (i, t) in sorted_unique_term.iter().enumerate() {
            b.insert(t.as_bytes(), i as u64).map_err(|e| format!("fst insert: {e}"))?;
        }
        let bytes = b.into_inner().map_err(|e| format!("fst finish: {e}"))?;
        let byte_len = bytes.len();
        let map = Map::from_bytes(bytes).map_err(|e| format!("fst open: {e}"))?;
        Ok(TermDict {
            map,
            byte_len,
            term_count: sorted_unique_term.len(),
            lev0: LevenshteinAutomatonBuilder::new(0, false),
            // `true` = transpositions cost one edit (Damerau), so `teh` → `the` is distance 1.
            lev1: LevenshteinAutomatonBuilder::new(1, true),
            lev2: LevenshteinAutomatonBuilder::new(2, true),
        })
    }

    /// Rebuild from a previously serialized FST (see `format`).
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        let byte_len = bytes.len();
        let map = Map::from_bytes(bytes).map_err(|e| format!("fst open: {e}"))?;
        let term_count = map.len();
        Ok(TermDict {
            map,
            byte_len,
            term_count,
            lev0: LevenshteinAutomatonBuilder::new(0, false),
            lev1: LevenshteinAutomatonBuilder::new(1, true),
            lev2: LevenshteinAutomatonBuilder::new(2, true),
        })
    }

    /// The serialized FST. This is exactly what `format` writes, so a dictionary survives a round
    /// trip without a rebuild.
    ///
    /// `tantivy-fst` exposes no borrowing accessor for the underlying bytes, so this copies. The
    /// copy happens **only at serialization time**, never on the query path, and the alternative
    /// (holding a second permanent `Vec` beside the `Map`) would double resident dictionary memory
    /// for every index whether or not it is ever serialized.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.map.as_fst().to_vec()
    }

    pub fn term_count(&self) -> usize {
        self.term_count
    }

    /// Serialized size of the dictionary in bytes.
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }

    pub fn bytes_per_term(&self) -> f64 {
        self.byte_len as f64 / self.term_count.max(1) as f64
    }

    /// Exact lookup. This is the 90 % path and must stay effectively free.
    #[inline]
    pub fn exact(&self, term: &str) -> Option<u32> {
        self.map.get(term.as_bytes()).map(|v| v as u32)
    }

    /// Expand a query token to matching terms under the typo policy.
    ///
    /// - A **numeric** token is matched exactly, always. `300g` never reaches `800g`.
    /// - Otherwise the edit budget comes from [`max_edit_for`].
    /// - `prefix` enables typeahead semantics: the term may extend beyond the query. Applied to the
    ///   last token of a query only, by convention.
    ///
    /// Results carry their distance so the caller can rank by typo bucket.
    pub fn expand(&self, token: &str, is_numeric: bool, prefix: bool) -> Vec<TermMatch> {
        // Numeric-token exemption. Non-negotiable: see `analyze::tokenize`.
        if is_numeric {
            return self.exact(token).map(|id| vec![TermMatch { term_id: id, distance: 0 }]).unwrap_or_default();
        }
        let budget = max_edit_for(token);
        if budget == 0 && !prefix {
            return self
                .exact(token)
                .map(|id| vec![TermMatch { term_id: id, distance: 0 }])
                .unwrap_or_default();
        }
        let builder = match budget {
            0 => &self.lev0,
            1 => &self.lev1,
            _ => &self.lev2,
        };
        let dfa = if prefix { builder.build_prefix_dfa(token) } else { builder.build_dfa(token) };

        let mut out = Vec::new();
        let automaton = Dfa(dfa);
        let mut stream = self.map.search(&automaton).into_stream();
        while let Some((key, id)) = stream.next() {
            // Recover the distance by replaying the key through the DFA — the streamer does not
            // surface the accepting state.
            let d = replay_distance(&automaton, key);
            out.push(TermMatch { term_id: id as u32, distance: d });
        }
        out
    }

    /// [`TermDict::expand_lazy`], but also returning each matched term's TEXT.
    ///
    /// The FST stream already yields the key bytes during traversal and the plain expansion throws
    /// them away, so recovering the text costs one allocation per match and no extra traversal.
    ///
    /// **Only the multi-segment path calls this**, because it is the only one that needs to match
    /// terms ACROSS dictionaries — a term id means different things in different segments, so
    /// summing document frequency to a collection-wide figure can only be keyed on the text. A
    /// single-index search never allocates any of these.
    pub fn expand_lazy_text(
        &self,
        token: &str,
        is_numeric: bool,
        prefix: bool,
    ) -> Vec<(TermMatch, String)> {
        if !prefix {
            if let Some(id) = self.exact(token) {
                // The text is the token itself; no stream, no search.
                return vec![(TermMatch { term_id: id, distance: 0 }, token.to_string())];
            }
        }
        if is_numeric {
            return self
                .exact(token)
                .map(|id| vec![(TermMatch { term_id: id, distance: 0 }, token.to_string())])
                .unwrap_or_default();
        }
        let budget = max_edit_for(token);
        if budget == 0 && !prefix {
            return self
                .exact(token)
                .map(|id| vec![(TermMatch { term_id: id, distance: 0 }, token.to_string())])
                .unwrap_or_default();
        }
        let builder = match budget {
            0 => &self.lev0,
            1 => &self.lev1,
            _ => &self.lev2,
        };
        let dfa = if prefix { builder.build_prefix_dfa(token) } else { builder.build_dfa(token) };
        let automaton = Dfa(dfa);
        let mut out = Vec::new();
        let mut stream = self.map.search(&automaton).into_stream();
        while let Some((key, id)) = stream.next() {
            let d = replay_distance(&automaton, key);
            // Terms are UTF-8 by construction (`build` takes `&[String]`), so this cannot fail;
            // the lossy form avoids an unwrap on the hot path regardless.
            let text = String::from_utf8_lossy(key).into_owned();
            out.push((TermMatch { term_id: id as u32, distance: d }, text));
        }
        out
    }

    /// The **lazy** expansion the production engines all use: try exact first, and only pay for
    /// fuzzy when the exact result set underdelivers.
    ///
    /// Typesense calls this `typo_tokens_threshold`: *"if at least N results are not found for a
    /// search term, then Typesense will start looking for typo-corrected variations."*
    pub fn expand_lazy(&self, token: &str, is_numeric: bool, prefix: bool) -> Vec<TermMatch> {
        if !prefix {
            if let Some(id) = self.exact(token) {
                return vec![TermMatch { term_id: id, distance: 0 }];
            }
        }
        self.expand(token, is_numeric, prefix)
    }
}

/// Run `key` through the automaton and read the accepting distance.
#[inline]
fn replay_distance(a: &Dfa, key: &[u8]) -> u8 {
    let mut state = a.start();
    for &b in key {
        state = a.accept(&state, b);
        if !a.can_match(&state) {
            // A prefix DFA can sink after the matched prefix; the match was already established.
            return 0;
        }
    }
    match a.0.distance(state) {
        Distance::Exact(d) => d,
        Distance::AtLeast(d) => d,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(term: &[&str]) -> TermDict {
        let mut v: Vec<String> = term.iter().map(|s| s.to_string()).collect();
        v.sort();
        v.dedup();
        TermDict::build(&v).unwrap()
    }

    #[test]
    fn exact_lookup_round_trips() {
        let d = dict(&["colgate", "nestle", "milo", "bear", "brand"]);
        assert!(d.exact("colgate").is_some());
        assert!(d.exact("nope").is_none());
        assert_eq!(d.term_count(), 5);
    }

    /// The gates, asserted directly against the documented production defaults.
    #[test]
    fn length_gates_match_the_production_consensus() {
        assert_eq!(max_edit_for("abc"), 0, "3 chars: no typos (Typesense/Algolia agree)");
        assert_eq!(max_edit_for("abcd"), 1, "4 chars: 1 typo");
        assert_eq!(max_edit_for("abcdefg"), 1, "7 chars: still 1");
        assert_eq!(max_edit_for("abcdefgh"), 2, "8 chars: 2 typos");
        // And the cap holds no matter how long the term is.
        assert_eq!(max_edit_for(&"a".repeat(64)), 2, "capped at 2, always");
    }

    /// The headline behaviour: real typos from presyo's frozen fixture resolve.
    #[test]
    fn real_typos_resolve() {
        let d = dict(&["colgate", "nescafe", "safeguard", "century", "coca", "cola", "redbull"]);
        for (typo, want) in [
            ("colgaye", "colgate"),   // substitution
            ("nescaffe", "nescafe"),  // insertion
            ("safeguar", "safeguard"), // deletion
            ("centruy", "century"),   // transposition (Damerau)
        ] {
            let m = d.expand(typo, false, false);
            let ids: Vec<u32> = m.iter().map(|x| x.term_id).collect();
            let want_id = d.exact(want).unwrap();
            assert!(ids.contains(&want_id), "expand({typo}) should reach {want}, got {m:?}");
        }
    }

    /// The correctness guard, restated at the dictionary layer.
    #[test]
    fn numeric_tokens_are_never_fuzzy_matched() {
        let d = dict(&["300g", "800g", "500g", "1500ml"]);
        let m = d.expand("300g", true, false);
        assert_eq!(m.len(), 1, "a numeric token must match itself and nothing else: {m:?}");
        assert_eq!(m[0].term_id, d.exact("300g").unwrap());
        assert_eq!(m[0].distance, 0);

        // And prove the guard is load-bearing: without it, the same token IS within edit distance
        // 1 of a different size, which would be a price-comparison correctness bug.
        let unguarded = d.expand("300g", false, false);
        let ids: Vec<u32> = unguarded.iter().map(|x| x.term_id).collect();
        assert!(
            ids.contains(&d.exact("800g").unwrap()),
            "sanity: 300g IS within edit distance of 800g — which is exactly why is_numeric exists"
        );
    }

    #[test]
    fn exact_match_reports_distance_zero() {
        let d = dict(&["colgate", "colgate2", "coldgate"]);
        let m = d.expand("colgate", false, false);
        let exact = m.iter().find(|x| x.term_id == d.exact("colgate").unwrap()).unwrap();
        assert_eq!(exact.distance, 0, "an exact hit must be distance 0 so it wins its typo bucket");
    }

    #[test]
    fn prefix_enables_typeahead() {
        let d = dict(&["colgate", "college", "collect", "milo"]);
        let m = d.expand("colle", false, true);
        let ids: Vec<u32> = m.iter().map(|x| x.term_id).collect();
        assert!(ids.contains(&d.exact("college").unwrap()));
        assert!(ids.contains(&d.exact("collect").unwrap()));
        assert!(!ids.contains(&d.exact("milo").unwrap()));
    }

    /// A one-character token in prefix mode must match only terms that START with it — not every
    /// term one substitution away from it.
    #[test]
    fn a_short_prefix_does_not_match_everything() {
        let d = dict(&["cruz", "jacob", "precious", "medina", "colgate"]);
        let m = d.expand("c", false, true);
        let ids: Vec<u32> = m.iter().map(|x| x.term_id).collect();
        assert!(ids.contains(&d.exact("cruz").unwrap()));
        assert!(ids.contains(&d.exact("colgate").unwrap()));
        assert!(!ids.contains(&d.exact("jacob").unwrap()), "`c` must not reach `jacob`: {m:?}");
        assert!(!ids.contains(&d.exact("precious").unwrap()), "`c` must not reach `precious`");
        assert_eq!(m.len(), 2, "only the two c-initial terms: {m:?}");
    }

    #[test]
    fn lazy_expansion_short_circuits_on_exact() {
        let d = dict(&["colgate", "colgat", "colgatt"]);
        let m = d.expand_lazy("colgate", false, false);
        assert_eq!(m.len(), 1, "an exact hit must not pay for fuzzy expansion: {m:?}");
        assert_eq!(m[0].distance, 0);
    }

    #[test]
    fn short_tokens_get_no_typo_budget() {
        let d = dict(&["abc", "abd", "abe"]);
        let m = d.expand("abc", false, false);
        assert_eq!(m.len(), 1, "3-char tokens are exact-only, or every short query explodes");
    }

    #[test]
    fn empty_dictionary_is_safe() {
        let d = TermDict::build(&[]).unwrap();
        assert_eq!(d.term_count(), 0);
        assert!(d.exact("anything").is_none());
        assert!(d.expand("anything", false, false).is_empty());
    }
}
