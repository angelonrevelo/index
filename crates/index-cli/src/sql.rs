//! Reading rows from a SQL dump — the fourth input format.
//!
//! CSV, TSV and JSONL cover "a client prints rows". `--sql` covers **the dump a database already
//! wrote**: `pg_dump --inserts` / `--column-inserts`, a Supabase `seed.sql`, `sqlite3 .dump`,
//! `mysqldump`. Every one of those is a file sitting in a repo or a backup directory, already full
//! of rows — `INSERT INTO table (cols) VALUES (...), (...);` — and no client needs to run for them
//! to be indexed. Plain `pg_dump` (the default, `COPY ... FROM stdin` blocks) is read too, because
//! it is the dump format most likely to exist.
//!
//! ```text
//! index build -d data/ --sql --table product \
//!   --schema 'sku:0:0.6,name:3:0.4' --key sku < dump.sql
//! ```
//!
//! # What is parsed, and what is honestly not
//!
//! Parsed: quoted and bare identifiers (Postgres `"…"`, MySQL `` `…` ``), schema-qualified tables,
//! `OVERRIDING SYSTEM VALUE`, multi-row `VALUES`, single-quote strings with `''` doubling,
//! `E'…'` backslash escapes (including octal and `\xHHHH`), dollar-quoted strings, `::type` casts,
//! plain numeric/boolean/`NULL` literals, `name(args)` calls kept as their literal text, `ON
//! CONFLICT` / `RETURNING` tails, `--` and `/* */` comments, and `COPY t (cols) FROM stdin;`
//! data blocks ending at `\.`. Any other statement is skipped, not parsed.
//!
//! Not parsed, on purpose: backslash escapes inside a *plain* `'…'` string. That is MySQL dump
//! dialect, and accepting it would corrupt a Postgres dump holding a Windows path (`'C:\tmp'`
//! would grow a tab). MySQL has a printing client — `mysql -B` — so its pipe story is the TSV
//! one; the dump reader stays on SQL-standard semantics and says so here.
//!
//! A dump is a snapshot of *literals*, not of evaluated expressions: `now()` arrives as the text
//! `now()`, which is what the file holds. Columns come from the INSERT/COPY column list; an
//! `INSERT` with no column list names its columns `c0, c1, …`. `NULL` arrives as an empty value,
//! exactly as a CSV export prints it. Every other statement is skipped, and the counts are
//! reported so a table that did not make it in is visible, not silent.

use std::collections::{HashMap, VecDeque};
use std::io::BufRead;

/// One row, addressed by column name. Same type the pipe formats produce.
pub type Record = HashMap<String, String>;

/// Streaming reader over a SQL dump. Yields one row per `INSERT` tuple or `COPY` data line.
pub struct SqlDumpReader<R: BufRead> {
    src: R,
    /// Lowercase table names to keep — bare (`product`) or qualified (`public.product`).
    /// Empty means every `INSERT`/`COPY` in the dump.
    tables: Vec<String>,
    /// Pending characters of the current line. Refilled from `src` line by line; SQL words never
    /// span lines, so all keyword lookahead can stop here.
    queue: VecDeque<char>,
    /// A line terminator was stripped at this queue boundary and not yet served. It behaves as one
    /// virtual `'\n'` that `bump` yields after the queue drains — so a string literal or a
    /// dollar quote spanning a physical line keeps its newline, which `refill` would otherwise
    /// silently delete.
    pending_nl: bool,
    line_no: u64,
    lossy: u64,
    eof: bool,
    /// Column names of the `COPY` block currently being read, if any. Data lines do not share the
    /// statement grammar — a `\N` or a tab inside them is data — so they bypass `skip_trivia`.
    copy_cols: Option<Vec<String>>,
    /// An `INSERT` mid-statement: its columns, and whether a tuple is already in progress.
    insert: Option<InsertState>,
    statements: u64,
    skipped: u64,
    /// Rows from tables the filter excluded, counted so the exclusion is visible.
    filtered: u64,
}

#[derive(Default)]
struct InsertState {
    cols: Vec<String>,
    /// False while tuples remain; flipped once the statement's `;` is consumed.
    open: bool,
}

impl<R: BufRead> SqlDumpReader<R> {
    pub fn new(src: R, tables: &[String]) -> Self {
        SqlDumpReader {
            src,
            tables: tables.iter().map(|t| t.to_lowercase()).collect(),
            queue: VecDeque::new(),
            pending_nl: false,
            line_no: 0,
            lossy: 0,
            eof: false,
            copy_cols: None,
            insert: None,
            statements: 0,
            skipped: 0,
            filtered: 0,
        }
    }

    /// Lines that contained bytes that were not valid UTF-8 and were repaired.
    /// Same policy as [`crate::row::RowReader`]: counted, reported, never fatal.
    pub fn lossy_line(&self) -> u64 {
        self.lossy
    }

    /// Line number of the row last returned, for error messages a human can act on.
    pub fn line_no(&self) -> u64 {
        self.line_no
    }

    /// `INSERT`/`COPY` statements rows were read from.
    pub fn statements(&self) -> u64 {
        self.statements
    }

    /// Statements skipped as not-row-bearing (`CREATE`, `SET`, `INSERT … SELECT`, `COPY … FROM 'file'`).
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Rows dropped by the `--table` filter.
    pub fn filtered(&self) -> u64 {
        self.filtered
    }

    /// Next row, or `None` at end of dump. A malformed row is an ERROR, not a skip: a loader that
    /// silently drops rows produces an index quietly missing data, which is indistinguishable from
    /// the engine being bad at its job.
    pub fn next_row(&mut self) -> Result<Option<Record>, String> {
        loop {
            // A COPY data block owns the stream verbatim: every line up to `\.` is a row.
            if let Some(cols) = self.copy_cols.clone() {
                return self.copy_row(&cols);
            }
            // Continue an INSERT that still has tuples.
            if let Some(st) = &self.insert {
                let open = st.open;
                if open {
                    return self.insert_row();
                }
                self.insert = None;
            }
            self.skip_trivia()?;
            if self.peek()?.is_none() {
                return Ok(None);
            }
            let word = match self.peek_word()? {
                Some(w) => w,
                None => {
                    // Not a word start (stray punctuation); skip the statement to stay in sync.
                    self.skip_to_semicolon()?;
                    self.skipped += 1;
                    continue;
                }
            };
            match word.to_ascii_lowercase().as_str() {
                "insert" => {
                    self.consume_word("insert")?;
                    let (full, bare, cols) = self.parse_insert_head()?;
                    if !self.wanted(&full, &bare) {
                        self.skip_to_semicolon()?;
                        self.skipped += 1;
                        self.filtered += 1;
                        continue;
                    }
                    match cols {
                        Some(cols) => {
                            self.statements += 1;
                            self.insert = Some(InsertState { cols, open: true });
                        }
                        // INSERT … SELECT / DEFAULT VALUES: no literal rows to read.
                        None => {
                            self.skip_to_semicolon()?;
                            self.skipped += 1;
                        }
                    }
                }
                "copy" => {
                    self.consume_word("copy")?;
                    let (full, bare, stdin, cols) = self.parse_copy_head()?;
                    if !self.wanted(&full, &bare) {
                        self.skip_to_semicolon()?;
                        self.skipped += 1;
                        self.filtered += 1;
                        continue;
                    }
                    if !stdin {
                        // COPY from a file or a program carries no inline data.
                        self.skip_to_semicolon()?;
                        self.skipped += 1;
                        continue;
                    }
                    // Consume the header's `;` (and any WITH options) BEFORE raw data begins:
                    // data lines bypass the statement grammar entirely. The rest of the header
                    // line — at minimum its terminator — is skipped with them, so the first data
                    // row starts clean.
                    self.skip_to_semicolon()?;
                    self.skip_rest_of_line()?;
                    self.statements += 1;
                    // A COPY without a column list still has data; its rows name columns
                    // positionally, decided by the first row's arity.
                    self.copy_cols = Some(cols.unwrap_or_default());
                }
                _ => {
                    self.skip_to_semicolon()?;
                    self.skipped += 1;
                }
            }
        }
    }

    // ---- COPY data blocks ------------------------------------------------------------------------------

    fn copy_row(&mut self, cols: &[String]) -> Result<Option<Record>, String> {
        let line = match self.raw_line()? {
            Some(l) => l,
            None => {
                // EOF mid-COPY: the dump is truncated. Refusing beats guessing.
                return Err("dump ended inside a COPY block (no \\.)".to_string());
            }
        };
        if line == "\\." {
            self.copy_cols = None;
            return self.next_row();
        }
        let cells: Vec<String> = line.split('\t').map(unescape_copy_cell).collect();
        let names: Vec<String> = if cols.is_empty() {
            // Positional columns, decided ONCE by the first row's arity and remembered, so a
            // ragged later row is an error rather than a different set of names.
            let n: Vec<String> = (0..cells.len()).map(|i| format!("c{i}")).collect();
            self.copy_cols = Some(n.clone());
            n
        } else {
            cols.to_vec()
        };
        if cells.len() != names.len() {
            return Err(format!(
                "line {}: COPY row has {} fields, header has {} — a ragged row would put values \
                 under the wrong names",
                self.line_no,
                cells.len(),
                names.len()
            ));
        }
        Ok(Some(
            names
                .into_iter()
                .zip(cells)
                .filter(|(_, v)| !v.is_empty())
                .collect(),
        ))
    }

    // ---- INSERT statements ------------------------------------------------------------------------------

    /// After the word `insert`: the table, optional OVERRIDING, optional column list, and whether
    /// `VALUES` (literal rows) follows. Returns `(qualified, bare, columns)`; `columns` is `None`
    /// when the statement has no literal rows to read.
    fn parse_insert_head(&mut self) -> Result<(String, String, Option<Vec<String>>), String> {
        self.skip_trivia()?;
        if !self.take_word("into")? {
            return Err(format!("line {}: INSERT without INTO", self.line_no));
        }
        let (full, bare) = self.qualified_ident()?;
        self.skip_trivia()?;
        let mut cols = if self.peek()? == Some('(') {
            self.bump()?;
            Some(self.ident_list()?)
        } else {
            None
        };
        self.skip_trivia()?;
        if self.take_word("overriding")? {
            self.skip_trivia()?;
            let _ = self.take_word("system")? || self.take_word("user")?;
            self.skip_trivia()?;
            let _ = self.take_word("value")? || self.take_word("values")?;
            self.skip_trivia()?;
            // The grammar puts the column list before OVERRIDING; accept it after too rather
            // than misread a dialect that writes it there.
            if cols.is_none() && self.peek()? == Some('(') {
                self.bump()?;
                cols = Some(self.ident_list()?);
                self.skip_trivia()?;
            }
        }
        if self.take_word("values")? {
            // A column list may be absent (`sqlite3 .dump` style); an empty list means the rows
            // name their columns positionally, c0, c1, …
            return Ok((full, bare, Some(cols.unwrap_or_default())));
        }
        // SELECT / TABLE / DEFAULT VALUES: nothing literal follows.
        Ok((full, bare, None))
    }

    /// One tuple of the INSERT in `self.insert`. Row emission is one call per tuple, so a
    /// ten-million-row dump streams with constant memory.
    fn insert_row(&mut self) -> Result<Option<Record>, String> {
        let cols = self.insert.as_ref().expect("insert state").cols.clone();
        self.skip_trivia()?;
        if self.peek()? == Some(',') {
            self.bump()?;
            self.skip_trivia()?;
        }
        if self.peek()?.is_none() || self.peek()? == Some(';') {
            // Statement ended without another tuple (or the file did).
            if self.peek()? == Some(';') {
                self.bump()?;
            }
            self.insert = None;
            return self.next_row();
        }
        if self.peek()? != Some('(') {
            // ON CONFLICT / RETURNING after the last tuple: consume to `;`, then keep serving the
            // outer loop (which sees the INSERT as closed).
            self.insert = None;
            self.skip_to_semicolon()?;
            return self.next_row();
        }
        self.bump()?;
        let mut values: Vec<String> = Vec::new();
        loop {
            self.skip_trivia()?;
            values.push(self.value()?);
            self.skip_trivia()?;
            match self.bump()? {
                Some(',') => continue,
                Some(')') => break,
                other => {
                    return Err(format!(
                        "line {}: expected , or ) in VALUES, got {:?}",
                        self.line_no,
                        other
                    ))
                }
            }
        }
        if !cols.is_empty() && values.len() != cols.len() {
            return Err(format!(
                "line {}: VALUES row has {} entries, column list has {} — a ragged row would put \
                 values under the wrong names",
                self.line_no,
                values.len(),
                cols.len()
            ));
        }
        // Statement stays open while a `,` follows the tuple; closed on `;` or a clause tail.
        self.skip_trivia()?;
        match self.peek()? {
            Some(',') => {}
            Some(';') => {
                self.bump()?;
                self.insert.as_mut().expect("insert state").open = false;
            }
            Some(_) => {
                self.insert = None;
                self.skip_to_semicolon()?;
            }
            None => self.insert = None,
        }
        let names: Vec<String> = if cols.is_empty() {
            (0..values.len()).map(|i| format!("c{i}")).collect()
        } else {
            cols
        };
        Ok(Some(
            names.into_iter().zip(values).filter(|(_, v)| !v.is_empty()).collect(),
        ))
    }

    /// One expression from a VALUES list, decoded to its literal text.
    fn value(&mut self) -> Result<String, String> {
        let v = match self.peek()? {
            Some('\'') => {
                self.bump()?;
                self.string_literal(false)?
            }
            Some('$') => self.dollar_string()?,
            Some(c) if c.is_ascii_digit() || c == '.' => self.number()?,
            Some(c) if c == '-' || c == '+' => {
                self.bump()?;
                let sign = c;
                match self.peek()? {
                    Some(d) if d.is_ascii_digit() => format!("{sign}{}", self.number()?),
                    _ => return Err(format!("line {}: stray {sign:?} in VALUES", self.line_no)),
                }
            }
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let w = self.read_word()?;
                match self.peek()? {
                    Some('\'') if matches!(w.as_str(), "e" | "E" | "u" | "U") => {
                        self.bump()?;
                        self.string_literal(true)?
                    }
                    Some('&') if matches!(w.as_str(), "u" | "U") => {
                        self.bump()?;
                        if self.peek()? == Some('\'') {
                            self.bump()?;
                        }
                        self.string_literal(true)?
                    }
                    Some('(') => {
                        // A call like now() or gen_random_uuid(): keep the expression text, which
                        // is what the dump holds. A dump is literals, not evaluated expressions.
                        let mut text = w;
                        text.push('(');
                        self.bump()?;
                        self.consume_balanced(&mut text)?;
                        text
                    }
                    _ => match w.to_ascii_lowercase().as_str() {
                        // NULL arrives empty, exactly as a CSV export prints it; booleans fold
                        // lowercase so a facet built from them is stable.
                        "null" => String::new(),
                        "true" => "true".to_string(),
                        "false" => "false".to_string(),
                        _ => w,
                    },
                }
            }
            other => {
                return Err(format!(
                    "line {}: unexpected {:?} in VALUES",
                    self.line_no,
                    other
                ))
            }
        };
        // ::casts — the type name is consumed and discarded; the value is what preceded it.
        self.skip_trivia()?;
        if self.peek()? == Some(':') {
            self.bump()?;
            if self.peek()? == Some(':') {
                self.bump()?;
                self.skip_type_name()?;
            } else {
                return Err(format!("line {}: stray ':' in VALUES", self.line_no));
            }
        }
        Ok(v)
    }

    /// `'…'` with `''` doubling; with `escapes`, backslash sequences decode like Postgres `E''`.
    fn string_literal(&mut self, escapes: bool) -> Result<String, String> {
        let mut out = String::new();
        loop {
            match self.bump()? {
                None => return Err("unterminated string literal".to_string()),
                Some('\'') => {
                    if self.peek()? == Some('\'') {
                        self.bump()?;
                        out.push('\'');
                    } else {
                        return Ok(out);
                    }
                }
                Some('\\') if escapes => {
                    let c = self.bump()?.ok_or("unterminated string literal")?;
                    match c {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'v' => out.push('\u{b}'),
                        'x' => {
                            let mut hex = String::new();
                            while let Some(h) = self.peek()? {
                                if h.is_ascii_hexdigit() && hex.len() < 6 {
                                    hex.push(h);
                                    self.bump()?;
                                } else {
                                    break;
                                }
                            }
                            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                out.push(ch);
                            }
                        }
                        '0'..='7' => {
                            let mut oct = c.to_digit(8).unwrap_or(0);
                            while let Some(o) = self.peek()? {
                                let d = o.to_digit(8).unwrap_or(8);
                                if o.is_ascii_digit() && d < 8 && oct < 32 {
                                    oct = oct * 8 + d;
                                    self.bump()?;
                                } else {
                                    break;
                                }
                            }
                            if let Some(ch) = char::from_u32(oct) {
                                out.push(ch);
                            }
                        }
                        other => out.push(other),
                    }
                }
                Some(c) => out.push(c),
            }
        }
    }

    /// `$tag$ … $tag$`, the quoting pg_dump uses for anything containing quotes. The opening `$`
    /// has been peeked, not consumed, so it is read here as the tag's first character.
    fn dollar_string(&mut self) -> Result<String, String> {
        match self.bump()? {
            Some('$') => {}
            _ => return Err("unterminated dollar quote".to_string()),
        }
        let mut tag = String::from("$");
        loop {
            match self.bump()? {
                None => return Err("unterminated dollar quote".to_string()),
                Some('$') => {
                    tag.push('$');
                    break;
                }
                Some(c) => tag.push(c),
            }
        }
        let close: Vec<char> = tag.chars().collect();
        let mut win: VecDeque<char> = VecDeque::new();
        let mut out = String::new();
        loop {
            match self.bump()? {
                None => return Err("unterminated dollar quote".to_string()),
                Some(c) => {
                    out.push(c);
                    win.push_back(c);
                    if win.len() > close.len() {
                        win.pop_front();
                    }
                    if win.iter().copied().eq(close.iter().copied()) {
                        out.truncate(out.len() - close.len());
                        return Ok(out);
                    }
                }
            }
        }
    }

    fn number(&mut self) -> Result<String, String> {
        let mut out = String::new();
        while let Some(c) = self.peek()? {
            if c.is_ascii_digit() || c == '.' {
                out.push(c);
                self.bump()?;
            } else {
                break;
            }
        }
        // Exponent, if this is one: `1e-5`.
        if matches!(self.peek()?, Some('e') | Some('E')) {
            let save = out.len();
            out.push(self.bump()?.expect("peeked"));
            if matches!(self.peek()?, Some('+') | Some('-')) {
                out.push(self.bump()?.expect("peeked"));
            }
            match self.peek()? {
                Some(d) if d.is_ascii_digit() => {}
                _ => out.truncate(save), // it was an ident character, not an exponent
            }
            while let Some(d) = self.peek()? {
                if d.is_ascii_digit() {
                    out.push(d);
                    self.bump()?;
                } else {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// After `::`: a (possibly qualified, possibly parameterised, possibly array) type name.
    fn skip_type_name(&mut self) -> Result<(), String> {
        loop {
            self.skip_spaces()?;
            match self.peek()? {
                Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '"' => {
                    let _ = self.ident()?;
                }
                Some('.') => {
                    self.bump()?;
                }
                Some('[') => {
                    self.bump()?;
                    if self.peek()? == Some(']') {
                        self.bump()?;
                    }
                }
                Some('(') => {
                    self.bump()?;
                    let mut t = String::new();
                    self.consume_balanced(&mut t)?;
                }
                _ => return Ok(()),
            }
        }
    }

    // ---- shared lexing -----------------------------------------------------------------------------------

    /// Whitespace, `--` line comments and `/* */` block comments.
    fn skip_trivia(&mut self) -> Result<(), String> {
        loop {
            match self.peek()? {
                Some(c) if c.is_whitespace() => {
                    self.bump()?;
                }
                Some('-') if self.peek_at(1)? == Some('-') => {
                    // A line comment; a lone `-` is an expression, not a comment.
                    self.skip_rest_of_line()?;
                }
                Some('/') if self.peek_at(1)? == Some('*') => {
                    self.bump()?;
                    self.bump()?;
                    let mut depth = 1;
                    while depth > 0 {
                        match self.bump()? {
                            None => return Ok(()),
                            Some('*') if self.peek()? == Some('/') => {
                                self.bump()?;
                                depth -= 1;
                            }
                            Some('/') if self.peek()? == Some('*') => {
                                self.bump()?;
                                depth += 1;
                            }
                            _ => {}
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// Whitespace only (used inside type names, where `--` is an operator).
    fn skip_spaces(&mut self) -> Result<(), String> {
        while matches!(self.peek()?, Some(c) if c.is_whitespace()) {
            self.bump()?;
        }
        Ok(())
    }

    /// Consume to the statement's `;`, honouring strings, dollar quotes, identifiers and comments
    /// so a semicolon inside any of them does not end the skip.
    fn skip_to_semicolon(&mut self) -> Result<(), String> {
        let mut depth = 0i32;
        loop {
            match self.peek()? {
                None => return Ok(()),
                Some(';') if depth == 0 => {
                    self.bump()?;
                    return Ok(());
                }
                Some('(') => {
                    self.bump()?;
                    depth += 1;
                }
                Some(')') => {
                    self.bump()?;
                    depth -= 1;
                }
                Some('\'') => {
                    self.bump()?;
                    let _ = self.string_literal(false)?;
                }
                Some('"') => {
                    let _ = self.ident()?;
                }
                Some('`') => {
                    let _ = self.ident()?;
                }
                Some('$') => {
                    // A dollar quote has a letter after the `$`; a bare `$` does not.
                    let mut tag = String::from("$");
                    let mut i = 1;
                    while let Some(c) = self.peek_at(i)? {
                        if c == '$' {
                            for _ in 0..i + 1 {
                                self.bump()?;
                            }
                            let _ = self.dollar_tag_string(&tag)?;
                            break;
                        }
                        if c.is_ascii_alphanumeric() || c == '_' {
                            tag.push(c);
                            i += 1;
                        } else {
                            self.bump()?;
                            break;
                        }
                    }
                }
                Some('-') if self.peek_at(1)? == Some('-') => {
                    self.skip_rest_of_line()?;
                }
                Some('/') if self.peek_at(1)? == Some('*') => {
                    self.skip_trivia()?;
                }
                _ => {
                    self.bump()?;
                }
            }
        }
    }

    /// Dollar-quoted string whose opening tag is already known (used while skipping).
    fn dollar_tag_string(&mut self, tag: &str) -> Result<String, String> {
        let close: Vec<char> = format!("{tag}$").chars().collect();
        let mut win: VecDeque<char> = VecDeque::new();
        loop {
            match self.bump()? {
                None => return Err("unterminated dollar quote".to_string()),
                Some(c) => {
                    win.push_back(c);
                    if win.len() > close.len() {
                        win.pop_front();
                    }
                    if win.iter().copied().eq(close.iter().copied()) {
                        return Ok(String::new());
                    }
                }
            }
        }
    }

    /// Everything up to the matching `)`, appended verbatim (for call text and type parameters).
    fn consume_balanced(&mut self, text: &mut String) -> Result<(), String> {
        let mut depth = 1;
        while depth > 0 {
            match self.bump()? {
                None => return Err("unterminated parenthesis".to_string()),
                Some('\'') => {
                    text.push('\'');
                    text.push_str(&self.string_literal(false)?);
                    text.push('\'');
                    continue;
                }
                Some(c) => {
                    if c == '(' {
                        depth += 1;
                    }
                    if c == ')' {
                        depth -= 1;
                        text.push(c);
                        if depth == 0 {
                            return Ok(());
                        }
                        continue;
                    }
                    text.push(c);
                }
            }
        }
        Ok(())
    }

    /// `schema."Table".x` — bare parts fold to lowercase, quoted parts keep their case.
    fn qualified_ident(&mut self) -> Result<(String, String), String> {
        let mut parts: Vec<String> = Vec::new();
        loop {
            self.skip_trivia()?;
            match self.ident()? {
                Some(p) => parts.push(p),
                None => break,
            }
            if self.peek()? == Some('.') {
                self.bump()?;
            } else {
                break;
            }
        }
        if parts.is_empty() {
            return Err(format!("line {}: expected a table name", self.line_no));
        }
        let bare = parts.last().expect("non-empty").clone();
        Ok((parts.join("."), bare))
    }

    /// One identifier: `"quoted"` (with `""` doubling), `` `backticked` ``, or bare (lowercased).
    fn ident(&mut self) -> Result<Option<String>, String> {
        match self.peek()? {
            Some('"') => {
                self.bump()?;
                let mut out = String::new();
                loop {
                    match self.bump()? {
                        None => return Err("unterminated quoted identifier".to_string()),
                        Some('"') => {
                            if self.peek()? == Some('"') {
                                self.bump()?;
                                out.push('"');
                            } else {
                                return Ok(Some(out));
                            }
                        }
                        Some(c) => out.push(c),
                    }
                }
            }
            Some('`') => {
                self.bump()?;
                let mut out = String::new();
                loop {
                    match self.bump()? {
                        None => return Err("unterminated quoted identifier".to_string()),
                        Some('`') => {
                            if self.peek()? == Some('`') {
                                self.bump()?;
                                out.push('`');
                            } else {
                                return Ok(Some(out));
                            }
                        }
                        Some(c) => out.push(c),
                    }
                }
            }
            Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '$' => {
                let mut out = String::new();
                while let Some(c) = self.peek()? {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                        out.push(c);
                        self.bump()?;
                    } else {
                        break;
                    }
                }
                Ok(Some(out.to_lowercase()))
            }
            _ => Ok(None),
        }
    }

    /// `( "a", b, c )` — a comma-separated identifier list.
    fn ident_list(&mut self) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        loop {
            self.skip_trivia()?;
            match self.ident()? {
                Some(p) => out.push(p),
                None => {
                    return Err(format!(
                        "line {}: expected a column name in the list",
                        self.line_no
                    ))
                }
            }
            self.skip_trivia()?;
            match self.bump()? {
                Some(',') => continue,
                Some(')') => return Ok(out),
                other => {
                    return Err(format!(
                        "line {}: expected , or ) in the column list, got {:?}",
                        self.line_no,
                        other
                    ))
                }
            }
        }
    }

    // ---- COPY headers ------------------------------------------------------------------------------------

    /// After the word `copy`: table, optional column list, and whether `FROM stdin` follows
    /// (only then do raw data lines follow). `(qualified, bare, from_stdin, columns)`.
    fn parse_copy_head(&mut self) -> Result<CopyHead, String> {
        self.skip_trivia()?;
        let (full, bare) = self.qualified_ident()?;
        self.skip_trivia()?;
        let cols = if self.peek()? == Some('(') {
            self.bump()?;
            Some(self.ident_list()?)
        } else {
            None
        };
        self.skip_trivia()?;
        if !self.take_word("from")? {
            return Ok((full, bare, false, None));
        }
        self.skip_trivia()?;
        match self.peek()? {
            // `stdin` is a bare word; a file name is a string literal, `program` a bare word.
            Some('s') | Some('S') => {
                if !self.take_word("stdin")? {
                    return Ok((full, bare, false, None));
                }
            }
            _ => return Ok((full, bare, false, None)),
        }
        Ok((full, bare, true, cols))
    }

    // ---- character plumbing -------------------------------------------------------------------------------

    /// The rest of the current physical line, verbatim — for COPY data blocks, which bypass the
    /// statement grammar. The queue holds the current line's unconsumed characters (refills are
    /// line-at-a-time), so it is drained first and nothing is lost.
    fn raw_line(&mut self) -> Result<Option<String>, String> {
        let mut s: String = self.queue.drain(..).collect();
        if self.eof {
            return Ok((!s.is_empty()).then_some(s));
        }
        let mut raw: Vec<u8> = Vec::new();
        let n = self.src.read_until(b'\n', &mut raw).map_err(|e| e.to_string())?;
        if n == 0 {
            self.eof = true;
            return Ok((!s.is_empty()).then_some(s));
        }
        self.line_no += 1;
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
        }
        match String::from_utf8_lossy(&raw) {
            std::borrow::Cow::Borrowed(t) => s.push_str(t),
            std::borrow::Cow::Owned(t) => {
                self.lossy += 1;
                s.push_str(&t);
            }
        }
        Ok(Some(s))
    }

    /// Consume to the end of the CURRENT physical line (a `--` comment's extent), including its
    /// terminator. Bumping through the virtual `'\n'` — not comparing line numbers — is what keeps
    /// this from eating the next line's first character: `bump` refills AND pops, so a
    /// line-number check around it consumed one character past the boundary.
    fn skip_rest_of_line(&mut self) -> Result<(), String> {
        loop {
            match self.bump()? {
                Some('\n') | None => return Ok(()),
                Some(_) => continue,
            }
        }
    }

    fn refill(&mut self) -> Result<bool, String> {
        let mut raw: Vec<u8> = Vec::new();
        let n = self.src.read_until(b'\n', &mut raw).map_err(|e| e.to_string())?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.line_no += 1;
        let mut ended = false;
        while matches!(raw.last(), Some(b'\n' | b'\r')) {
            raw.pop();
            ended = true;
        }
        if ended {
            self.pending_nl = true;
        }
        match String::from_utf8_lossy(&raw) {
            std::borrow::Cow::Borrowed(s) => self.queue.extend(s.chars()),
            std::borrow::Cow::Owned(s) => {
                self.lossy += 1;
                self.queue.extend(s.chars());
            }
        }
        Ok(true)
    }

    /// Characters available, counting the pending line terminator as one.
    fn avail(&self) -> usize {
        self.queue.len() + self.pending_nl as usize
    }

    /// Make at least `k` characters available; returns how many are available.
    fn ensure(&mut self, k: usize) -> Result<usize, String> {
        while self.avail() < k && !self.eof {
            self.refill()?;
        }
        Ok(self.avail())
    }

    fn peek(&mut self) -> Result<Option<char>, String> {
        if self.queue.is_empty() && !self.pending_nl {
            self.ensure(1)?;
        }
        if self.queue.is_empty() {
            return Ok(self.pending_nl.then_some('\n'));
        }
        Ok(Some(*self.queue.front().expect("non-empty")))
    }

    fn peek_at(&mut self, k: usize) -> Result<Option<char>, String> {
        if k < self.queue.len() {
            return Ok(Some(self.queue[k]));
        }
        if k == self.queue.len() && self.pending_nl {
            return Ok(Some('\n'));
        }
        if self.pending_nl {
            // Positions past the virtual newline shift by one: stream position k is queue[k-1].
            self.ensure(k + 1)?;
            return Ok(self.queue.get(k - 1).copied());
        }
        Ok((self.ensure(k + 1)? > k).then(|| self.queue[k]))
    }

    fn bump(&mut self) -> Result<Option<char>, String> {
        if self.queue.is_empty() && !self.pending_nl {
            self.ensure(1)?;
        }
        if self.queue.is_empty() {
            return Ok(if self.pending_nl {
                self.pending_nl = false;
                Some('\n')
            } else {
                None
            });
        }
        Ok(self.queue.pop_front())
    }

    /// The next SQL word, without consuming. Line-bounded, which is safe: no SQL keyword or
    /// identifier this reader must recognise spans a line.
    fn peek_word(&mut self) -> Result<Option<String>, String> {
        match self.peek()? {
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let mut w = String::new();
                for &c in self.queue.iter() {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                        w.push(c);
                    } else {
                        break;
                    }
                }
                Ok(Some(w))
            }
            _ => Ok(None),
        }
    }

    fn read_word(&mut self) -> Result<String, String> {
        let mut out = String::new();
        while let Some(c) = self.peek()? {
            if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                out.push(c);
                self.bump()?;
            } else {
                break;
            }
        }
        Ok(out)
    }

    /// Consume the next word if it is exactly `w` (case-insensitive); report whether it was.
    fn take_word(&mut self, w: &str) -> Result<bool, String> {
        match self.peek_word()? {
            Some(next) if next.eq_ignore_ascii_case(w) => {
                self.read_word()?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn consume_word(&mut self, w: &str) -> Result<(), String> {
        let got = self.read_word()?;
        if !got.eq_ignore_ascii_case(w) {
            return Err(format!("line {}: expected {w:?}, got {got:?}", self.line_no));
        }
        Ok(())
    }

    fn wanted(&self, full: &str, bare: &str) -> bool {
        self.tables.is_empty() || self.tables.iter().any(|t| t == full || t == bare)
    }
}

/// A reader is an iterator of records, so `for rec in reader` works and an `Err` item is the
/// loud stop the malformed-row policy calls for.
impl<R: BufRead> Iterator for SqlDumpReader<R> {
    type Item = Result<Record, String>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_row() {
            Ok(Some(r)) => Some(Ok(r)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// `(qualified, bare, from_stdin, columns)` — the head of a `COPY` statement.
#[allow(clippy::type_complexity)]
type CopyHead = (String, String, bool, Option<Vec<String>>);

/// One COPY data field. `\N` is NULL and arrives empty; the writer escapes every real backslash,
/// so a backslash here is always an escape.
fn unescape_copy_cell(cell: &str) -> String {
    if !cell.contains('\\') {
        return cell.to_string();
    }
    let mut out = String::with_capacity(cell.len());
    let mut it = cell.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            None => out.push('\\'),
            Some('N') => {}
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('v') => out.push('\u{b}'),
            Some(other) => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(sql: &str) -> Vec<Record> {
        let r = SqlDumpReader::new(std::io::Cursor::new(sql.to_string()), &[]);
        let mut out = Vec::new();
        for rec in r {
            out.push(rec.expect("a valid row"));
        }
        out
    }

    fn rows_of(sql: &str, tables: &[&str]) -> Vec<Record> {
        let t: Vec<String> = tables.iter().map(|s| s.to_string()).collect();
        let r = SqlDumpReader::new(std::io::Cursor::new(sql.to_string()), &t);
        let mut out = Vec::new();
        for rec in r {
            out.push(rec.expect("a valid row"));
        }
        out
    }

    #[test]
    fn a_plain_multi_row_insert_with_a_column_list() {
        let rs = rows(
            "INSERT INTO public.vendor (id, name, category) VALUES\n  \
             (1, 'NorthGate Hardware', 'services'),\n  \
             (2, 'Juan''s Pool Works', 'maintenance');",
        );
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0]["name"], "NorthGate Hardware");
        assert_eq!(rs[1]["name"], "Juan's Pool Works", "'' doubles to '");
        assert_eq!(rs[1]["category"], "maintenance");
    }

    #[test]
    fn quoted_identifiers_and_schema_qualified_tables() {
        let rs = rows(
            "INSERT INTO \"public\".\"profile\" (\"full_name\", \"email\") VALUES \
             ('Maria Santos', 'maria@example.com');",
        );
        assert_eq!(rs[0]["full_name"], "Maria Santos");
        assert_eq!(rs[0]["email"], "maria@example.com");
    }

    #[test]
    fn overriding_system_value_and_on_conflict_tails() {
        let rs = rows(
            "INSERT INTO public.membership (membership_id, role) OVERRIDING SYSTEM VALUE VALUES \
             (1, 'admin') ON CONFLICT DO NOTHING;",
        );
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0]["role"], "admin");
    }

    #[test]
    fn comments_and_other_statements_are_skipped() {
        let rs = rows(
            "-- a header comment\n\
             SET search_path = public;\n\
             /* block, with a ; inside */\n\
             CREATE TABLE IF NOT EXISTS t (id int); -- trailing\n\
             INSERT INTO t (id, name) VALUES (1, 'one');\n\
             BEGIN;\nCOMMIT;",
        );
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0]["name"], "one");
    }

    #[test]
    fn a_semicolon_inside_a_skipped_statement_does_not_split_it() {
        let rs = rows(
            "INSERT INTO keep (id) VALUES (1);\n\
             CREATE FUNCTION f() RETURNS void AS $f$ BEGIN RAISE NOTICE 'a;b'; END $f$ LANGUAGE plpgsql;\n\
             INSERT INTO keep (id) VALUES (2);",
        );
        assert_eq!(rs.len(), 2, "the dollar-quoted body survives the skip");
    }

    #[test]
    fn null_arrives_empty_and_booleans_fold_lowercase() {
        let rs = rows("INSERT INTO t (a, b, c) VALUES (NULL, TRUE, FALSE);");
        assert_eq!(rs[0].len(), 2, "NULL is filtered like an empty CSV cell");
        assert_eq!(rs[0]["b"], "true");
        assert_eq!(rs[0]["c"], "false");
    }

    #[test]
    fn e_strings_dollar_quotes_and_casts() {
        let rs = rows(
            "INSERT INTO t (a, b, c, d) VALUES \
             (E'line\nbreak\ttab', $tag$raw ; text$tag$, '2025-01-01'::timestamp with time zone, \
             'x'::text);",
        );
        assert_eq!(rs[0]["a"], "line\nbreak\ttab");
        assert_eq!(rs[0]["b"], "raw ; text");
        assert_eq!(rs[0]["c"], "2025-01-01");
        assert_eq!(rs[0]["d"], "x");
    }

    #[test]
    fn calls_numbers_and_multi_row_values_spanning_lines() {
        let rs = rows(
            "INSERT INTO t (id, at, score)\nVALUES\n  (1, now(), -1.5e-3),\n  (2, 'x', +42);",
        );
        assert_eq!(rs[0]["at"], "now()");
        assert_eq!(rs[0]["score"], "-1.5e-3");
        assert_eq!(rs[1]["score"], "+42");
        assert_eq!(rs[1]["at"], "x");
    }

    #[test]
    fn no_column_list_names_columns_positionally() {
        let rs = rows("INSERT INTO t VALUES (1, 'two');");
        assert_eq!(rs[0]["c0"], "1");
        assert_eq!(rs[0]["c1"], "two");
    }

    #[test]
    fn a_ragged_values_row_is_an_error_not_a_guess() {
        let mut r = SqlDumpReader::new(
            std::io::Cursor::new("INSERT INTO t (a, b, c) VALUES (1, 2);".to_string()),
            &[],
        );
        assert!(matches!(r.next(), Some(Err(_))), "3 named columns, 2 values");
    }

    #[test]
    fn the_table_filter_matches_bare_and_qualified_names() {
        let sql = "INSERT INTO public.a (x) VALUES ('a');\n\
                   INSERT INTO b (x) VALUES ('b');\n\
                   INSERT INTO \"public\".\"c\" (x) VALUES ('c');";
        assert_eq!(rows_of(sql, &["a"]).len(), 1);
        assert_eq!(rows_of(sql, &["public.a", "c"]).len(), 2);
        assert_eq!(rows_of(sql, &[]).len(), 3);
        let mut r = SqlDumpReader::new(
            std::io::Cursor::new("INSERT INTO a (x) VALUES ('a');".to_string()),
            &["z".to_string()],
        );
        assert!(r.next().is_none());
        assert_eq!(r.filtered(), 1, "excluded rows are counted, not silent");
    }

    #[test]
    fn insert_select_is_skipped_not_misparsed() {
        let rs = rows("INSERT INTO a (x) SELECT y FROM b;\nINSERT INTO a (x) VALUES ('v');");
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0]["x"], "v");
    }

    #[test]
    fn a_pg_dump_copy_block_reads_verbatim_until_backslash_dot() {
        let rs = rows(
            "SET search_path = public;\n\
             COPY public.product (sku, name, price) FROM stdin;\n\
             1\t7-Eleven\t99.50\n\
             \\N\tA\tB\n\
             2\tTab\\there\tC\n\
             \\.\n\
             INSERT INTO product (sku, name) VALUES (3, 'after');",
        );
        assert_eq!(rs.len(), 4);
        assert_eq!(rs[0]["name"], "7-Eleven");
        assert_eq!(rs[0]["price"], "99.50");
        assert_eq!(rs[1].len(), 2, r"\N is NULL, filtered like an empty cell");
        assert_eq!(rs[1]["name"], "A");
        assert_eq!(rs[2]["sku"], "2");
        assert_eq!(rs[2]["name"], "Tab\there", r"\t in COPY data decodes");
        assert_eq!(rs[2]["price"], "C");
        assert_eq!(rs[3]["sku"], "3", "statements after the block resume normally");
    }

    #[test]
    fn mysql_backticks_open_and_a_sqlite_dump() {
        let rs = rows(
            "/*!40000 SET NAMES utf8mb4 */;\n\
             INSERT INTO `t` (`a`, `b`) VALUES ('one', 'two');\n\
             INSERT INTO \"u\" VALUES (5);",
        );
        assert_eq!(rs[0]["a"], "one");
        assert_eq!(rs[1]["c0"], "5");
    }

    #[test]
    fn quoted_identifiers_keep_their_case_and_embedded_quotes() {
        let rs = rows("INSERT INTO t (\"We\"\"ird\", b) VALUES (1, 2);");
        assert_eq!(rs[0]["We\"ird"], "1");
    }

    #[test]
    fn truncated_dollar_quote_and_string_are_errors() {
        for bad in [
            "INSERT INTO t (a) VALUES ($tag$ never closed;",
            "INSERT INTO t (a) VALUES ('never closed",
        ] {
            let mut r = SqlDumpReader::new(std::io::Cursor::new(bad.to_string()), &[]);
            assert!(matches!(r.next(), Some(Err(_))), "{bad}");
        }
    }
}
