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
    fold_into(s, &mut out);
    out
}

/// Append the fold of `s` to `out`.
///
/// Byte-wise with an ASCII fast path: no ASCII character carries a diacritic and none of them
/// case-maps outside ASCII, so a maximal ASCII run is a `push_str` (memcpy) followed by
/// `make_ascii_lowercase` (vectorized in place). Only the non-ASCII remainder pays for character
/// decoding, the diacritic table and `char::to_lowercase`. The consumer corpora measure 97.7 %
/// ASCII, so this is the path that runs.
///
/// Exists so [`tokenize`] can fold into a buffer it reuses across documents instead of allocating
/// a `String` per field. `fast_fold_matches_the_legacy_fold` pins it to the character-at-a-time original.
fn fold_into(s: &str, out: &mut String) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let run = i;
        while i < b.len() && b[i] < 0x80 {
            i += 1;
        }
        if i > run {
            let at = out.len();
            out.push_str(&s[run..i]);
            out[at..].make_ascii_lowercase();
        }
        let Some(ch) = s[i..].chars().next() else { break };
        i += ch.len_utf8();
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
        let mut out = String::new();
        self.write_token(&mut out);
        out
    }

    /// Append [`Self::token`] to `out`.
    ///
    /// Exists so the tokenizer can rewrite a quantity straight into the token buffer it already
    /// owns, instead of allocating a `String` per quantity only to move its bytes one slot over.
    fn write_token(&self, out: &mut String) {
        use std::fmt::Write;
        let unit = match self.unit {
            BaseUnit::Gram => "g",
            BaseUnit::Millilitre => "ml",
            BaseUnit::Piece => "pc",
        };
        // Emit a whole number when the fractional part is zero, so `1.5L` and `1500ml` produce
        // the byte-identical token `1500ml`.
        let _ = if self.milli_base % 1000 == 0 {
            write!(out, "{}{}", self.milli_base / 1000, unit)
        } else {
            write!(out, "{}.{:03}{}", self.milli_base / 1000, self.milli_base % 1000, unit)
        };
    }
}

/// Parse a `<number><unit>` token into a canonical [`Quantity`].
///
/// Accepts `1.5l`, `1,5l` (comma decimal), `1500ml`, `300g`, `1kg`, `6pcs`, `12s`.
/// Returns `None` when the token is not a quantity, which is the common case.
pub fn parse_quantity(tok: &str) -> Option<Quantity> {
    let t = tok.trim();
    // A quantity is `<digits>[.<digits>]<unit>`, and the integer part below must be non-empty and
    // all ASCII digits — so a token whose first character is anything else can never parse. This
    // is an exact restatement of the checks further down, hoisted so the overwhelmingly common
    // case (an ordinary word) costs one byte compare instead of a scan plus a `replace`.
    if !t.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
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
/// [`fold`], plus the **original byte offset** each folded character came from.
///
/// Folding is not length-preserving -- a diacritic is dropped, `ß` lowercases to two characters --
/// so an offset into the folded string says nothing about the input. Highlighting needs offsets
/// into the text the caller passed, which is what this returns.
///
/// Kept separate from [`fold`] rather than replacing it: `fold` runs on every field of every
/// document at build time and should not allocate a second vector to serve a query-time feature.
/// `fold_agrees_with_fold_with_origin` pins the two to the same output.
pub fn fold_with_origin(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut origin = Vec::with_capacity(s.len());
    for (at, ch) in s.char_indices() {
        match strip_diacritic(ch) {
            Some(base) => {
                for c in base.to_lowercase() {
                    out.push(c);
                    origin.push(at);
                }
            }
            None => {
                if !('\u{0300}'..='\u{036F}').contains(&ch) {
                    for c in ch.to_lowercase() {
                        out.push(c);
                        origin.push(at);
                    }
                }
            }
        }
    }
    (out, origin)
}

/// [`tokenize`], plus each token's `[start, end)` byte range in the ORIGINAL text.
///
/// Produces exactly the same tokens as [`tokenize`] -- pinned by
/// `tokenize_span_matches_tokenize` -- so a highlight can never disagree with what was indexed.
/// When two tokens merge into a quantity (`500` + `ml` -> `500ml`) the surviving span covers both,
/// which is what a reader expects to see underlined.
pub fn tokenize_span(text: &str) -> Vec<(Token, usize, usize)> {
    let (folded, origin) = fold_with_origin(text);
    let ch: Vec<char> = folded.chars().collect();
    // Byte offset in `text` just past the character that produced folded char `i`.
    let end_of = |i: usize| -> usize {
        let at = origin.get(i).copied().unwrap_or(text.len());
        text[at..].chars().next().map_or(text.len(), |c| at + c.len_utf8())
    };

    let mut out: Vec<(Token, usize, usize)> = Vec::new();
    let mut cur = String::new();
    let mut position = 0u32;
    let mut first: Option<usize> = None;
    let mut last = 0usize;

    let flush = |cur: &mut String,
                     out: &mut Vec<(Token, usize, usize)>,
                     position: &mut u32,
                     first: &mut Option<usize>,
                     last: usize| {
        if cur.is_empty() {
            *first = None;
            return;
        }
        let t = match parse_quantity(cur) {
            Some(q) => q.token(),
            None => std::mem::take(cur),
        };
        cur.clear();
        let is_numeric = t.chars().any(|c| c.is_ascii_digit());
        let start = first.map_or(0, |f| origin.get(f).copied().unwrap_or(0));
        out.push((Token { text: t, position: *position, is_numeric }, start, end_of(last)));
        *position += 1;
        *first = None;
    };

    for i in 0..ch.len() {
        let c = ch[i];
        if c.is_alphanumeric() {
            if first.is_none() {
                first = Some(i);
            }
            last = i;
            cur.push(c);
        } else if (c == '.' || c == ',' || c == '-')
            && i > 0
            && ch[i - 1].is_ascii_digit()
            && ch.get(i + 1).is_some_and(|n| n.is_ascii_digit())
        {
            last = i;
            cur.push('.');
        } else {
            flush(&mut cur, &mut out, &mut position, &mut first, last);
        }
    }
    flush(&mut cur, &mut out, &mut position, &mut first, last);

    // Mirror `merge_split_quantity`, extending the span over both halves.
    let mut i = 0;
    while i + 1 < out.len() {
        let number_only = !out[i].0.text.is_empty()
            && out[i].0.text.chars().all(|c| c.is_ascii_digit() || c == '.');
        let unit_only = !out[i + 1].0.text.is_empty()
            && out[i + 1].0.text.chars().all(|c| c.is_ascii_alphabetic());
        if number_only && unit_only {
            let joined = format!("{}{}", out[i].0.text, out[i + 1].0.text);
            if let Some(q) = parse_quantity(&joined) {
                out[i].0.text = q.token();
                out[i].0.is_numeric = true;
                out[i].2 = out[i + 1].2;
                out.remove(i + 1);
                continue;
            }
        }
        i += 1;
    }
    out
}

thread_local! {
    /// The fold buffer [`tokenize`] reuses. `add()` calls `tokenize` once per field per document —
    /// three million times on the `scale` ladder — and a fresh `String` per call is three million
    /// allocations of text nobody keeps. Never borrowed re-entrantly: nothing reachable from
    /// `tokenize_folded` calls `tokenize`.
    static FOLD_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

pub fn tokenize(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let n = tokenize_into(text, &mut out);
    out.truncate(n);
    out
}

/// Tokenize into a buffer the caller owns and reuses, returning **how many tokens were written**.
///
/// The tokens are `buf[..count]`. Anything beyond `count` is retained scratch from an earlier call:
/// this function deliberately does **not** truncate, because the whole point is that the `String`
/// inside each slot keeps its heap allocation and is refilled by `clear` + `push_str` rather than
/// freed and malloc'd again. A caller that wants an owned `Vec` should use [`tokenize`].
///
/// `add()` calls this three million times on the `scale` ladder for 13.69 M tokens. As a
/// `Vec<Token>` return that is 3 M vector allocations and 13.69 M `String` allocate/free pairs of
/// text nobody keeps; through a reused buffer it is a handful of both.
pub fn tokenize_into(text: &str, buf: &mut Vec<Token>) -> usize {
    FOLD_BUF.with(|fold_buf| {
        let mut folded = fold_buf.borrow_mut();
        folded.clear();
        fold_into(text, &mut folded);
        tokenize_folded(&folded, buf)
    })
}

/// Split an ALREADY-FOLDED string into tokens.
///
/// The token text is a contiguous byte range of the folded string. That is not an accident of this
/// corpus, it is structural: the only rewrite the split performs is a kept separator becoming `.`,
/// and `,`, `-` and `.` are all one byte, so the rewrite is length-preserving. So a token is
/// materialized by one exact-capacity copy of a slice instead of being grown a character at a time
/// into a `String` that is then moved out — which is what made the old loop allocate, and
/// reallocate, per token.
///
/// Scanning is byte-wise. A byte below `0x80` decides alphanumeric-ness on its own; only a
/// non-ASCII lead byte pays for `char` decoding and the Unicode `is_alphanumeric` tables. The old
/// loop collected the whole field into a `Vec<char>` first, purely to look one character back and
/// one forward; the two guards it needed are `is_ascii_digit`, which no multi-byte character can
/// satisfy, so a byte index answers both.
fn tokenize_folded(folded: &str, out: &mut Vec<Token>) -> usize {
    let b = folded.as_bytes();
    // ~5 bytes per token across the consumer corpora. Only worth doing for a buffer that has never
    // been used: a reused one is already at the high-water mark of every field seen so far, and
    // reserving on top of that would grow it without bound.
    if out.is_empty() {
        out.reserve(b.len() / 5 + 1);
    }
    // How many slots of `out` the current field has claimed. `out.len()` is the high-water mark,
    // not the token count.
    let mut n = 0usize;
    let mut position = 0u32;
    // Start of the token in progress, or `None`. `sep` records that it contains a kept `,` or `-`
    // still to be normalized — false for all but a handful of tokens in any real corpus.
    let mut start: Option<usize> = None;
    let mut sep = false;

    let mut i = 0usize;
    while i < b.len() {
        let c = b[i];
        if c < 0x80 {
            if c.is_ascii_alphanumeric() {
                if start.is_none() {
                    start = Some(i);
                    sep = false;
                }
                i += 1;
                continue;
            }
            if (c == b'.' || c == b',' || c == b'-')
                && i > 0
                && b[i - 1].is_ascii_digit()
                && b.get(i + 1).is_some_and(u8::is_ascii_digit)
            {
                // A separator BETWEEN DIGITS is normalized to `.` and kept, so `1.5l`, `1,5l` and
                // `30-23` survive as single tokens and, crucially, as the SAME token whichever way
                // they were typed. profstopick's contract test demands `MATH 30.23`, `math30.23`
                // and `MATH  30-23` all reach the same course.
                //
                // The cost is real and accepted: a hyphenated numeric RANGE (`3-4`) folds to `3.4`.
                // Index and query normalize identically, so matching stays consistent; only the
                // semantics of a range are lost, and no consumer corpus expresses ranges this way.
                if start.is_none() {
                    start = Some(i);
                    sep = false;
                }
                sep |= c != b'.';
                i += 1;
                continue;
            }
            flush(folded, &mut start, i, sep, out, &mut n, &mut position);
            i += 1;
        } else {
            let ch = folded[i..].chars().next().unwrap_or('\u{0}');
            if ch.is_alphanumeric() {
                if start.is_none() {
                    start = Some(i);
                    sep = false;
                }
            } else {
                flush(folded, &mut start, i, sep, out, &mut n, &mut position);
            }
            i += ch.len_utf8();
        }
    }
    flush(folded, &mut start, b.len(), sep, out, &mut n, &mut position);
    merge_split_quantity(out, &mut n);
    n
}

/// The slot `out[*n]`, appending a fresh one only when the buffer has never been that long.
///
/// Reusing the slot is the point: its `String` keeps the allocation it had on the previous field,
/// so a token costs a `clear` plus a memcpy instead of a malloc and, one field later, a free.
fn slot<'a>(out: &'a mut Vec<Token>, n: &mut usize) -> &'a mut Token {
    let at = *n;
    if at == out.len() {
        out.push(Token { text: String::new(), position: 0, is_numeric: false });
    }
    *n += 1;
    &mut out[at]
}

/// Emit `folded[start..end]` as a token, if a token is in progress.
fn flush(
    folded: &str,
    start: &mut Option<usize>,
    end: usize,
    sep: bool,
    out: &mut Vec<Token>,
    n: &mut usize,
    position: &mut u32,
) {
    let Some(at) = start.take() else { return };
    let raw = &folded[at..end];
    let tok = slot(out, n);
    tok.text.clear();
    if sep {
        // Every `,` or `-` still inside a token is by construction a kept separator.
        tok.text.extend(raw.chars().map(|c| if c == ',' || c == '-' { '.' } else { c }));
    } else {
        tok.text.push_str(raw);
    }
    if let Some(q) = parse_quantity(&tok.text) {
        tok.text.clear();
        q.write_token(&mut tok.text);
    }
    tok.is_numeric = tok.text.as_bytes().iter().any(u8::is_ascii_digit);
    tok.position = *position;
    *position += 1;
}

/// Merge a bare number followed by a bare unit into one quantity token.
///
/// Retailer feeds write `180 g`, `1.5 L` and `1000 grams` as often as they write `180g` — presyo's
/// own normalizer carries an explicit `"180 g" -> "180g"` rule for exactly this reason. Without
/// this pass the number and the unit become two independent terms, the size stops being a single
/// comparable token, and the numeric-token guard has nothing to guard.
fn merge_split_quantity(token: &mut Vec<Token>, n: &mut usize) {
    let mut i = 0;
    while i + 1 < *n {
        // Byte-wise: no multi-byte character can be an ASCII digit, `.` or an ASCII letter, so a
        // byte test rejects exactly what the character test rejected, without decoding.
        let number_only = !token[i].text.is_empty()
            && token[i].text.as_bytes().iter().all(|&c| c.is_ascii_digit() || c == b'.');
        let unit_only = !token[i + 1].text.is_empty()
            && token[i + 1].text.as_bytes().iter().all(u8::is_ascii_alphabetic);
        if number_only && unit_only {
            let joined = format!("{}{}", token[i].text, token[i + 1].text);
            if let Some(q) = parse_quantity(&joined) {
                token[i].text.clear();
                q.write_token(&mut token[i].text);
                token[i].is_numeric = true;
                // Retired to the far end of the buffer rather than dropped, so the `String` it
                // holds is available as scratch to the next field instead of being freed.
                let spare = token.remove(i + 1);
                token.push(spare);
                *n -= 1;
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
    /// Byte length of the longest folded surface form in `entry`. A token longer than this cannot
    /// be a key, so `apply_alias` skips its hash entirely — the starter table's longest row is
    /// `condensada` at 10 bytes, and most corpus tokens are longer than that.
    max_surface_len: usize,
}

impl AliasTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `surface -> canonical`. Both sides are folded, so callers may pass raw text.
    pub fn insert(&mut self, surface: &str, canonical: &str) -> &mut Self {
        let key = fold(surface);
        self.max_surface_len = self.max_surface_len.max(key.len());
        self.entry.insert(key, fold(canonical));
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
    if alias.entry.is_empty() {
        return;
    }
    for t in token.iter_mut() {
        // A token longer than the longest key, or a numeric one, can never be rewritten. Both are
        // decided without hashing the string, which is what the lookup would otherwise cost on
        // every token of every field.
        if t.is_numeric || t.text.len() > alias.max_surface_len {
            continue;
        }
        if let Some(canon) = alias.get(&t.text) {
            t.text.clear();
            t.text.push_str(canon);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tokenizer as it stood before this lane — character-at-a-time, `Vec<char>`, a `String`
    /// grown and moved out per token — copied here verbatim, dependencies included.
    ///
    /// This is not documentation. It is the oracle: the fast tokenizer is only allowed to be fast,
    /// never different, because a change to the token stream silently changes every ranking in the
    /// engine. Compared against the live one by `fast_tokenizer_matches_the_legacy_one`. If a rule
    /// ever legitimately changes, this copy changes with it in the same commit, deliberately.
    mod legacy {
        use crate::analyze::{strip_diacritic, BaseUnit, Quantity, Token};

        pub fn fold(s: &str) -> String {
            let mut out = String::with_capacity(s.len());
            for ch in s.chars() {
                match strip_diacritic(ch) {
                    Some(base) => out.extend(base.to_lowercase()),
                    None => {
                        if !('\u{0300}'..='\u{036F}').contains(&ch) {
                            out.extend(ch.to_lowercase());
                        }
                    }
                }
            }
            out
        }

        pub fn parse_quantity(tok: &str) -> Option<Quantity> {
            let t = tok.trim();
            let split = t.find(|c: char| c.is_ascii_alphabetic())?;
            if split == 0 {
                return None;
            }
            let (num_s, unit_s) = t.split_at(split);
            let num_s = num_s.replace(',', ".");
            if num_s.matches('.').count() > 1 {
                return None;
            }
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
            let mut frac_v: u64 = 0;
            for i in 0..3 {
                frac_v =
                    frac_v * 10 + frac_part.as_bytes().get(i).map_or(0, |b| (b - b'0') as u64);
            }
            let thousandths = int_v.checked_mul(1000)?.checked_add(frac_v)?;
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
            let milli_base = thousandths.checked_mul(mult)? / 1_000;
            Some(Quantity { milli_base, unit })
        }

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
                    cur.push('.');
                } else {
                    flush(&mut cur, &mut out, &mut position);
                }
            }
            flush(&mut cur, &mut out, &mut position);
            merge_split_quantity(&mut out);
            out
        }

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
                        continue;
                    }
                }
                i += 1;
            }
        }
    }

    /// A corpus chosen to bend every rule the tokenizer has: empty and whitespace-only fields,
    /// accents both precomposed and combining, mixed case, every separator class, digit/unit
    /// splits, quantities that must and must not parse, scripts with no case and no ASCII
    /// (CJK, Arabic, Devanagari, emoji), a character whose lowercase is two characters, a
    /// character whose lowercase is longer in bytes, and a field far longer than any real one.
    fn demanding_corpus() -> Vec<String> {
        let mut c: Vec<String> = [
            "",
            " ",
            "   \t\n  ",
            "-",
            "---",
            "9",
            ".",
            ",",
            "1.5",
            "1,5",
            "3-4",
            "a,b.c-d",
            "Colgate Total Toothpaste 150g",
            "LUCKY ME PANCIT CANTON 60G",
            "Lucky Me! Pancit Canton (Chilimansi)",
            "1.5L Coke Zero",
            "1,5 L Coke Zero",
            "1500ml Coke Zero",
            "500 ml bottle",
            "1000 grams of rice",
            "MATH 30-23 section",
            "MATH  30.23",
            "math30.23",
            "300g vs 800g vs 300xyz vs 1.2.3g",
            "12s 6pcs 1kg 1mg 1cl 0.1l",
            "Pe\u{f1}a-Reyes",
            "Para\u{f1}aque City, Metro Manila",
            "Nestl\u{e9} Nescaf\u{e9} 3-in-1",
            "Caf\u{65}\u{301} Espan\u{6e}\u{303}ol nin\u{6e}\u{303}o",
            "\u{c5}NGSTR\u{d6}M \u{d8}RSTED \u{160}KODA \u{17d}U\u{17d}U \u{e6}on \u{c6}ON",
            "\u{df}rasse 12,5 kg",
            "\u{130}stanbul",
            "\u{1e9e}RASSE",
            "\u{fb00}ame",
            "\u{1c5}ungla",
            "\u{3a3}\u{38a}\u{3a3}\u{3a5}\u{3a6}\u{39f}\u{3a3}",
            "\u{416}\u{423}\u{420}\u{41d}\u{410}\u{41b} \u{416}\u{443}\u{440}\u{43d}\u{430}\u{43b}",
            "\u{6771}\u{4eac}\u{90fd}\u{6e0b}\u{8c37}\u{533a} 100g",
            "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}",
            "\u{d55c}\u{ad6d}\u{c5b4} 500ml",
            "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629} 1.5l",
            "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940} \u{92a}\u{93e}\u{920}",
            "emoji \u{1f600} between \u{1f1f5}\u{1f1ed} tokens",
            "tab\tseparated\rvalues\nhere",
            "trailing separator 100g-",
            "-100g leading",
            "under_score dot.dot 1.a a.1",
            "12,345,678 units",
            "A1.5B 1-2-3 4.5.6",
            "\u{a0}nbsp\u{a0}bound\u{a0}",
            "\u{ff14}\u{ff12} fullwidth \u{ff44}\u{ff49}\u{ff47}\u{ff49}\u{ff54}\u{ff53}",
            "\u{2168} roman numeral",
            "\u{bd} vulgar fraction 2g",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        // A field far longer than any real one, mixing scripts so neither path is skipped.
        c.push("Brgy. San Jos\u{e9} Elementary School \u{d1}u\u{f1}oa 1.5L \u{6771}\u{4eac} ".repeat(2_000));
        // Every ASCII byte, so no character class is left untested.
        c.push((0u8..128).map(|b| b as char).collect());
        c
    }

    /// **The lane's whole contract.** The fast tokenizer must produce the byte-identical token
    /// stream the old one did — text, position and the numeric flag — or every ranking in the
    /// engine moves without anything failing.
    #[test]
    fn fast_tokenizer_matches_the_legacy_one() {
        for text in demanding_corpus() {
            let cut = text.len().min(120);
            assert_eq!(
                legacy::tokenize(&text),
                tokenize(&text),
                "token streams diverge for {:?}",
                &text[..text.char_indices().map(|(i, _)| i).take_while(|&i| i <= cut).last().unwrap_or(0)]
            );
        }
    }

    /// The fold is used to derive keys and slugs outside the tokenizer, so it is pinned separately.
    #[test]
    fn fast_fold_matches_the_legacy_fold() {
        for text in demanding_corpus() {
            assert_eq!(legacy::fold(&text), fold(&text), "fold diverges");
            // A prefix must fold the same way — the ASCII fast path runs over maximal runs, so a
            // run boundary must not be observable in the output.
            for cut in [1usize, 2, 3, 7, 13, 64] {
                if cut <= text.len() && text.is_char_boundary(cut) {
                    assert_eq!(legacy::fold(&text[..cut]), fold(&text[..cut]), "fold prefix");
                }
            }
        }
    }

    /// `parse_quantity` grew a fast rejection; it must reject and accept exactly what it did.
    #[test]
    fn quantity_parsing_matches_the_legacy_one() {
        let mut probe: Vec<String> = Vec::new();
        for text in demanding_corpus() {
            for t in legacy::tokenize(&text) {
                probe.push(t.text);
            }
        }
        for extra in [
            "1.5l", "1,5l", ",5l", ".5l", "-5l", "5", "l", "", " 300g ", "0.1l", "100ml", "1e3g",
            "\u{f1}5g", "5\u{f1}g", "1.2.3g", "300xyz", "\u{661}\u{662}\u{663}g",
        ] {
            probe.push(extra.to_string());
        }
        for t in probe {
            assert_eq!(legacy::parse_quantity(&t), parse_quantity(&t), "parse_quantity({t:?})");
        }
    }

    /// The alias pass grew a length gate and an in-place rewrite. Same output, every table.
    #[test]
    fn alias_application_matches_the_legacy_one() {
        let alias = AliasTable::philippine_grocery();
        let empty = AliasTable::new();
        for text in demanding_corpus() {
            for table in [&alias, &empty] {
                let mut want = tokenize(&text);
                for t in want.iter_mut() {
                    if !t.is_numeric {
                        if let Some(canon) = table.get(&t.text) {
                            t.text = canon.to_string();
                        }
                    }
                }
                let mut got = tokenize(&text);
                apply_alias(&mut got, table);
                assert_eq!(want, got, "alias application diverges");
            }
        }
    }

    /// The span tokenizer is a second implementation of the tokenizing rules, so it can drift from
    /// the first. It must not: a highlight that disagrees with what was indexed marks the wrong
    /// words. These pin them together over the shapes the rules actually bend for.
    #[test]
    fn tokenize_span_matches_tokenize() {
        for text in [
            "",
            "   ",
            "Colgate Total Toothpaste 150g",
            "Lucky Me Pancit Canton 60g",
            "1.5L Coke Zero",
            "MATH 30-23 section",
            "Cafe\u{301} Espan\u{303}ol nin\u{303}o",
            "Nesc\u{e1}fe 3-in-1",
            "500 ml bottle",
            "a,b.c-d",
            "\u{df}rasse 12,5 kg",
            "---",
            "9",
        ] {
            let plain = tokenize(text);
            let span: Vec<Token> = tokenize_span(text).into_iter().map(|(t, _, _)| t).collect();
            assert_eq!(plain, span, "token streams diverge for {text:?}");
        }
    }

    #[test]
    fn fold_agrees_with_fold_with_origin() {
        for text in ["", "Caf\u{e9}", "\u{df}", "ÅNGSTRÖM", "1.5L", "ni\u{f1}o"] {
            assert_eq!(fold(text), fold_with_origin(text).0, "fold diverges for {text:?}");
        }
    }

    /// Spans must index the ORIGINAL text, not the folded one -- folding is not length preserving.
    #[test]
    fn spans_point_into_the_original_text() {
        let text = "Caf\u{e9} Nesc\u{e1}fe 500 ml";
        for (t, a, b) in tokenize_span(text) {
            assert!(a <= b && b <= text.len(), "span {a}..{b} out of bounds for {text:?}");
            assert!(text.is_char_boundary(a) && text.is_char_boundary(b), "span not on a boundary");
            let raw = &text[a..b];
            assert!(!raw.is_empty(), "empty span for token {:?}", t.text);
        }
        let span = tokenize_span(text);
        assert_eq!(&text[span[0].1..span[0].2], "Caf\u{e9}", "the accented word keeps its bytes");
        // `500` + `ml` merge into one quantity token, and the span covers both halves.
        let last = span.last().unwrap();
        assert_eq!(&text[last.1..last.2], "500 ml");
    }

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
