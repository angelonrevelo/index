//! A minimal JSON reader — enough to address a change record, and no more.
//!
//! # Why this exists rather than `serde_json`
//!
//! `index-text` has two dependencies, both audited in `docs/research/build-or-buy.md`, and the CLI
//! is meant to be the thing you can drop on a server without a supply chain. More to the point,
//! **nothing here needs a general JSON library**: a change record is addressed by a handful of
//! dotted paths (`after.sku`, `op`) and every value is wanted as a string. Parsing to a full
//! document model and then walking it would be more code, not less.
//!
//! So this parses one line into a flat `path -> string` map, in a single pass, and never allocates
//! a tree. Nested objects contribute dotted paths; arrays are captured verbatim as their source
//! text, because a change stream that puts an array in a field wants it indexed as text, not
//! traversed.
//!
//! # What it deliberately does not do
//!
//! - No number typing: `1.5` arrives as `"1.5"`, which is what the analyzer wants anyway, and what
//!   `set_numeric_field` parses itself.
//! - No duplicate-key policy beyond last-wins.
//! - No streaming across lines: a record must be one line, which is what every change stream and
//!   `jq -c` produce.

use std::collections::HashMap;

/// Parse one JSON object into dotted paths. Returns `None` if it is not a well-formed object.
///
/// `null` is recorded as an absent path rather than as the string `"null"`: a null column and a
/// missing column mean the same thing to an index, and mapping one to the four-letter word `null`
/// would index that word.
pub fn flatten(line: &str) -> Option<HashMap<String, String>> {
    let b = line.as_bytes();
    let mut p = Parser { b, at: 0, out: HashMap::new() };
    p.ws();
    if p.peek()? != b'{' {
        return None;
    }
    p.object("")?;
    p.ws();
    // Trailing content means the line was not a single record, which is worth refusing: it is
    // usually a stream that was not newline-delimited after all.
    if p.at != b.len() {
        return None;
    }
    Some(p.out)
}

struct Parser<'a> {
    b: &'a [u8],
    at: usize,
    out: HashMap<String, String>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.at).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.at += 1)
    }

    /// `prefix` is the dotted path of the enclosing object, empty at the top level.
    fn object(&mut self, prefix: &str) -> Option<()> {
        self.eat(b'{')?;
        self.ws();
        if self.peek()? == b'}' {
            self.at += 1;
            return Some(());
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let path =
                if prefix.is_empty() { key } else { format!("{prefix}.{key}") };
            self.value(&path)?;
            self.ws();
            match self.peek()? {
                b',' => self.at += 1,
                b'}' => {
                    self.at += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    fn value(&mut self, path: &str) -> Option<()> {
        match self.peek()? {
            b'{' => self.object(path),
            // An array is kept as its source text. Traversing it would require an index in the
            // path grammar, and a change stream that ships an array wants it indexed as words.
            b'[' => {
                let start = self.at;
                self.skip_array()?;
                let raw = std::str::from_utf8(&self.b[start..self.at]).ok()?;
                self.out.insert(path.to_string(), raw.to_string());
                Some(())
            }
            b'"' => {
                let s = self.string()?;
                self.out.insert(path.to_string(), s);
                Some(())
            }
            _ => {
                let start = self.at;
                while matches!(
                    self.peek(),
                    Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9' | b'a'..=b'z')
                ) {
                    self.at += 1;
                }
                let raw = std::str::from_utf8(&self.b[start..self.at]).ok()?;
                if raw.is_empty() {
                    return None;
                }
                // `null` is an ABSENT path, not the string "null". A null column and a missing
                // column mean the same thing here, and recording the word would index it.
                if raw != "null" {
                    self.out.insert(path.to_string(), raw.to_string());
                }
                Some(())
            }
        }
    }

    /// Skip a balanced array, respecting strings so a `]` inside one does not end it.
    fn skip_array(&mut self) -> Option<()> {
        self.eat(b'[')?;
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek()? {
                b'"' => {
                    self.string()?;
                    continue;
                }
                b'[' | b'{' => depth += 1,
                b']' | b'}' => depth -= 1,
                _ => {}
            }
            self.at += 1;
        }
        Some(())
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let c = self.peek()?;
            self.at += 1;
            match c {
                b'"' => return Some(out),
                b'\\' => {
                    let e = self.peek()?;
                    self.at += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            // A surrogate pair must be rejoined, or a name outside the BMP arrives
                            // as two replacement characters and never matches its own query.
                            let ch = if (0xD800..0xDC00).contains(&hi) {
                                self.eat(b'\\')?;
                                self.eat(b'u')?;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return None;
                                }
                                let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                char::from_u32(c)?
                            } else {
                                char::from_u32(hi)?
                            };
                            out.push(ch);
                        }
                        _ => return None,
                    }
                }
                // Multi-byte UTF-8 passes through a byte at a time; the run is re-decoded below.
                _ => {
                    let start = self.at - 1;
                    let len = utf8_len(c)?;
                    self.at = start + len;
                    out.push_str(std::str::from_utf8(self.b.get(start..self.at)?).ok()?);
                }
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let s = std::str::from_utf8(self.b.get(self.at..self.at + 4)?).ok()?;
        self.at += 4;
        u32::from_str_radix(s, 16).ok()
    }
}

/// Byte length of the UTF-8 sequence starting with `c`.
fn utf8_len(c: u8) -> Option<usize> {
    match c {
        0x00..=0x7f => Some(1),
        0xc0..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf7 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(line: &str, path: &str) -> Option<String> {
        flatten(line)?.get(path).cloned()
    }

    #[test]
    fn flat_objects_and_dotted_paths() {
        let line = r#"{"op":"u","after":{"sku":"A-1","name":"Colgate","price":19.5}}"#;
        assert_eq!(get(line, "op").as_deref(), Some("u"));
        assert_eq!(get(line, "after.sku").as_deref(), Some("A-1"));
        assert_eq!(get(line, "after.name").as_deref(), Some("Colgate"));
        // Numbers arrive as text, which is what the analyzer and `set_numeric_field` both want.
        assert_eq!(get(line, "after.price").as_deref(), Some("19.5"));
        assert_eq!(get(line, "after.missing"), None);
    }

    /// `null` must be ABSENT, not the word "null" — otherwise a nullable column indexes a term
    /// that no document really contains and every null row matches a query for it.
    #[test]
    fn null_is_absent_rather_than_a_word() {
        let line = r#"{"sku":"A-1","brand":null}"#;
        let m = flatten(line).unwrap();
        assert_eq!(m.get("sku").map(String::as_str), Some("A-1"));
        assert!(!m.contains_key("brand"), "null must not become a value");
    }

    #[test]
    fn escapes_and_non_ascii_survive() {
        let line = r#"{"a":"line\nbreak","b":"quote\"inside","c":"café","d":"日本","e":"😀"}"#;
        assert_eq!(get(line, "a").as_deref(), Some("line\nbreak"));
        assert_eq!(get(line, "b").as_deref(), Some("quote\"inside"));
        assert_eq!(get(line, "c").as_deref(), Some("café"));
        assert_eq!(get(line, "d").as_deref(), Some("日本"));
        // A surrogate pair must be rejoined, or an emoji arrives as two broken halves.
        assert_eq!(get(line, "e").as_deref(), Some("😀"));
    }

    #[test]
    fn arrays_are_kept_verbatim_and_do_not_break_parsing() {
        let line = r#"{"tag":["a","b]c"],"sku":"A-1"}"#;
        assert_eq!(get(line, "tag").as_deref(), Some(r#"["a","b]c"]"#));
        assert_eq!(get(line, "sku").as_deref(), Some("A-1"), "a ] inside a string does not end the array");
    }

    #[test]
    fn malformed_input_is_refused_rather_than_guessed() {
        for bad in [
            "",
            "not json",
            "[1,2]",                      // not an object
            r#"{"a":1"#,                  // truncated
            r#"{"a":1} {"b":2}"#,         // two records on one line
            r#"{"a":}"#,
            r#"{a:1}"#,                   // unquoted key
        ] {
            assert!(flatten(bad).is_none(), "should have refused: {bad:?}");
        }
    }

    #[test]
    fn empty_object_and_nesting_depth() {
        assert_eq!(flatten("{}").unwrap().len(), 0);
        let line = r#"{"a":{"b":{"c":"deep"}}}"#;
        assert_eq!(get(line, "a.b.c").as_deref(), Some("deep"));
    }
}
