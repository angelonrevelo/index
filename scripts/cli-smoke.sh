#!/usr/bin/env bash
# CLI smoke: drive `index build`, `apply` and `search` over a pipe, exactly as the README does.
#
# This exists because the CLI is the only surface a database administrator ever touches, and "it
# compiles" says nothing about whether a pipe of rows becomes a searchable index. It uses no
# database: the point under test is the tool, and any database's client output reduces to the same
# CSV or JSONL by the time it reaches stdin.
#
# The assertion that matters is the last one: applying a change stream must converge on the same
# answers as rebuilding from the final state. `bench/roadmap/p50-change-stream.md` measures that as
# a property over 8,000 operations; this is the end-to-end version that runs in CI.

set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release -p index-cli
IDX="target/release/index"
[ -x "$IDX" ] || IDX="target/release/index.exe"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

SCHEMA='sku:0:0.6,name:3:0.4,brand:1:0.6,price:0:0.6'
fail=0
check() {
  if [ "$2" = "$3" ]; then
    echo "  PASS  $1"
  else
    echo "  FAIL  $1 (want $3, got $2)"
    fail=$((fail + 1))
  fi
}

# ---- build, from a pipe, with a header line -------------------------------------------------
cat > "$WORK/rows.csv" <<'CSV'
sku,name,brand,price
A-1,Colgate Total Toothpaste 150g,Colgate,119.5
A-2,Colgate Fresh Gel Toothpaste 100g,Colgate,89.0
A-3,Aquafresh Mini Toothpaste 50g,Aquafresh,45.0
A-4,Oral B Toothbrush Medium,Oral B,75.0
A-5,Bear Brand Powdered Milk 300g,Bear Brand,210.0
CSV
"$IDX" build -d "$WORK/data" --schema "$SCHEMA" --key sku --facet brand --numeric price \
  < "$WORK/rows.csv" 2>/dev/null

keys() { "$IDX" search -d "$1" -k 50 "$2" 2>/dev/null | cut -f2 | sort | tr '\n' ' '; }

check "an index is built from a pipe"          "$(keys "$WORK/data" toothpaste)"  "A-1 A-2 A-3 "
check "typos are corrected"                    "$(keys "$WORK/data" 'colgaye')"   "A-1 A-2 "

# ---- apply a change stream ---------------------------------------------------------------------
# Deliberately includes the ordering hazard: A-9 is inserted and deleted in the SAME stream and
# must end up absent. Before per-key collapsing this returned the row, because batched upserts were
# appended after the deletes had already run.
cat > "$WORK/changes.jsonl" <<'JSONL'
{"op":"u","after":{"sku":"A-1","name":"Colgate Total Charcoal Toothpaste 150g","brand":"Colgate","price":129.5}}
{"op":"c","after":{"sku":"A-9","name":"Temporary Ghost Toothpaste","brand":"Ghost","price":1.0}}
{"op":"d","before":{"sku":"A-9"}}
{"op":"d","before":{"sku":"A-3"}}
{"op":"c","after":{"sku":"A-6","name":"Sensodyne Repair Toothpaste 75g","brand":"Sensodyne","price":249.0}}
JSONL
"$IDX" apply -d "$WORK/data" --jsonl --key after.sku,before.sku \
  --field sku=after.sku --field name=after.name --field brand=after.brand --field price=after.price \
  < "$WORK/changes.jsonl" 2>/dev/null

check "an update is found by its new text"     "$(keys "$WORK/data" charcoal)"    "A-1 "
check "a deleted row is gone"                  "$(keys "$WORK/data" aquafresh)"   ""
check "an inserted row is present"             "$(keys "$WORK/data" sensodyne)"   "A-6 "
check "insert+delete in one stream ends absent" "$(keys "$WORK/data" ghost)"      ""

# ---- the claim that matters: apply == rebuild ---------------------------------------------------
cat > "$WORK/final.csv" <<'CSV'
sku,name,brand,price
A-1,Colgate Total Charcoal Toothpaste 150g,Colgate,129.5
A-2,Colgate Fresh Gel Toothpaste 100g,Colgate,89.0
A-4,Oral B Toothbrush Medium,Oral B,75.0
A-5,Bear Brand Powdered Milk 300g,Bear Brand,210.0
A-6,Sensodyne Repair Toothpaste 75g,Sensodyne,249.0
CSV
"$IDX" build -d "$WORK/rebuilt" --schema "$SCHEMA" --key sku --facet brand --numeric price \
  < "$WORK/final.csv" 2>/dev/null

for q in toothpaste colgate sensodyne milk brush charcoal 'colgaye tothpaste' 'bear brand' gel; do
  check "applied == rebuilt for '$q'" "$(keys "$WORK/data" "$q")" "$(keys "$WORK/rebuilt" "$q")"
done

# ---- the TOAST guard --------------------------------------------------------------------------
# Postgres logical decoding emits a PLACEHOLDER for a TOASTed column an update did not touch. Since
# a segment stores whole documents and the engine keeps no field text, `apply` cannot merge -- so an
# unrelated UPDATE would silently blank the column and search would stop finding the row. Refusing
# is the only correct answer at this layer.
toast_json='{"op":"u","after":{"sku":"A-1","name":"","brand":"Colgate"}}'
ok_json='{"op":"u","after":{"sku":"A-1","name":"Colgate Total Charcoal Toothpaste 150g","brand":"Colgate"}}'
apply_req() {
  echo "$1" | "$IDX" apply -d "$WORK/data" --jsonl --key after.sku --require name \
    --field sku=after.sku --field name=after.name --field brand=after.brand >/dev/null 2>&1
}
if apply_req "$toast_json"; then
  echo "  FAIL  an upsert blanking a required field must be refused"
  fail=$((fail + 1))
else
  echo "  PASS  an upsert blanking a required field is refused (TOAST placeholder guard)"
fi
# ...and the same record is accepted when the field is actually present.
if apply_req "$ok_json"; then
  echo "  PASS  ...and a complete row still applies"
else
  echo "  FAIL  a complete row must still apply under --require"
  fail=$((fail + 1))
fi

# ---- the producer-side fix: --reselect re-reads the row by key ---------------------------------
# The stream says a key changed; the stream does NOT get to say what the row now contains. The
# "database" here is a plain CSV file, and the reselect command is what psql would be: print the
# one row for the key. A TOAST placeholder in the stream never reaches the index, an upsert for a
# row deleted before the re-read becomes a delete, and a client that fails stops the stream.
cat > "$WORK/truth.csv" <<'CSV'
sku,name,brand,price
A-2,Colgate Fresh Gel Toothpaste 100g,Colgate,99.0
CSV
cat > "$WORK/reread.csv" <<'CSV'
op,sku,name,brand,price
u,A-2,__debezium_unavailable_value,Colgate,99.0
u,A-5,X-5,X,1.0
CSV
"$IDX" apply -d "$WORK/data" --csv --key sku --numeric price   --reselect "awk -F, -v k=%K 'NR==1 || \$1==k' \"$WORK/truth.csv\""   < "$WORK/reread.csv" 2>/dev/null
check "a placeholder is healed by the re-read"  "$(keys "$WORK/data" gel)"          "A-2 "
check "an upsert for a row gone at re-read is a delete" "$(keys "$WORK/data" bear)" ""
if echo '{"op":"u","after":{"sku":"A-1"}}' | "$IDX" apply -d "$WORK/data" --jsonl --key after.sku     --reselect "exit 3" >/dev/null 2>&1; then
  echo "  FAIL  a failing reselect must stop the stream"
  fail=$((fail + 1))
else
  echo "  PASS  a failing reselect is a loud stop rather than a skipped row"
fi

# ---- --sql: the dump IS the pipe --------------------------------------------------------------
# A dump a database already wrote — pg_dump --column-inserts, a Supabase seed.sql, sqlite3 .dump —
# is full of rows and needs no client to run. This exercises both shapes the reader takes:
# INSERT statements (with the decorations dumps actually carry: OVERRIDING, ON CONFLICT, comments,
# dollar quotes) and a plain pg_dump COPY ... FROM stdin block, plus the --table filter.
cat > "$WORK/dump.sql" <<'SQL'
-- pg_dump-style header noise
SET search_path = public;
CREATE TABLE product (sku text, name text, brand text, price numeric); -- skipped
INSERT INTO public.product (sku, name, brand, price) OVERRIDING SYSTEM VALUE VALUES
  ('S-1', 'Colgate Total Toothpaste 150g', 'Colgate', 119.5)
  ON CONFLICT (sku) DO UPDATE SET name = EXCLUDED.name;
INSERT INTO public.other (x) VALUES ('not this table');
COPY public.product (sku, name, brand, price) FROM stdin;
S-2	Colgate Fresh Gel Toothpaste 100g	Colgate	89.0
S-3	Aquafresh Mini Toothpaste 50g	Aquafresh	45.0
\.
SQL
"$IDX" build -d "$WORK/sqldata" --sql --table public.product \
  --schema "$SCHEMA" --key sku --facet brand --numeric price < "$WORK/dump.sql" 2>/dev/null

check "INSERT rows from a dump are indexed"    "$(keys "$WORK/sqldata" colgate)"   "S-1 S-2 "
check "a COPY block's rows are indexed"        "$(keys "$WORK/sqldata" aquafresh)" "S-3 "
check "dump typos are corrected"               "$(keys "$WORK/sqldata" 'colgaye')" "S-1 S-2 "

# ---- refusals: a tool that silently does the wrong thing is worse than one that stops ------------
if "$IDX" build -d "$WORK/nokey" --schema "$SCHEMA" < "$WORK/rows.csv" 2>/dev/null; then
  if "$IDX" apply -d "$WORK/nokey" --jsonl < "$WORK/changes.jsonl" 2>/dev/null; then
    echo "  FAIL  applying to a keyless collection must be refused"
    fail=$((fail + 1))
  else
    echo "  PASS  applying to a keyless collection is refused"
  fi
fi

if printf 'sku,name\nA-1\n' | "$IDX" build -d "$WORK/ragged" --schema "$SCHEMA" 2>/dev/null; then
  echo "  FAIL  a ragged row must be refused"
  fail=$((fail + 1))
else
  echo "  PASS  a ragged row is refused rather than shifted"
fi

echo
if [ "$fail" -eq 0 ]; then
  echo "OVERALL: PASS"
else
  echo "OVERALL: FAIL ($fail)"
  exit 1
fi
