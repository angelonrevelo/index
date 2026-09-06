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
