//! The analyzer — normalization and tokenization.
//!
//! This is the layer `docs/research/demand.md` Finding 2 identified as the actual product: four
//! repos on this machine hand-rolled a worse version of it, each carrying scar tissue the others
//! lack. The scars are encoded here as tests.
//!
//! Three rules are load-bearing and none of them is negotiable:
//!
//! 1. **One fold, used everywhere.** profstopick learned that folding differently from your slug
//!    function produces professors nobody can reach — `Peña-Reyes` was unreachable by `pena` for
//!    all 72 non-ASCII entries in its corpus.
//! 2. **Size is canonicalized, never stripped.** presyo measured that stripping the size token
//!    instead of canonicalizing it costs −4.6 pp recall. `1.5L`, `1500ml` and `1.5 liters` must
//!    become one token.
//! 3. **Numeric tokens are never fuzzy-matched.** `300g` matching `800g` is a correctness bug in a
//!    price-comparison app that presents as a relevance bug. This mirrors Algolia's
//!    `allowTyposOnNumericTokens`, which must be off.

/// A token produced by the analyzer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// The normalized term text.
    pub text: String,
    /// Position within the field (for future phrase support; currently informational).
    pub position: u32,
    /// True when the token contains a digit. Such tokens are **exempt from fuzzy matching**.
    pub is_numeric: bool,
}

/// Fold a string to its canonical comparison form.
///
/// NFD-style decomposition of the Latin-1/Latin-Extended-A range, combining marks dropped, then
/// lowercased. This is deliberately dependency-free and covers the scripts the consumer corpora
/// actually contain (Filipino, Spanish-origin brand names, English). A full ICU fold is a
/// `unicode-normalization` dependency away when a corpus needs it — see
/// `docs/research/build-or-buy.md`.
///
/// **This function is the single fold.** Anything that derives a key, a slug or a search term must
/// route through it, or links break.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match strip_diacritic(ch) {
            Some(base) => out.extend(base.to_lowercase()),
            None => {
                // Drop combining marks entirely (U+0300–U+036F).
                if !('\u{0300}'..='\u{036F}').contains(&ch) {
                    out.extend(ch.to_lowercase());
                }
            }
        }
    }
    out
}

/// Map a precomposed accented character to its base letter. Returns `None` when the character has
/// no diacritic to strip (including when it is itself a combining mark).
fn strip_diacritic(ch: char) -> Option<char> {
    let base = match ch {
        'à'..='å' | 'À'..='Å' => 'a',
        'è'..='ë' | 'È'..='Ë' => 'e',
        'ì'..='ï' | 'Ì'..='Ï' => 'i',
        'ò'..='ö' | 'Ò'..='Ö' => 'o',
        'ù'..='ü' | 'Ù'..='Ü' => 'u',
        'ý' | 'ÿ' | 'Ý' => 'y',
        'ñ' | 'Ñ' => 'n',
        'ç' | 'Ç' => 'c',
        'æ' | 'Æ' => 'a',
        'ø' | 'Ø' => 'o',
        'š' | 'Š' => 's',
        'ž' | 'Ž' => 'z',
        _ => return None,
    };
    Some(base)
}

/// A physical quantity reduced to a base unit, so `1.5L`, `1500ml` and `1.5 liters` compare equal.
///
/// Stored as an integer count of the base unit scaled by 1000, so 1.5 L → 1_500_000 (µL·10³ is not
/// meaningful; the scale exists only to keep one decimal place of grams/millilitres exact without
/// floating point). Integer arithmetic means `sameSize` is exact, not tolerance-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Quantity {
    /// Base-unit amount × 1000.
    pub milli_base: u64,
    /// The base unit this was reduced to.
    pub unit: BaseUnit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BaseUnit {
    /// Mass, base gram.
    Gram,
    /// Volume, base millilitre.
    Millilitre,
    /// Discrete count, base piece.
    Piece,
}

impl Quantity {
    /// The canonical token text for this quantity — what actually enters the index.
    pub fn token(&self) -> String {
        let unit = match self.unit {
            BaseUnit::Gram => "g",
            BaseUnit::Millilitre => "ml",
            BaseUnit::Piece => "pc",
        };
        // Emit a whole number when the fractional part is zero, so `1.5L` and `1500ml` produce
        // the byte-identical token `1500ml`.
        if self.milli_base % 1000 == 0 {
            format!("{}{}", self.milli_base / 1000, unit)
        } else {
            format!("{}.{:03}{}", self.milli_base / 1000, self.milli_base % 1000, unit)
        }
    }
}

/// Parse a `<number><unit>` token into a canonical [`Quantity`].
///
/// Accepts `1.5l`, `1,5l` (comma decimal), `1500ml`, `300g`, `1kg`, `6pcs`, `12s`.
/// Returns `None` when the token is not a quantity, which is the common case.
pub fn parse_quantity(tok: &str) -> Option<Quantity> {
    let t = tok.trim();
    let split = t.find(|c: char| c.is_ascii_alphabetic())?;
    if split == 0 {
        return None; // no leading number
    }
    let (num_s, unit_s) = t.split_at(split);
    let num_s = num_s.replace(',', ".");
    if num_s.matches('.').count() > 1 {
        return None;
    }
    // Parse the number as fixed-point thousandths without floating point.
    let (int_part, frac_part) = match num_s.split_once('.') {
        Some((a, b)) => (a, b),
        None => (num_s.as_str(), ""),
    };
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let int_v: u64 = int_part.parse().ok()?;
    // Take at most 3 fractional digits, right-padded to exactly 3.
    let mut frac_v: u64 = 0;
    for i in 0..3 {
        frac_v = frac_v * 10 + frac_part.as_bytes().get(i).map_or(0, |b| (b - b'0') as u64);
    }
    let thousandths = int_v.checked_mul(1000)?.checked_add(frac_v)?;

    // Multiplier is also in thousandths of the base unit.
    let (mult, unit) = match unit_s {
        "g" | "gram" | "grams" | "gr" => (1_000, BaseUnit::Gram),
        "kg" | "kilo" | "kilos" | "kilogram" | "kilograms" => (1_000_000, BaseUnit::Gram),
        "mg" => (1, BaseUnit::Gram),
        "ml" | "milliliter" | "millilitre" | "mls" => (1_000, BaseUnit::Millilitre),
        "l" | "li" | "lit" | "liter" | "liters" | "litre" | "litres" => {
            (1_000_000, BaseUnit::Millilitre)
        }
        "cl" => (10_000, BaseUnit::Millilitre),
        "pc" | "pcs" | "piece" | "pieces" | "s" | "pack" | "packs" | "ct" | "count" => {
            (1_000, BaseUnit::Piece)
        }
        _ => return None,
    };
    // `thousandths` is value×1000; `mult` is base-units-per-unit×1000. Product would be ×10^6,
    // so divide by 1000 to land back on ×1000.
    let milli_base = thousandths.checked_mul(mult)? / 1_000;
    Some(Quantity { milli_base, unit })
}

/// Tokenize a field into normalized tokens.
///
/// Splits on any non-alphanumeric character, **except** that a `.` or `,` directly between two
/// digits is kept, so `1.5l` survives as one token rather than becoming `1` and `5l`.
/// Quantity tokens are rewritten to their canonical form.
pub fn tokenize(text: &str) -> Vec<Token> {
    let folded = fold(text);
    let bytes: Vec<char> = folded.chars().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut position = 0u32;

    let flush = |cur: &mut String, out: &mut Vec<Token>, position: &mut u32| {
        if cur.is_empty() {
            return;
        }
        let text = match parse_quantity(cur) {
            Some(q) => q.token(),
            None => std::mem::take(cur),
        };
        cur.clear();
        let is_numeric = text.chars().any(|c| c.is_ascii_digit());
        out.push(Token { text, position: *position, is_numeric });
        *position += 1;
    };

    for i in 0..bytes.len() {
        let c = bytes[i];
        if c.is_alphanumeric() {
            cur.push(c);
        } else if (c == '.' || c == ',' || c == '-')
            && i > 0
            && bytes[i - 1].is_ascii_digit()
            && bytes.get(i + 1).is_some_and(|n| n.is_ascii_digit())
        {
            // A separator BETWEEN DIGITS is normalized to `.` and kept, so `1.5l`, `1,5l` and
            // `30-23` survive as single tokens and, crucially, as the SAME token whichever way
            // they were typed. profstopick's contract test demands `MATH 30.23`, `math30.23` and
            // `MATH  30-23` all reach the same course.
            //
            // The cost is real and accepted: a hyphenated numeric RANGE (`3-4`) folds to `3.4`.
            // Index and query normalize identically, so matching stays consistent; only the
            // semantics of a range are lost, and no consumer corpus expresses ranges this way.
            cur.push('.');
        } else {
            flush(&mut cur, &mut out, &mut position);
        }
    }
    flush(&mut cur, &mut out, &mut position);
    merge_split_quantity(&mut out);
    out
}

/// Merge a bare number followed by a bare unit into one quantity token.
///
/// Retailer feeds write `180 g`, `1.5 L` and `1000 grams` as often as they write `180g` — presyo's
/// own normalizer carries an explicit `"180 g" -> "180g"` rule for exactly this reason. Without
/// this pass the number and the unit become two independent terms, the size stops being a single
/// comparable token, and the numeric-token guard has nothing to guard.
fn merge_split_quantity(token: &mut Vec<Token>) {
    let mut i = 0;
    while i + 1 < token.len() {
        let number_only = !token[i].text.is_empty()
            && token[i].text.chars().all(|c| c.is_ascii_digit() || c == '.');
        let unit_only = !token[i + 1].text.is_empty()
            && token[i + 1].text.chars().all(|c| c.is_ascii_alphabetic());
        if number_only && unit_only {
            let joined = format!("{}{}", token[i].text, token[i + 1].text);
            if let Some(q) = parse_quantity(&joined) {
                token[i].text = q.token();
                token[i].is_numeric = true;
                token.remove(i + 1);
                // Positions stay monotonic but are no longer dense; nothing depends on density.
                continue;
            }
        }
        i += 1;
    }
}

/// An alias table: a curated map from surface form to canonical form, applied **before** indexing
/// and before querying.
///
/// The research is unambiguous that this beats anything statistical for Filipino
/// (`docs/research/relevance.md` §7): Tagalog infixation, circumfixion and CV-reduplication defeat
/// Snowball's model, and no production-grade Filipino stemmer exists. A few hundred curated rows
/// outperform anything trainable on the available corpora.
#[derive(Clone, Debug, Default)]
pub struct AliasTable {
    entry: std::collections::HashMap<String, String>,
}

impl AliasTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `surface -> canonical`. Both sides are folded, so callers may pass raw text.
    pub fn insert(&mut self, surface: &str, canonical: &str) -> &mut Self {
        self.entry.insert(fold(surface), fold(canonical));
        self
    }

    /// Iterate `(surface, canonical)` pairs. Order is unspecified by `HashMap`, so `format`
    /// sorts before writing to keep serialization deterministic.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        let mut v: Vec<(&str, &str)> =
            self.entry.iter().map(|(k, val)| (k.as_str(), val.as_str())).collect();
        v.sort_unstable();
        v.into_iter()
    }

    pub fn get(&self, folded_token: &str) -> Option<&str> {
        self.entry.get(folded_token).map(|s| s.as_str())
    }

    pub fn len(&self) -> usize {
        self.entry.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entry.is_empty()
    }

    /// The Philippine grocery + retail starter table.
    ///
    /// Every row here is drawn from a real abbreviation, alias or Filipino↔English pair observed in
    /// `presyo/packages/identifier/src/normalize.ts` or its Filipino search-term taxonomy. This is
    /// a seed, not a complete table — it exists so the default behaviour is useful rather than
    /// empty, and so the shape is testable.
    pub fn philippine_grocery() -> Self {
        let mut t = Self::new();
        // Abbreviations observed in scraped retailer names.
        let abbrev = [
            ("pwdr", "powder"),
            ("pwd", "powder"),
            ("evap", "evaporated"),
            ("condensada", "condensed"),
            ("choco", "chocolate"),
            ("choc", "chocolate"),
            ("btl", "bottle"),
            ("pck", "pack"),
            ("pk", "pack"),
            ("sach", "sachet"),
            ("reg", "regular"),
            ("orig", "original"),
            ("asstd", "assorted"),
            ("w", "with"),
            ("wmkt", "wet market"),
            ("ref", "refill"),
        ];
        // Filipino ↔ English category terms. These are what the `filipino_category` rows in
        // presyo's frozen fixture actually contain.
        let filipino = [
            ("bigas", "rice"),
            ("gatas", "milk"),
            ("kape", "coffee"),
            ("asukal", "sugar"),
            ("asin", "salt"),
            ("mantika", "oil"),
            ("sabon", "soap"),
            ("panligo", "bath"),
            ("panlaba", "laundry"),
            ("gamot", "medicine"),
            ("ubo", "cough"),
            ("sardinas", "sardines"),
            ("delata", "canned"),
            ("itlog", "egg"),
            ("tinapay", "bread"),
            ("manok", "chicken"),
            ("baboy", "pork"),
            ("isda", "fish"),
            ("inumin", "drink"),
            ("chichirya", "snack"),
            ("paaralan", "school"),
        ];
        for (s, c) in abbrev.into_iter().chain(filipino) {
            t.insert(s, c);
        }
        t
    }
}

/// Apply an alias table to an already-tokenized field, in place.
///
/// Numeric tokens are never rewritten — an alias table must not be able to change a size.
pub fn apply_alias(token: &mut [Token], alias: &AliasTable) {
    for t in token.iter_mut() {
        if t.is_numeric {
            continue;
        }
        if let Some(canon) = alias.get(&t.text) {
            t.text = canon.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// profstopick's scar: `Peña-Reyes` must be reachable by typing `pena`.
    /// Its measured failure was 72 entries unreachable by the name a student actually types.
    #[test]
    fn fold_makes_accented_names_reachable() {
        assert_eq!(fold("Peña-Reyes"), "peña-reyes".replace('ñ', "n"));
        assert!(fold("Peña-Reyes").starts_with("pena"));
        for (raw, want) in [
            ("Nestlé", "nestle"),
            ("Parañaque", "paranaque"),
            ("Biñan", "binan"),
            ("Dueñas", "duenas"),
            ("Ordoñez", "ordonez"),
            ("L'Oréal", "l'oreal"),
            ("jalapeño", "jalapeno"),
        ] {
            assert_eq!(fold(raw), want, "fold({raw})");
        }
    }

    #[test]
    fn fold_is_idempotent() {
        for s in ["Peña-Reyes", "NESTLÉ", "Coca-Cola 1.5L", "plain ascii"] {
            assert_eq!(fold(&fold(s)), fold(s), "fold must be idempotent for {s}");
        }
    }

    /// presyo's scar, and the reason size is canonicalized rather than stripped.
    #[test]
    fn equivalent_sizes_produce_one_token() {
        let want = "1500ml";
        for raw in ["1.5L", "1.5 l", "1500ml", "1.5 liters", "1,5L", "1.5litre"] {
            let tok = tokenize(raw);
            let got: Vec<&str> = tok.iter().map(|t| t.text.as_str()).collect();
            assert!(
                got.contains(&want),
                "tokenize({raw}) = {got:?} should canonicalize to {want}"
            );
        }
        for raw in ["1kg", "1000g", "1000 grams"] {
            let tok = tokenize(raw);
            let got: Vec<&str> = tok.iter().map(|t| t.text.as_str()).collect();
            assert!(got.contains(&"1000g"), "tokenize({raw}) = {got:?}");
        }
    }

    /// The correctness guard: a size must never be confusable with a different size.
    #[test]
    fn different_sizes_never_collide() {
        let a = &tokenize("300g")[0];
        let b = &tokenize("800g")[0];
        assert_ne!(a.text, b.text);
        assert!(a.is_numeric && b.is_numeric, "size tokens must be flagged numeric so fuzzy skips them");
        // And the near-miss that a tolerance-based comparison would merge:
        assert_ne!(tokenize("1.5l")[0].text, tokenize("1.6l")[0].text);
    }

    #[test]
    fn digits_do_not_split_tokens() {
        let t: Vec<String> = tokenize("Coca Cola 1.5L").iter().map(|t| t.text.clone()).collect();
        assert_eq!(t, vec!["coca", "cola", "1500ml"]);
    }

    #[test]
    fn punctuation_splits_and_positions_advance() {
        let t = tokenize("Lucky Me! Pancit Canton (Chilimansi)");
        let text: Vec<&str> = t.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(text, vec!["lucky", "me", "pancit", "canton", "chilimansi"]);
        assert_eq!(t.iter().map(|x| x.position).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn alias_rewrites_words_but_never_sizes() {
        let alias = AliasTable::philippine_grocery();
        let mut t = tokenize("gatas pwdr 300g");
        apply_alias(&mut t, &alias);
        let text: Vec<&str> = t.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(text, vec!["milk", "powder", "300g"], "size token must survive aliasing");
    }

    #[test]
    fn quantity_parsing_rejects_non_quantities() {
        assert_eq!(parse_quantity("hello"), None);
        assert_eq!(parse_quantity("g"), None);
        assert_eq!(parse_quantity("300"), None);
        assert_eq!(parse_quantity("1.2.3g"), None);
        assert_eq!(parse_quantity("300xyz"), None);
    }

    #[test]
    fn quantity_is_exact_not_floating() {
        // 0.1 + 0.2 style errors must not exist here.
        let a = parse_quantity("0.1l").unwrap();
        let b = parse_quantity("100ml").unwrap();
        assert_eq!(a, b);
        assert_eq!(a.token(), "100ml");
    }
}
