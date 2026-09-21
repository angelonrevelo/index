//! The query parser — quoted phrases mixed with ordinary terms.
//!
//! `bench/roadmap/p45-phrase.md` shipped phrase search and left exactly this open:
//!
//! > **No quoted-substring query syntax.** `search_phrase(q, k)` treats the *whole* query as the
//! > phrase; there is no `red "ice cream"` mixed query. That needs a query parser, which this
//! > engine has deliberately never had, and is a larger decision than a phrase verifier.
//!
//! It is a larger decision because a query language is a contract with everything downstream: a
//! planner has to decide what a phrase means next to a term, what an exclusion means next to a
//! facet clause, and which of them wins when they disagree. This module is the half of that
//! decision that can be settled without an index: it turns a string into a [`Query`] and touches
//! nothing else — no dictionary, no postings, no `Index`. [`crate::Index::search`] and
//! [`crate::Searcher::search`] consume [`Query`]: unquoted terms stay bag-of-words BM25, each
//! `phrase` is an adjacency filter, `exclude` drops documents that contain those tokens, and a
//! query equal to a live application key is rank-1.
//!
//! # Four rules, each the safe direction of a rule this repo already has
//!
//! 1. **Tokens come from the analyzer, never from a split invented here.** [`parse`] routes every
//!    clause through [`crate::analyze::tokenize`], so `Café` is `cafe` and `1.5 L` is `1500ml` —
//!    the tokens the index actually stored. `analyze.rs` rule 1 is **one fold, used everywhere**,
//!    and its scar is a corpus where a second, slightly different tokenizer left 72 entries
//!    unreachable by the name a student types. A parser that split on whitespace would be that
//!    second tokenizer: every folded or canonicalized word would be unreachable by a quoted
//!    phrase, which is the one construct whose whole point is to be exact.
//! 2. **An unterminated quote degrades; it never errors.** A query arrives one keystroke at a
//!    time, so `red "ice` is not a broken query — it is the ordinary intermediate state of
//!    `red "ice cream"`. The dangling `"` is treated as a literal character and the run it opened
//!    is flushed as ordinary terms: the phrase's own words, with the adjacency requirement
//!    dropped. The alternative — an error, or a panic — would make every phrase query invalid for
//!    the moment it takes to type its closing quote. There is no input this function may reject.
//! 3. **`-` negates only at the start of a clause.** `e-mail`, `3-4` and `Coca-Cola` all contain a
//!    `-`, and none of them means *not mail*, *not 4* or *not cola* — profstopick's `MATH 30-23`
//!    is the corpus case. So the marker counts only where nothing precedes it in the clause: at
//!    the start of the input, after whitespace, or either side of a quote. A `-` with no clause to
//!    negate (`-`, `- ice`) negates nothing; `-"ice cream"` carries onto the run.
//! 4. **A clause that tokenizes to nothing is dropped.** No empty string reaches `term`, and no
//!    empty vector reaches `phrase`. `""`, `"---"`, `-` and `"` all mean nothing. An empty phrase
//!    is a constraint no document can satisfy, and `p45`'s rule for that is **empty, never
//!    everything** — but that rule belongs to the executor, which is the only thing that can
//!    apply it. Emitting `vec![vec![]]` would push an unrepresentable clause into every consumer;
//!    dropping it is the parser's honest answer.
//!
//! # Known limit
//!
//! `exclude` is a flat token list, so a *negated* phrase (`-"ice cream"`) can only contribute its
//! tokens, which excludes documents containing either word rather than the phrase. That is
//! deliberately not silently widened into a positive phrase either — a host that needs negated
//! phrases needs a `Vec<Vec<String>>` beside `exclude`, and adding that field before there is a
//! planner to consume it would be guessing.

use crate::analyze::tokenize;

/// A parsed query. Every token is an **analyzer** token, ready to be resolved against a dictionary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    /// Ordinary (unquoted) tokens, in the order they were typed.
    pub term: Vec<String>,
    /// Each quoted run as its own token sequence, in the order the runs appeared. Order *within*
    /// a run is the phrase; order between runs is not.
    pub phrase: Vec<Vec<String>>,
    /// Tokens a matching document must **not** contain.
    pub exclude: Vec<String>,
}

impl Query {
    /// No positive constraint. An exclude-only parse is empty in this sense, so search returns
    /// nothing rather than everything-minus-those-tokens.
    pub fn is_empty(&self) -> bool {
        self.term.is_empty() && self.phrase.is_empty()
    }

    /// Analyzer tokens the bag-of-words arm scores: unquoted terms plus every phrase token.
    /// Exclude tokens are absent — they filter, they do not rank.
    pub fn scoring_query(&self) -> String {
        let mut out = String::new();
        for t in self.term.iter().chain(self.phrase.iter().flatten()) {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(t);
        }
        out
    }
}

/// Parse a query string into [`Query`].
///
/// Splits on whitespace, except inside a `"` run, and marks a clause with a leading `-` as an
/// exclusion. See the module docs for the four rules that decide the edge cases; the short version
/// is that this function is **total** — every input, however unbalanced, parses to something.
///
/// ```
/// use index_text::query::parse;
///
/// let q = parse("red \"ice cream\" -discontinued");
/// assert_eq!(q.term, vec!["red"]);
/// assert_eq!(q.phrase, vec![vec!["ice", "cream"]]);
/// assert_eq!(q.exclude, vec!["discontinued"]);
/// ```
pub fn parse(query: &str) -> Query {
    let mut q = Query::default();
    walk(query, |clause, token| match clause {
        Clause::Term => q.term.extend(token),
        Clause::Phrase => q.phrase.push(token),
        Clause::Exclude => q.exclude.extend(token),
    });
    q
}

/// The string a **typeahead** scores, and whether its last token is the one still being typed.
///
/// [`Query::scoring_query`] files terms before phrases because order does not matter to a
/// bag-of-words score. In typeahead it matters twice: the engine prefix-expands the LAST token
/// only, and anchors on the FIRST. So this keeps the positive clauses (terms and phrase runs, never
/// exclusions) in the order they were typed, and prefix-expands the last one only when it is an
/// unquoted term:
///
/// - `cruz -sant` → `("cruz", true)`: an exclusion is exact and is skipped, so the term before it
///   is still the word being completed.
/// - `"dela cruz" an` → `("dela cruz an", true)`.
/// - `"dela cr"` → `("dela cr", false)`: a CLOSED quote is exact (`p45`), so nothing in it is
///   expanded, and neither is a term the phrase follows.
/// - `"dela cr` → `("dela cr", true)`: an unclosed quote is ordinary terms (rule 2).
///
/// ```
/// use index_text::query::typeahead_scoring_query;
///
/// assert_eq!(typeahead_scoring_query("cru -santos"), ("cru".to_string(), true));
/// assert_eq!(typeahead_scoring_query("red \"ice cream\""), ("red ice cream".to_string(), false));
/// ```
pub fn typeahead_scoring_query(query: &str) -> (String, bool) {
    let mut out = String::new();
    let mut last_is_term = false;
    walk(query, |clause, token| {
        if clause == Clause::Exclude {
            return;
        }
        for t in token {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&t);
        }
        last_is_term = clause == Clause::Term;
    });
    (out, last_is_term)
}

/// What a flushed clause is. Private: [`Query`] is the public shape; the order clauses arrive in
/// is only needed by [`typeahead_scoring_query`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    Term,
    Phrase,
    Exclude,
}

/// The parse loop, handing each non-empty clause to `sink` in the order it was typed.
fn walk(query: &str, mut sink: impl FnMut(Clause, Vec<String>)) {
    // The clause being accumulated: one unquoted word, or the text between two quotes.
    let mut buf = String::new();
    // True while `buf` is gathering the inside of a quoted run.
    let mut in_phrase = false;
    // True when the clause being accumulated was marked with a leading `-`. It survives an
    // opening quote (rule 3) and is cleared by whatever closes the clause.
    let mut negated = false;

    for c in query.chars() {
        match c {
            // A quote both closes the run being gathered and opens the next one, so the flush is
            // of whatever shape the run *was*; `in_phrase` then flips.
            '"' => {
                flush(&mut sink, &mut buf, &mut negated, in_phrase);
                in_phrase = !in_phrase;
            }
            // Whitespace ends an unquoted clause and is ordinary text inside a quoted run, which
            // is what lets `"ice cream"` be two tokens of one phrase.
            c if c.is_whitespace() => {
                if in_phrase {
                    buf.push(c);
                } else {
                    flush(&mut sink, &mut buf, &mut negated, false);
                    // A marker separated from its clause by a space marks nothing: `- ice` is the
                    // term `ice`. Cleared HERE rather than inside `flush`, because an opening quote
                    // also flushes and the marker must survive that one -- `-"ice cream"`.
                    negated = false;
                }
            }
            // Rule 3: an exclusion marker, only where it starts a clause. `negated` is already
            // set for `--`, and `buf` is non-empty for `e-mail`, so both fall through as text.
            '-' if !in_phrase && buf.is_empty() && !negated => negated = true,
            c => buf.push(c),
        }
    }
    // End of input closes the last clause — as a phrase only if its quote was closed. An
    // unterminated run is flushed as terms (rule 2), which costs nothing here: the flush is
    // already "tokenize what was gathered", and the only thing a phrase adds is adjacency.
    flush(&mut sink, &mut buf, &mut negated, false);
}

/// Tokenize the accumulated clause and hand it to `sink` as a term, phrase or exclusion.
///
/// `negated` is consumed by whatever clause it files, and only then: an EMPTY flush is the opening
/// quote of `-"ice cream"` and must carry the negation into the quoted run. The whitespace arm
/// clears it separately, which is what makes `- ice` the term `ice` rather than an exclusion.
fn flush(
    sink: &mut impl FnMut(Clause, Vec<String>),
    buf: &mut String,
    negated: &mut bool,
    phrase: bool,
) {
    if !buf.is_empty() {
        let token: Vec<String> = tokenize(buf).into_iter().map(|t| t.text).collect();
        // Rule 4: a clause of nothing but punctuation is not a clause.
        if !token.is_empty() {
            let clause = if *negated {
                Clause::Exclude
            } else if phrase {
                Clause::Phrase
            } else {
                Clause::Term
            };
            sink(clause, token);
        }
        buf.clear();
        // Only a clause that actually filed consumes the marker. An empty flush is the opening
        // quote of `-"ice cream"`, which must carry the negation into the quoted run.
        *negated = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case `p45` could not express: a term, a phrase and an exclusion in one query.
    #[test]
    fn a_mixed_query_separates_terms_phrases_and_exclusions() {
        let q = parse("red \"ice cream\" -discontinued");
        assert_eq!(q.term, vec!["red"]);
        assert_eq!(q.phrase, vec![vec!["ice", "cream"]]);
        assert_eq!(q.exclude, vec!["discontinued"]);
    }

    #[test]
    fn nothing_to_parse_parses_to_nothing() {
        for s in ["", " ", "   \t ", "\"\"", "-", "\""] {
            assert_eq!(parse(s), Query::default(), "parse({s:?}) should be empty");
            assert!(parse(s).is_empty());
        }
        let mixed = parse("red \"ice cream\" -discontinued");
        assert!(!mixed.is_empty());
        assert_eq!(mixed.scoring_query(), "red ice cream");
        assert!(parse("-discontinued").is_empty());
        assert_eq!(parse("-discontinued").scoring_query(), "");
    }

    /// Rule 2. `red "ice` is the state of `red "ice cream"` between two keystrokes, so it has to
    /// answer rather than fail: the run degrades to its own words.
    #[test]
    fn an_unterminated_quote_degrades_to_terms() {
        let q = parse("red \"ice cream");
        assert_eq!(q.term, vec!["red", "ice", "cream"]);
        assert!(q.phrase.is_empty(), "an unclosed run is not a phrase");
        // Same case with nothing after the quote, and the quote alone.
        assert_eq!(parse("red \"").term, vec!["red"]);
        assert_eq!(parse("\"\"\""), Query::default());
    }

    /// Rule 4. An empty run is not an empty *phrase* — an empty phrase is a constraint no document
    /// satisfies, and deciding that is the executor's job, not the parser's.
    #[test]
    fn empty_quotes_contribute_nothing() {
        let q = parse("red \"\" \"ice cream\"");
        assert_eq!(q.term, vec!["red"]);
        assert_eq!(q.phrase, vec![vec!["ice", "cream"]], "the empty run must not appear");
        // A run of pure punctuation is equally nothing.
        assert_eq!(parse("\"---\""), Query::default());
    }

    #[test]
    fn repeated_and_nested_spaces_collapse() {
        let q = parse("  red   \"  ice   cream  \" \t -x  ");
        assert_eq!(q.term, vec!["red"]);
        assert_eq!(q.phrase, vec![vec!["ice", "cream"]]);
        assert_eq!(q.exclude, vec!["x"]);
    }

    /// Rule 3. `e-mail` is one hyphenated word, not the term `mail` negated.
    #[test]
    fn a_hyphen_inside_a_word_is_not_an_exclusion() {
        let q = parse("e-mail");
        assert!(q.exclude.is_empty(), "e-mail must exclude nothing");
        assert_eq!(q.term, vec!["e", "mail"]);
        let q = parse("coca-cola -sugar");
        assert_eq!(q.term, vec!["coca", "cola"]);
        assert_eq!(q.exclude, vec!["sugar"]);
    }

    /// A quoted run is literal text, so a `-` inside it is ordinary text that the analyzer then
    /// splits away — never an exclusion.
    #[test]
    fn a_dash_inside_a_quoted_run_is_not_an_exclusion() {
        let q = parse("red \"-discontinued\"");
        assert_eq!(q.term, vec!["red"]);
        assert_eq!(q.phrase, vec![vec!["discontinued"]]);
        assert!(q.exclude.is_empty());
        // Unterminated, the same run degrades to a term and still excludes nothing.
        let q = parse("\"-discontinued");
        assert_eq!(q.term, vec!["discontinued"]);
        assert!(q.exclude.is_empty());
    }

    #[test]
    fn a_dash_with_no_clause_to_negate_is_dropped() {
        // `- ice` is the term `ice`: the marker negated an empty clause, and silently turning the
        // next word into an exclusion would remove documents the user never asked about.
        let q = parse("- ice");
        assert_eq!(q.term, vec!["ice"]);
        assert!(q.exclude.is_empty());
        // A repeated marker is one marker, not a double negative.
        assert_eq!(parse("--ice").exclude, vec!["ice"]);
    }

    #[test]
    fn several_phrases_keep_their_own_order() {
        let q = parse("\"ice cream\" tub \"vanilla bean\"");
        assert_eq!(q.term, vec!["tub"]);
        assert_eq!(q.phrase, vec![vec!["ice", "cream"], vec!["vanilla", "bean"]]);
    }

    /// Rule 1, made visible. The parser has no token vocabulary of its own.
    #[test]
    fn tokens_are_the_analyzers_not_the_parsers() {
        let q = parse("Café \"1.5 L\"");
        assert_eq!(q.term, vec!["cafe"], "accented query text must fold");
        assert_eq!(q.phrase, vec![vec!["1500ml"]], "a size canonicalizes as it did at index time");
    }

    /// The documented limit: an excluded phrase can only contribute tokens, because `exclude` has
    /// nowhere to put the adjacency.
    #[test]
    fn a_negated_phrase_contributes_its_tokens() {
        let q = parse("red -\"ice cream\"");
        assert_eq!(q.term, vec!["red"]);
        assert!(q.phrase.is_empty(), "an excluded phrase is not a required phrase");
        assert_eq!(q.exclude, vec!["ice", "cream"]);
    }

    /// `parse` is fed by a search box, so its input is whatever was typed: unbalanced quotes,
    /// stray dashes, combining marks. Nothing may panic, and nothing syntactic may survive into a
    /// token.
    #[test]
    fn parse_never_panics_on_generated_input() {
        fn mix(state: &mut u64) -> u64 {
            *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        // A hostile alphabet: every character that is syntax here (`"`, `-`, space), letters and
        // digits to carry meaning, and multi-byte UTF-8 to catch any byte indexing.
        let alpha: Vec<char> = "ab 1e\u{301}\"\"--é".chars().collect();
        let mut st = 0x0EAD_BEEF_u64;
        for _ in 0..4_000 {
            let len = (mix(&mut st) % 24) as usize;
            let mut s = String::new();
            for _ in 0..len {
                s.push(alpha[(mix(&mut st) as usize) % alpha.len()]);
            }
            let q = parse(&s);
            for t in q.term.iter().chain(q.exclude.iter()).chain(q.phrase.iter().flatten()) {
                assert!(!t.is_empty(), "empty token in {q:?} from {s:?}");
                assert!(!t.contains('"'), "a quote leaked into {t:?} from {s:?}");
                assert!(!t.contains('-'), "a dash leaked into {t:?} from {s:?}");
                assert!(!t.chars().any(char::is_whitespace), "whitespace in {t:?} from {s:?}");
            }
            // The same string must parse the same way every time — no hidden state.
            assert_eq!(q, parse(&s), "parse is not deterministic for {s:?}");
        }
    }

    /// The typeahead string keeps typed order, drops exclusions, and prefixes only a trailing
    /// unquoted term.
    #[test]
    fn typeahead_scoring_keeps_typed_order_and_prefixes_only_an_open_term() {
        let t = |q: &str| typeahead_scoring_query(q);
        assert_eq!(t("cruz -santos"), ("cruz".into(), true));
        assert_eq!(t("\"dela cruz\" an"), ("dela cruz an".into(), true));
        assert_eq!(t("\"dela cr\""), ("dela cr".into(), false));
        assert_eq!(t("\"dela cr"), ("dela cr".into(), true));
        assert_eq!(t("red \"ice cream\" -x"), ("red ice cream".into(), false));
        assert_eq!(t("-x"), (String::new(), false));
        assert_eq!(t(""), (String::new(), false));
        // Same tokens as the full parse, only reordered: nothing is folded twice.
        let q = parse("\"Dela Cruz\" Ana -santos");
        let mut a: Vec<String> = t("\"Dela Cruz\" Ana -santos")
            .0
            .split(' ')
            .map(String::from)
            .collect();
        let mut b: Vec<String> = q.scoring_query().split(' ').map(String::from).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }
}
