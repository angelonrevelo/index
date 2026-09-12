//! Reading rows from whatever a database's own client can print.
//!
//! This is the whole "works on any database" claim, and it is deliberately unglamorous: **if it can
//! print rows, it can feed this.** There is no driver, no connection string and no per-database
//! integration, because every database already ships a client that emits CSV, TSV or JSON, and
//! every change-data-capture tool already emits newline-delimited JSON.
//!
//! ```text
//! psql -c "COPY (SELECT ...) TO STDOUT (FORMAT csv)" | index build --csv  ...
//! sqlite3 -csv app.db "SELECT ..."                   | index build --csv  ...
//! mysql -B -e "SELECT ..."                           | index build --tsv  ...
//! mongoexport --type json                            | index build --jsonl ...
//! curl -s /api/products | jq -c '.[]'                | index build --jsonl ...
//! ```
//!
//! The cost of that choice is honest and worth stating: **the pipe is the integration**, so
//! pagination, restarts and credentials belong to whoever runs the command, and this tool cannot
//! resume a stream it did not start. What it gets in return is that a database nobody here has
//! heard of works on day one.
//!
//! # The one format decision
//!
//! A record is a `name -> value` map, never a positional tuple, because every downstream flag
//! (`--key`, `--facet`, `--field`) addresses columns by name. CSV and TSV get their names from a
//! header line; JSON gets them from its own keys, dotted for nesting. That is what lets one set of
//! flags serve all three.

use std::collections::HashMap;
use std::io::BufRead;

use crate::json;

/// How the incoming bytes are framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// RFC-4180-ish: quotes, doubled quotes, embedded newlines inside quotes.
    Csv,
    /// Tab-separated, no quoting. What `mysql -B` and `psql -At -F$'\t'` emit.
    Tsv,
    /// One JSON object per line. What every CDC tool and `jq -c` emit.
    Jsonl,
}

/// One row, addressed by column name (or dotted JSON path).
pub type Record = HashMap<String, String>;

/// Streaming reader over any of the three formats.
pub struct RowReader<R: BufRead> {
    src: R,
    format: Format,
    /// Column names for the delimited formats. `None` until the header is read.
    header: Option<Vec<String>>,
    /// Whether the first delimited line is a header. When false, columns are named `c0`, `c1`, ...
    has_header: bool,
    line_no: u64,
    /// Lines that were not valid UTF-8 and were repaired. See [`RowReader::lossy_line`].
    lossy: u64,
}

impl<R: BufRead> RowReader<R> {
    pub fn new(src: R, format: Format, has_header: bool) -> Self {
        RowReader { src, format, header: None, has_header, line_no: 0, lossy: 0 }
    }

    /// How many lines contained bytes that were not valid UTF-8 and were repaired.
    ///
    /// **Not zero in practice, and that is the point.** A real export meets Windows `cp1252`, a
    /// latin-1 dump, or a `TEXT` column holding bytes nobody ever decoded — SQLite in particular
    /// stores whatever it was handed. Aborting a four-million-row build because one row carries a
    /// stray `0x92` is the wrong answer for a tool whose claim is "any database", so invalid bytes
    /// become U+FFFD and are COUNTED, and the count is reported rather than swallowed.
    pub fn lossy_line(&self) -> u64 {
        self.lossy
    }

    /// Line number of the record last returned, for error messages that a human can act on.
    pub fn line_no(&self) -> u64 {
        self.line_no
    }

    /// Next record, or `None` at end of input. `Err` on a malformed record.
    ///
    /// A malformed record is an ERROR rather than a skip. A loader that silently drops rows it
    /// could not parse produces an index that is quietly missing data, and the only symptom is a
    /// search that does not find something — which is indistinguishable from the engine being bad
    /// at its job.
    pub fn next_row(&mut self) -> Result<Option<Record>, String> {
        loop {
            let Some(line) = self.read_logical_line()? else { return Ok(None) };
            if line.trim().is_empty() {
                continue;
            }
            match self.format {
                Format::Jsonl => {
                    let m = json::flatten(&line).ok_or_else(|| {
                        format!("line {}: not a JSON object", self.line_no)
                    })?;
                    return Ok(Some(m));
                }
                Format::Csv | Format::Tsv => {
                    let cell = match self.format {
                        Format::Csv => split_csv(&line),
                        _ => line.split('\t').map(str::to_string).collect(),
                    };
                    if self.header.is_none() {
                        self.header = Some(match self.has_header {
                            true => cell.iter().map(|c| c.trim().to_string()).collect(),
                            // Positional names, so `--key c0` still works with no header.
                            false => (0..cell.len()).map(|i| format!("c{i}")).collect(),
                        });
                        if self.has_header {
                            continue;
                        }
                    }
                    let name = self.header.as_ref().expect("header set above");
                    if cell.len() != name.len() {
                        return Err(format!(
                            "line {}: {} columns, header has {} — a ragged row would put values \
                             under the wrong names",
                            self.line_no,
                            cell.len(),
                            name.len()
                        ));
                    }
                    return Ok(Some(
                        name.iter().cloned().zip(cell).filter(|(_, v)| !v.is_empty()).collect(),
                    ));
                }
            }
        }
    }

    /// One logical line. For CSV that may span several physical lines, because a quoted field may
    /// contain a newline -- an address column routinely does.
    ///
    /// Reads BYTES and repairs invalid UTF-8 rather than requiring it. See
    /// [`RowReader::lossy_line`] for why: the alternative is that one bad byte anywhere in a very
    /// large export destroys the whole build, which is what this did when it was first pointed at
    /// twenty-seven real SQLite databases.
    fn read_logical_line(&mut self) -> Result<Option<String>, String> {
        let mut buf = String::new();
        loop {
            let mut raw: Vec<u8> = Vec::new();
            let n = self.src.read_until(b'\n', &mut raw).map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok((!buf.is_empty()).then_some(buf));
            }
            self.line_no += 1;
            while matches!(raw.last(), Some(b'\n' | b'\r')) {
                raw.pop();
            }
            match String::from_utf8_lossy(&raw) {
                std::borrow::Cow::Borrowed(s) => buf.push_str(s),
                std::borrow::Cow::Owned(s) => {
                    self.lossy += 1;
                    buf.push_str(&s);
                }
            }
            if self.format != Format::Csv || quotes_balanced(&buf) {
                return Ok(Some(buf));
            }
            buf.push('\n');
        }
    }
}

/// A reader is an iterator of records, so `for rec in reader` works and an `Err` item is the
/// loud stop the malformed-record policy calls for.
impl<R: BufRead> Iterator for RowReader<R> {
    type Item = Result<Record, String>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_row() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// Whether a CSV line has an even number of unescaped quotes, i.e. no field is still open.
fn quotes_balanced(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    let mut open = false;
    while i < b.len() {
        if b[i] == b'"' {
            if open && b.get(i + 1) == Some(&b'"') {
                i += 2;
                continue;
            }
            open = !open;
        }
        i += 1;
    }
    !open
}

/// RFC-4180-ish splitter: honours double quotes and doubled quotes inside them.
///
/// The naive `split(',')` shreds any corpus with a comma in an address, and this project has
/// already been bitten by exactly that — `scale` carries the same note about the DepEd masterlist.
pub fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        match (c, quoted) {
            ('"', true) if it.peek() == Some(&'"') => {
                cur.push('"');
                it.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read(text: &str, f: Format, header: bool) -> Result<Vec<Record>, String> {
        let r = RowReader::new(Cursor::new(text.to_string()), f, header);
        let mut out = Vec::new();
        for rec in r {
            out.push(rec?);
        }
        Ok(out)
    }

    #[test]
    fn csv_with_a_header_names_its_columns() {
        let rows = read("sku,name\nA-1,Colgate Total\nA-2,Aquafresh Mini\n", Format::Csv, true)
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["sku"], "A-1");
        assert_eq!(rows[1]["name"], "Aquafresh Mini");
    }

    /// A comma inside a quoted field must not split it. `psql COPY ... FORMAT csv` quotes exactly
    /// this way, and a naive splitter shreds every address column in the corpus.
    #[test]
    fn quoted_commas_and_embedded_newlines_survive() {
        let text = "sku,addr\nA-1,\"12 Mabini St, Makati\"\nA-2,\"Line one\nLine two\"\n";
        let rows = read(text, Format::Csv, true).unwrap();
        assert_eq!(rows.len(), 2, "an embedded newline is one row, not two");
        assert_eq!(rows[0]["addr"], "12 Mabini St, Makati");
        assert_eq!(rows[1]["addr"], "Line one\nLine two");
    }

    #[test]
    fn doubled_quotes_are_one_quote() {
        let rows = read("a\n\"say \"\"hi\"\"\"\n", Format::Csv, true).unwrap();
        assert_eq!(rows[0]["a"], r#"say "hi""#);
    }

    #[test]
    fn tsv_is_what_mysql_batch_mode_prints() {
        let rows = read("sku\tname\nA-1\tColgate Total\n", Format::Tsv, true).unwrap();
        assert_eq!(rows[0]["sku"], "A-1");
        assert_eq!(rows[0]["name"], "Colgate Total");
    }

    #[test]
    fn headerless_columns_are_addressable_positionally() {
        let rows = read("A-1,Colgate\n", Format::Csv, false).unwrap();
        assert_eq!(rows[0]["c0"], "A-1");
        assert_eq!(rows[0]["c1"], "Colgate");
    }

    #[test]
    fn jsonl_records_use_dotted_paths() {
        let rows =
            read("{\"op\":\"u\",\"after\":{\"sku\":\"A-1\"}}\n", Format::Jsonl, false).unwrap();
        assert_eq!(rows[0]["op"], "u");
        assert_eq!(rows[0]["after.sku"], "A-1");
    }

    /// A ragged row is an ERROR, not a skip and not a shift. Silently dropping it builds an index
    /// quietly missing data, whose only symptom is a search that does not find something.
    #[test]
    fn a_ragged_row_is_an_error_rather_than_a_silent_shift() {
        let err = read("sku,name\nA-1\n", Format::Csv, true).unwrap_err();
        assert!(err.contains("columns"), "{err}");
        assert!(read("{bad}\n", Format::Jsonl, false).is_err());
    }

    /// **Real databases contain bytes that are not UTF-8**, and aborting the whole build over one
    /// of them is the wrong answer for a tool whose claim is "any database". Twenty-seven real
    /// SQLite dumps on this machine failed exactly that way before this was fixed.
    ///
    /// The bytes below are `caf` + 0xE9 (`cafe` in latin-1/cp1252), which is what a non-UTF-8
    /// export of a perfectly ordinary row looks like.
    #[test]
    fn invalid_utf8_is_repaired_and_counted_rather_than_fatal() {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(b"sku,name\n");
        raw.extend_from_slice(b"A-1,caf\xe9 latte\n");
        raw.extend_from_slice(b"A-2,plain tea\n");

        let mut r = RowReader::new(Cursor::new(raw), Format::Csv, true);
        let mut got = Vec::new();
        for rec in &mut r {
            got.push(rec.expect("a bad byte must not abort the stream"));
        }
        assert_eq!(got.len(), 2, "both rows survive");
        assert_eq!(got[1]["name"], "plain tea", "a clean row is untouched");
        assert!(got[0]["name"].contains("latte"), "the readable part of the bad row survives");
        assert!(got[0]["name"].contains('\u{fffd}'), "the bad byte became U+FFFD");
        assert_eq!(r.lossy_line(), 1, "and it is COUNTED, so the caller can report it");
    }

    #[test]
    fn blank_lines_are_skipped_and_empty_cells_are_absent() {
        let rows = read("sku,name\n\nA-1,\n", Format::Csv, true).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["sku"], "A-1");
        assert!(!rows[0].contains_key("name"), "an empty cell is an absent value");
    }
}
