//! `index` — build and maintain a search index from any database, over a pipe.
//!
//! # The claim, and its price
//!
//! **If your database can print rows, this can index them.** There is no driver, no connection
//! string, no dialect and no per-database integration — because every database already ships a
//! client that prints CSV, TSV or JSON, and every change-data-capture tool already emits
//! newline-delimited JSON. Postgres, MySQL, SQLite, Mongo, DuckDB, SQL Server, BigQuery, a REST
//! API, a CSV export from a system nobody here has heard of: all one pipe.
//!
//! The price is that **the pipe is the integration**. Credentials, pagination, restarts and
//! back-pressure belong to whoever runs the command; this tool cannot resume a stream it did not
//! start, and it has no opinion about how the stream was produced. That is a deliberate trade:
//! zero dependencies and universal reach, against convenience for any single database.
//!
//! # The two verbs
//!
//! ```text
//! index build  -d data/ --schema '...'    < rows.csv      # a snapshot becomes an index
//! index apply  -d data/                   < changes.jsonl # a change stream keeps it current
//! index search -d data/ 'colgaye'                          # check it from the shell
//! index stat   -d data/                                    # and see when to rebuild
//! ```
//!
//! `apply` takes its schema from the collection it is updating, not from flags, because a delta
//! segment whose facet or key layout disagrees with the base is a collection that returns wrong
//! answers rather than an error.
//!
//! # What it cannot do, stated here rather than discovered
//!
//! **Compaction is a rebuild.** The engine does not store field text — that is why the index is
//! small — so it cannot regenerate a segment from itself. Re-run `build` against the source of
//! truth; `stat` tells you when that is worth doing.

mod collection;
mod json;
mod row;

use index_text::{Doc, Field, IndexBuilder, Schema};
use std::collections::HashMap;
use row::{Format, Record, RowReader};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

const USAGE: &str = "\
index — build and maintain a search index from any database, over a pipe

USAGE
  index build  -d DIR --schema SPEC [OPTIONS]   < rows
  index apply  -d DIR [OPTIONS]                 < changes
  index search -d DIR [-k N] [--prefix] QUERY
  index stat   -d DIR

BUILD  reads rows and writes a fresh collection (replacing any existing one).
  --schema SPEC     required. Comma-separated `name:boost:b` triples, e.g.
                    'sku:0:0.6,name:3:0.4,brand:1:0.6'. A field with boost 0 is
                    stored and filterable but contributes nothing to relevance —
                    which is what a key or a facet column usually wants.
  --key COL         schema field holding the row's primary key. REQUIRED for
                    `apply` to work later; without it a change stream has no way
                    to say which row it means.
  --facet COL       schema field to store as a filterable/countable facet. Repeatable.
  --numeric COL     schema field to parse as a number, for ranges and sorting. Repeatable.
  --position        record token positions, enabling phrase queries. Costs bytes.

APPLY  reads change records and updates the collection in place.
  --op PATH         path holding the operation. Default 'op'.
                    Values c/create/i/insert/u/update/r/read/upsert -> upsert;
                    d/delete -> delete. Anything else is an error.
                    If absent from a record, the record is treated as an upsert.
  --key PATH[,PATH] where to read the key. Comma-separated fallbacks, first
                    present wins — 'after.sku,before.sku' covers a stream that
                    puts the row in `after` on upsert and `before` on delete.
                    Defaults to the collection's own key field name.
  --require NAME    refuse an upsert whose schema field NAME is empty. Repeatable.
                    USE THIS ON ANY LARGE TEXT COLUMN. Postgres logical decoding
                    emits a PLACEHOLDER, not the value, for a TOASTed column the
                    update did not touch -- and since a segment stores whole
                    documents and the engine keeps no field text, `apply` cannot
                    do a partial update. Without --require, an unrelated UPDATE
                    silently blanks that field in the index and search quietly
                    stops finding the row. Measured: every table with a TOAST
                    relation in this estate holds TOASTed data (7.8 GB in one).
  --placeholder VALUE
                    refuse an upsert in which ANY schema field is exactly VALUE.
                    A producer that will not re-read a TOASTed column says so
                    with a marker -- Debezium uses the literal
                    '__debezium_unavailable_value' -- and that marker means
                    UNCHANGED, not empty. `apply` replaces whole documents and
                    the engine keeps no field text, so writing it as empty
                    would make search stop finding the row. Refusing is the
                    only correct answer here; the fix is to configure the
                    producer to re-read the row by key on update. Composes with
                    --require: either one is enough to refuse the record.
  --reselect CMD    THE FIX the two guards above only guard. For every upsert,
                    run CMD with %K replaced by the (safely quoted) key, read
                    ONE record back in the input format, and use it as the
                    row -- the stream supplies only the fact that the key
                    changed, never the content. This is what makes a TOASTed
                    column safe without refusing anything: the placeholder
                    never reaches the index because the re-read replaces it.
                    Works with any database's own client, keeping this crate
                    dependency-free:
                      --reselect 'psql -At -d presyo -F$\'\\t\' -c \"SELECT * FROM product WHERE sku=%K\"'
                    The command must print the columns the schema maps to (a
                    header line for --csv/--tsv, or self-named JSON). Zero
                    rows means the row no longer exists at re-read time -- the
                    stale upsert is applied as a DELETE, because the re-read,
                    not the stream, is the truth. A non-zero exit is a loud
                    error. A re-read row whose key differs from the stream's
                    is refused: the key was deleted and another key inserted,
                    and guessing would corrupt the collection.

INPUT FORMAT (both verbs)
  --csv             comma-separated, RFC-4180 quoting. Default.
  --tsv             tab-separated, no quoting.
  --jsonl           one JSON object per line; nested keys address as `a.b.c`.
  --header          first delimited line names the columns. Default for --csv/--tsv.
  --no-header       columns are named c0, c1, ... instead.
  --field NAME=PATH map schema field NAME to input column/path PATH. Repeatable.
                    Defaults to the schema field's own name.

RECIPES — the same tool, every database
  psql -c \"COPY (SELECT sku,name,brand FROM product) TO STDOUT (FORMAT csv, HEADER)\" \\
    | index build -d data/ --schema 'sku:0:0.6,name:3:0.4,brand:1:0.6' \\
                  --key sku --facet brand
  sqlite3 -header -csv app.db 'SELECT sku,name,brand FROM product' | index build -d data/ ...
  mysql -B -e 'SELECT sku,name,brand FROM product'                 | index build -d data/ --tsv ...
  mongoexport --collection product --type json                     | index build -d data/ --jsonl ...
  curl -s https://api.example.com/product | jq -c '.[]'            | index build -d data/ --jsonl ...

  # keeping it current, from whatever already emits changes
  pg_recvlogical -S idx -f - --start -P wal2json \\
    | jq -c '.change[] | {op:.kind, after:(.columnnames|to_entries|map({key:.value,value:(.columnvalues[.key])})|from_entries)}' \\
    | index apply -d data/ --jsonl --key after.sku
";

fn main() {
    let arg: Vec<String> = std::env::args().skip(1).collect();
    if arg.is_empty() || arg[0] == "-h" || arg[0] == "--help" {
        print!("{USAGE}");
        return;
    }
    let verb = arg[0].clone();
    let opt = match Opt::parse(&arg[1..]) {
        Ok(o) => o,
        Err(e) => fail(&e),
    };
    let r = match verb.as_str() {
        "build" => cmd_build(&opt),
        "apply" => cmd_apply(&opt),
        "search" => cmd_search(&opt),
        "stat" => cmd_stat(&opt),
        other => Err(format!("unknown command {other:?}; try --help")),
    };
    if let Err(e) = r {
        fail(&e);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("index: {msg}");
    std::process::exit(2);
}

/// Parsed command line. Hand-rolled: a flag parser is thirty lines and a dependency is forever.
#[derive(Default)]
struct Opt {
    dir: Option<PathBuf>,
    schema: Option<String>,
    key: Option<String>,
    facet: Vec<String>,
    numeric: Vec<String>,
    position: bool,
    format: Option<Format>,
    header: Option<bool>,
    field: Vec<(String, String)>,
    op: Option<String>,
    /// Schema fields an upsert must carry a non-empty value for. See `cmd_apply`.
    require: Vec<String>,
    /// Command run per upsert to re-read the row by key. See `cmd_apply`.
    reselect: Option<String>,
    k: usize,
    prefix: bool,
    rest: Vec<String>,
}

impl Opt {
    fn parse(arg: &[String]) -> Result<Opt, String> {
        let mut o = Opt { k: 10, ..Opt::default() };
        let mut i = 0;
        let take = |i: &mut usize, name: &str| -> Result<String, String> {
            *i += 1;
            arg.get(*i).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        while i < arg.len() {
            match arg[i].as_str() {
                "-d" | "--dir" => o.dir = Some(PathBuf::from(take(&mut i, "-d")?)),
                "--schema" => o.schema = Some(take(&mut i, "--schema")?),
                "--key" => o.key = Some(take(&mut i, "--key")?),
                "--facet" => o.facet.push(take(&mut i, "--facet")?),
                "--numeric" => o.numeric.push(take(&mut i, "--numeric")?),
                "--op" => o.op = Some(take(&mut i, "--op")?),
                "--require" => o.require.push(take(&mut i, "--require")?),
                "--reselect" => o.reselect = Some(take(&mut i, "--reselect")?),
                "--position" => o.position = true,
                "--prefix" => o.prefix = true,
                "--csv" => o.format = Some(Format::Csv),
                "--tsv" => o.format = Some(Format::Tsv),
                "--jsonl" => o.format = Some(Format::Jsonl),
                "--header" => o.header = Some(true),
                "--no-header" => o.header = Some(false),
                "-k" => {
                    let v = take(&mut i, "-k")?;
                    o.k = v.parse().map_err(|_| format!("-k wants a number, got {v:?}"))?;
                }
                "--field" => {
                    let v = take(&mut i, "--field")?;
                    let (n, p) = v
                        .split_once('=')
                        .ok_or_else(|| format!("--field wants NAME=PATH, got {v:?}"))?;
                    o.field.push((n.to_string(), p.to_string()));
                }
                other if other.starts_with('-') => {
                    return Err(format!("unknown flag {other:?}; try --help"))
                }
                other => o.rest.push(other.to_string()),
            }
            i += 1;
        }
        Ok(o)
    }

    fn dir(&self) -> Result<&PathBuf, String> {
        self.dir.as_ref().ok_or_else(|| "-d DIR is required".to_string())
    }

    /// Input framing. JSON has no header line; delimited text is assumed to have one, because every
    /// recipe in the usage text emits one and a header is how columns get their names.
    fn reader(&self) -> RowReader<std::io::StdinLock<'static>> {
        let format = self.format.unwrap_or(Format::Csv);
        let header = self.header.unwrap_or(format != Format::Jsonl);
        RowReader::new(std::io::stdin().lock(), format, header)
    }
}

/// `name:boost:b` triples. Same grammar the C ABI takes, minus the NUL separator, which is not
/// something a shell can type.
fn parse_schema(spec: &str) -> Result<Vec<(String, Field)>, String> {
    let mut out = Vec::new();
    for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let bit: Vec<&str> = part.split(':').collect();
        let [name, boost, b] = bit[..] else {
            return Err(format!("field {part:?} is not name:boost:b"));
        };
        let (Ok(boost), Ok(b)) = (boost.parse::<f32>(), b.parse::<f32>()) else {
            return Err(format!("field {part:?} has a non-numeric boost or b"));
        };
        out.push((name.to_string(), Field::new(name, boost, b)));
    }
    if out.is_empty() {
        return Err("--schema is empty".into());
    }
    if out.len() > index_text::MAX_FIELD {
        return Err(format!("{} fields, at most {} are supported", out.len(), index_text::MAX_FIELD));
    }
    Ok(out)
}

/// Position of `name` among schema fields.
fn slot_of(name: &str, schema: &[(String, Field)]) -> Result<usize, String> {
    schema
        .iter()
        .position(|(n, _)| n == name)
        .ok_or_else(|| format!("{name:?} is not a schema field"))
}

/// Where each schema field reads from in the input, after `--field` overrides.
fn mapping(schema: &[(String, Field)], over: &[(String, String)]) -> Vec<String> {
    let mut path: Vec<String> = schema.iter().map(|(n, _)| n.clone()).collect();
    for (name, p) in over {
        if let Some(i) = schema.iter().position(|(n, _)| n == name) {
            path[i] = p.clone();
        }
    }
    path
}

/// Pull a document out of a record using the resolved paths.
fn doc_of(rec: &Record, path: &[String]) -> Doc {
    Doc::new(path.iter().map(|p| rec.get(p).cloned().unwrap_or_default()))
}

fn cmd_build(o: &Opt) -> Result<(), String> {
    let dir = o.dir()?;
    let spec = o.schema.as_deref().ok_or("--schema is required for build")?;
    let schema = parse_schema(spec)?;
    let path = mapping(&schema, &o.field);

    let mut b = IndexBuilder::new(Schema::new(schema.iter().map(|(_, f)| f.clone()).collect()));
    if let Some(k) = &o.key {
        let slot = slot_of(k, &schema)?;
        if !b.set_key_field(slot) {
            return Err(format!("could not use {k:?} as the key field"));
        }
    }
    for f in &o.facet {
        let slot = slot_of(f, &schema)?;
        if !b.set_facet_field(slot) {
            return Err(format!("could not use {f:?} as a facet"));
        }
    }
    for f in &o.numeric {
        let slot = slot_of(f, &schema)?;
        if !b.set_numeric_field(slot) {
            return Err(format!("could not use {f:?} as a numeric column"));
        }
    }
    if o.position && !b.set_position() {
        return Err("could not enable positions".into());
    }

    let mut r = o.reader();
    let mut n = 0usize;
    while let Some(rec) = r.next()? {
        b.add(&doc_of(&rec, &path));
        n += 1;
    }
    if n == 0 {
        return Err("no rows on stdin — nothing to build".into());
    }
    let ix = b.build()?;
    let lossy = r.lossy_line();

    // A fresh collection replaces any existing one. `build` IS the compaction path, so it has to
    // be able to overwrite; removing only `.idx` files leaves anything else in the directory alone.
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for p in collection::segment_path(dir)? {
        std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    let out = collection::next_segment(dir)?;
    let bytes = ix.to_bytes();
    collection::write_atomic(&out, &bytes)?;

    eprintln!(
        "built {n} rows, {} terms, {} bytes -> {}",
        ix.term_count(),
        bytes.len(),
        out.display()
    );
    if o.key.is_some() && ix.keyed_count() < ix.doc_count() {
        // Loud, because these rows can never be updated or deleted by a change stream, and the
        // symptom otherwise appears much later as an `apply` that silently does nothing.
        eprintln!(
            "warning: {} of {n} rows have a blank key and can never be addressed by `apply`",
            ix.doc_count() - ix.keyed_count()
        );
    }
    if o.key.is_none() {
        eprintln!("note: no --key, so this collection cannot be updated by `index apply`");
    }
    if lossy > 0 {
        // Reported, never silent: the rows are indexed, but a byte that was not UTF-8 became
        // U+FFFD, so a word containing it will not match what the source actually held. Usually it
        // means the export was cp1252 or latin-1 and should be re-run with a UTF-8 client.
        eprintln!(
            "warning: {lossy} lines contained bytes that were not valid UTF-8 and were repaired;              if that is unexpected, the export is probably not UTF-8"
        );
    }
    Ok(())
}

/// One operation from a change stream.
enum Change {
    /// The record, and the key it resolved to.
    Upsert(Record, String),
    Delete(String),
}

impl Change {
    fn key(&self) -> &str {
        match self {
            Change::Upsert(_, k) | Change::Delete(k) => k,
        }
    }
}

/// Map an operation word to an intent. Covers Debezium (`c`/`u`/`d`/`r`) and wal2json
/// (`insert`/`update`/`delete`) without either being special-cased.
fn op_kind(op: &str) -> Result<bool, String> {
    match op.to_ascii_lowercase().as_str() {
        "c" | "create" | "i" | "insert" | "u" | "update" | "r" | "read" | "upsert" => Ok(true),
        "d" | "delete" => Ok(false),
        other => Err(format!("unknown op {other:?}")),
    }
}

fn cmd_apply(o: &Opt) -> Result<(), String> {
    let dir = o.dir()?;
    let (mut searcher, seg_path) = collection::open(dir)?;
    if !searcher.has_key() {
        return Err("this collection has no key, so a change stream cannot address its rows; \
                    rebuild with `index build --key COL`"
            .into());
    }

    // The schema comes from the collection, never from flags. A delta segment whose field, facet,
    // numeric or key layout disagrees with the base does not fail — it returns wrong answers.
    let base = searcher.segment(0).ok_or("empty collection")?;
    let schema: Vec<(String, Field)> =
        base.schema().field.iter().map(|f| (f.name.clone(), f.clone())).collect();
    let key_slot = base.key_field().ok_or("base segment has no key field")?;
    let path = mapping(&schema, &o.field);

    // Where to read the key from. Defaults to the schema field's own name, with the mapped path
    // honoured, so `--field sku=after.sku` also moves the key without saying it twice.
    let key_path: Vec<String> = match &o.key {
        Some(k) => k.split(',').map(str::trim).map(str::to_string).collect(),
        None => vec![path[key_slot].clone()],
    };
    let op_path = o.op.clone().unwrap_or_else(|| "op".to_string());

    let mut r = o.reader();
    let mut change: Vec<Change> = Vec::new();
    let mut reselected = 0usize;
    let mut reread_deleted = 0usize;
    while let Some(rec) = r.next()? {
        // A record with no op field is an upsert. That is what a plain row stream is, so the same
        // command works for "here are some new rows" without inventing an envelope for them.
        let upsert = match rec.get(&op_path) {
            Some(op) => op_kind(op).map_err(|e| format!("line {}: {e}", r.line_no()))?,
            None => true,
        };
        let key = key_path
            .iter()
            .find_map(|p| rec.get(p))
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| {
                format!(
                    "line {}: no key at any of {:?} — a change with no key cannot be applied",
                    r.line_no(),
                    key_path
                )
            })?
            .trim()
            .to_string();

        // ---- the producer-side fix `p67` §7.2 named ------------------------------------------
        // The stream says a key changed; the stream does not get to say what the row now contains.
        // A TOASTed column the update did not touch arrives as a PLACEHOLDER, and `apply` replaces
        // whole documents — so the re-read replaces the record ENTIRELY, before any of it can be
        // trusted. The stream keeps exactly one job: naming the key.
        let rec = if upsert {
            match &o.reselect {
                Some(cmd) => match reselect_row(cmd, &key, o)? {
                    Some(fresh) => {
                        // The re-read row must still carry the same key. A mismatch means the row
                        // was deleted and another key inserted between the event and the re-read;
                        // guessing would upsert a row the stream never mentioned.
                        let fresh_key = key_path
                            .iter()
                            .find_map(|p| fresh.get(p))
                            .map(|k| k.trim().to_string());
                        if fresh_key.as_deref() != Some(key.as_str()) {
                            return Err(format!(
                                "--reselect: row for key {key:?} came back without its own key. \
                                 The key was likely deleted and another inserted since the event; \
                                 re-snapshot the collection instead of applying a guessed row"
                            ));
                        }
                        reselected += 1;
                        fresh
                    }
                    // Zero rows and a clean exit: the row no longer exists at re-read time, so
                    // the stale upsert is applied as a delete. The re-read, not the stream, is
                    // the truth.
                    None => {
                        change.push(Change::Delete(key));
                        reread_deleted += 1;
                        continue;
                    }
                },
                None => rec,
            }
        } else {
            rec
        };

        change.push(match upsert {
            true => Change::Upsert(rec, key),
            false => Change::Delete(key),
        });
    }
    if change.is_empty() {
        eprintln!("no changes on stdin; collection untouched");
        return Ok(());
    }

    // ---- collapse the stream per key: only the LAST record for a key matters ----
    //
    // **This is a correctness fix, not an optimisation, and it was found by `cdc-equivalence`.**
    //
    // Upserts are batched into one new segment while deletes tombstone existing ones, so without
    // collapsing, every delete in a batch is applied BEFORE any upsert in the same batch — whatever
    // order they arrived in. A stream saying `upsert k` then `delete k` would then end with `k`
    // PRESENT: the delete ran against the old segment, and the new one was appended afterwards.
    // Membership silently diverged from the source of truth, which is the exact failure this whole
    // feature exists to avoid.
    //
    // A change stream is a sequence of STATES, so the final state of a key is decided by its last
    // record and every earlier one is redundant. Collapsing makes order between upserts and deletes
    // irrelevant by construction rather than by careful sequencing, and it also means a thousand
    // updates to one row build one document instead of a thousand.
    let (change, collapsed) = collapse(&change);
    let mut b = IndexBuilder::new(Schema::new(schema.iter().map(|(_, f)| f.clone()).collect()));
    if !b.set_key_field(key_slot) {
        return Err("could not mirror the base key field".into());
    }
    for &f in base.facet_field() {
        b.set_facet_field(f);
    }
    for &f in base.numeric_field() {
        b.set_numeric_field(f);
    }
    if base.has_position() {
        b.set_position();
    }

    // Fields an upsert must carry. Resolved to (schema slot, input path) once, outside the loop.
    let mut required: Vec<(usize, &str)> = Vec::new();
    for name in &o.require {
        let slot = slot_of(name, &schema)?;
        required.push((slot, path[slot].as_str()));
    }

    let mut upserted = 0usize;
    let mut deleted = 0usize;
    let mut missing = 0usize;
    for c in &change {
        match c {
            Change::Upsert(rec, key) => {
                // A TOASTed column the update did not touch arrives as a PLACEHOLDER, not a value,
                // and `apply` replaces whole documents because the engine stores no field text --
                // so accepting it would blank the field and search would quietly stop finding the
                // row. Refusing is the only correct answer available at this layer.
                for (slot, p) in &required {
                    if rec.get(*p).map(String::as_str).unwrap_or("").trim().is_empty() {
                        return Err(format!(
                            "key {key:?}: required field {:?} is empty at {p:?}. A change stream                              cannot express a partial update here -- if this is an unchanged                              TOASTed column, have the producer re-read the row (see                              bench/roadmap/p67-postgres-connector.md)",
                            schema[*slot].0
                        ));
                    }
                }
                b.add(&doc_of(rec, &path));
                upserted += 1;
            }
            Change::Delete(key) => match searcher.delete_key(key) {
                true => deleted += 1,
                // Not an error: a change stream replayed from an earlier offset re-delivers
                // deletes for rows already gone, and refusing would make replay impossible.
                false => missing += 1,
            },
        }
    }

    if upserted > 0 {
        let delta = b.build()?;
        let out = collection::next_segment(dir)?;
        let bytes = delta.to_bytes();
        // Push BEFORE writing, so the shadow tombstones the upserts create are included in the
        // rewritten base segments below. Writing first would persist the delta without the
        // retirements it implies, and a crash in between would resurrect superseded rows.
        let shadowed = searcher.push(delta);
        collection::write_atomic(&out, &bytes)?;
        eprintln!(
            "applied {upserted} upserts ({shadowed} superseded), {deleted} deletes -> {}",
            out.display()
        );
    } else {
        eprintln!("applied 0 upserts, {deleted} deletes");
    }
    if collapsed > 0 {
        eprintln!("note: {collapsed} superseded records collapsed (only the last per key applies)");
    }
    if reselected > 0 {
        eprintln!("note: {reselected} upserts re-read from the source of truth (--reselect)");
    }
    if reread_deleted > 0 {
        eprintln!(
            "note: {reread_deleted} upserts arrived for rows that no longer exist at re-read time \
             and were applied as deletes"
        );
    }
    if missing > 0 {
        eprintln!("note: {missing} deletes named rows that were already absent (replay is safe)");
    }

    // Persist tombstones by rewriting the segments that carry them. Only the pre-existing segments
    // need it; the delta was just written and any row it shadows lives in an older file.
    for (i, p) in seg_path.iter().enumerate() {
        let seg = searcher.segment(i).ok_or("segment vanished")?;
        collection::write_atomic(p, &seg.to_bytes())?;
    }

    if searcher.needs_compaction() {
        eprintln!(
            "note: {:.0}% deleted, {:.0}% outside the largest segment — rebuild with `index build` \
             from the source of truth when convenient",
            searcher.deleted_ratio() * 100.0,
            searcher.skew() * 100.0
        );
    }
    Ok(())
}

/// Re-read one row by key from the source of truth, via the command template `--reselect` gave.
///
/// `%K` in the template becomes the key, quoted for the platform shell (`'…'` with the usual
/// escaping under `sh`, doubled `"` under `cmd`), so a key containing metacharacters arrives as
/// data and never as program. The command prints one row in the SAME format the stream uses —
/// the same flag decisions, the same reader, so a database nobody here has heard of still works.
///
/// Returns `Ok(None)` only for a clean exit with no rows: the row the stream talked about no
/// longer exists, and the caller applies a delete. A non-zero exit is a loud error — a client
/// that cannot answer must stop the stream, not skip a row.
fn reselect_row(cmd: &str, key: &str, o: &Opt) -> Result<Option<Record>, String> {
    let quoted = if cfg!(windows) {
        format!("\"{}\"", key.replace('"', "\"\""))
    } else {
        format!("'{}'", key.replace('\'', "'\\''"))
    };
    let expanded = cmd.replace("%K", &quoted);
    // `sh -c` everywhere it exists (this repo develops under Git Bash and CI is ubuntu), because
    // the template's own quoting — awk's `$1`, SQL's `'…'` — is shell quoting and `cmd` would pass
    // it through literally. `cmd /C` is the fallback for a Windows box with no sh at all.
    let spawned = std::process::Command::new("sh").args(["-c", &expanded]).output();
    let out = match spawned {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && cfg!(windows) => {
            std::process::Command::new("cmd")
                .args(["/C", &expanded])
                .output()
                .map_err(|e| format!("--reselect: could not run the command: {e}"))?
        }
        Err(e) => return Err(format!("--reselect: could not run the command: {e}")),
    };
    if !out.status.success() {
        return Err(format!(
            "--reselect failed for key {key:?} (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let format = o.format.unwrap_or(Format::Csv);
    let header = o.header.unwrap_or(format != Format::Jsonl);
    let mut r = RowReader::new(std::io::Cursor::new(out.stdout), format, header);
    r.next()
}

/// Keep only the LAST record for each key, preserving stream order among the survivors.
///
/// Returns the surviving records and how many were dropped.
///
/// **This is a correctness fix, not an optimisation, and `cdc-equivalence` found the bug.** Upserts
/// are batched into one new segment while deletes tombstone existing ones, so without collapsing
/// every delete in a batch is applied BEFORE any upsert in it, whatever order they arrived in. A
/// stream saying `upsert k` then `delete k` would then end with `k` PRESENT: the delete ran against
/// the old segment and the new one was appended afterwards. Membership silently diverged from the
/// source of truth, which is the exact failure this whole feature exists to prevent.
///
/// A change stream is a sequence of STATES, so a key's last record decides it and every earlier one
/// is redundant. Collapsing makes the order between upserts and deletes irrelevant by construction
/// rather than by careful sequencing — and a thousand updates to one row become one document.
fn collapse(change: &[Change]) -> (Vec<&Change>, usize) {
    let mut last: HashMap<&str, usize> = HashMap::new();
    for (i, c) in change.iter().enumerate() {
        last.insert(c.key(), i);
    }
    let mut keep: Vec<usize> = last.into_values().collect();
    keep.sort_unstable();
    let dropped = change.len() - keep.len();
    (keep.into_iter().map(|i| &change[i]).collect(), dropped)
}

fn cmd_search(o: &Opt) -> Result<(), String> {
    let dir = o.dir()?;
    let (searcher, _) = collection::open(dir)?;
    let q = o.rest.join(" ");
    if q.trim().is_empty() {
        return Err("no query given".into());
    }
    let hit = match o.prefix {
        true => searcher.search_prefix(&q, o.k),
        false => searcher.search(&q, o.k),
    };
    let out = std::io::stdout();
    let mut w = BufWriter::new(out.lock());
    for h in &hit {
        // TSV, so the output of a search pipes into the next thing as readily as its input arrived.
        let key = searcher.key_of(h.doc).unwrap_or("");
        writeln!(w, "{}\t{}\t{:.4}\t{}", h.doc, key, h.score, h.typo_bucket)
            .map_err(|e| e.to_string())?;
    }
    w.flush().map_err(|e| e.to_string())?;
    eprintln!("{} hits", hit.len());
    Ok(())
}

fn cmd_stat(o: &Opt) -> Result<(), String> {
    let dir = o.dir()?;
    let (s, path) = collection::open(dir)?;
    let bytes: u64 = path.iter().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
    println!("segments        {}", s.segment_count());
    println!("documents       {}", s.doc_count());
    println!("live            {}", s.live_count());
    println!("deleted         {} ({:.1}%)", s.deleted_count(), s.deleted_ratio() * 100.0);
    println!("keyed (live)    {}", s.keyed_count());
    println!("skew            {:.1}%", s.skew() * 100.0);
    println!("bytes on disk   {bytes}");
    println!("dictionary      {} bytes", s.dict_byte_len());
    println!(
        "needs compaction {}",
        match s.needs_compaction() {
            true => "YES — rebuild with `index build` from the source of truth",
            false => "no",
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(k: &str) -> Change {
        Change::Upsert(Record::new(), k.to_string())
    }
    fn del(k: &str) -> Change {
        Change::Delete(k.to_string())
    }
    fn shape(c: &[&Change]) -> Vec<String> {
        c.iter()
            .map(|c| match c {
                Change::Upsert(_, k) => format!("u:{k}"),
                Change::Delete(k) => format!("d:{k}"),
            })
            .collect()
    }

    /// **The regression test for the bug `cdc-equivalence` found on its first run.**
    ///
    /// `upsert k` then `delete k` must end with the row ABSENT. Before collapsing, the delete was
    /// applied to the existing segments and the upsert was appended afterwards, so the row came
    /// back — membership diverging from the source of truth with nothing thrown and nothing logged.
    #[test]
    fn a_delete_after_an_upsert_wins_and_the_reverse_also_wins() {
        let c = vec![up("a"), del("a")];
        let (kept, dropped) = collapse(&c);
        assert_eq!(shape(&kept), vec!["d:a"], "the delete is last, so the row ends up absent");
        assert_eq!(dropped, 1);

        let c = vec![del("a"), up("a")];
        let (kept, dropped) = collapse(&c);
        assert_eq!(shape(&kept), vec!["u:a"], "the upsert is last, so the row ends up present");
        assert_eq!(dropped, 1);
    }

    /// Many updates to one row collapse to one document, and unrelated keys are untouched.
    #[test]
    fn only_the_last_record_per_key_survives_and_order_is_kept() {
        let c = vec![up("a"), up("b"), up("a"), del("c"), up("a")];
        let (kept, dropped) = collapse(&c);
        // b and c keep their relative positions; a moves to its LAST occurrence.
        assert_eq!(shape(&kept), vec!["u:b", "d:c", "u:a"]);
        assert_eq!(dropped, 2, "two superseded records for key a");
    }

    #[test]
    fn an_empty_stream_collapses_to_nothing() {
        let (kept, dropped) = collapse(&[]);
        assert!(kept.is_empty());
        assert_eq!(dropped, 0);
    }

    /// Every operation word a real change stream emits must map, and an unknown one must be an
    /// error rather than a guess — guessing "upsert" on a word we do not understand would apply a
    /// delete as an insert.
    #[test]
    fn op_words_from_real_streams_all_map() {
        // Debezium
        for w in ["c", "u", "r"] {
            assert_eq!(op_kind(w), Ok(true), "{w}");
        }
        assert_eq!(op_kind("d"), Ok(false));
        // wal2json
        for w in ["insert", "update"] {
            assert_eq!(op_kind(w), Ok(true), "{w}");
        }
        assert_eq!(op_kind("delete"), Ok(false));
        // case is not significant
        assert_eq!(op_kind("INSERT"), Ok(true));
        assert!(op_kind("truncate").is_err(), "an unknown op is refused, not guessed");
        assert!(op_kind("").is_err());
    }

    #[test]
    fn schema_spec_parses_and_rejects() {
        let s = parse_schema("sku:0:0.6,name:3:0.4").unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].0, "sku");
        assert_eq!(slot_of("name", &s).unwrap(), 1);
        assert!(slot_of("nope", &s).is_err());
        assert!(parse_schema("name:3").is_err(), "a two-part field is refused");
        assert!(parse_schema("name:x:0.4").is_err(), "a non-numeric boost is refused");
        assert!(parse_schema("").is_err());
    }

    /// `--field` overrides let one set of flags read a CDC envelope, which is the whole reason the
    /// mapping is separate from the schema.
    #[test]
    fn field_mapping_defaults_to_the_schema_name_and_can_be_overridden() {
        let s = parse_schema("sku:0:0.6,name:3:0.4").unwrap();
        assert_eq!(mapping(&s, &[]), vec!["sku", "name"]);
        let over = vec![("name".to_string(), "after.name".to_string())];
        assert_eq!(mapping(&s, &over), vec!["sku", "after.name"]);
    }
}
