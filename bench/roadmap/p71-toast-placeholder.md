# P71 — the TOAST placeholder, refused by name

**Tier:** T2 · **API:** `index apply --placeholder VALUE`, `index stat`
**Check:** `scripts/cli-smoke.sh` (in CI)
**Status: SHIPPED, 2026-09-06.**

`p67` measured the hazard rather than reasoning about it: **every table with a TOAST relation in the
estate holds TOASTed data** — 7.8 GB in presyo alone — and Postgres logical decoding emits a
**placeholder instead of the value** for a TOASTed column that an `UPDATE` did not touch.

`apply` replaces whole documents, and the engine deliberately stores no field text (that is why an
index is tens of bytes per document). So there is nothing to merge a partial update against:

> **A partial update is not expressible in this architecture.** The correct answer is to refuse the
> record, naming the field and the key — not to accept a blank.

`p67` shipped `--require NAME` for the empty case. This closes the other one: the placeholder is not
empty, it is a **sentinel string**, so `--require` waved it straight through.

## What shipped

- **`--placeholder VALUE`** recognises the sentinel — Debezium's `__debezium_unavailable_value` is
  the common one — and refuses the record naming the field and the key. It **composes with
  `--require`**: one guards a blank, the other guards a lie.
- **`index stat` reports empty-value counts per field**, so blanking that has *already* happened in
  an existing index is detectable rather than silent. A guard that only protects future writes leaves
  every past write unexamined.

`scripts/cli-smoke.sh` gates both directions — refused when the sentinel is present, accepted when a
real value is.

## Still open

- **This is a guard, not a fix.** The real answer remains the one `p67` named: a producer-side
  connector that **re-SELECTs the row by key on update** rather than trusting the stream's `after`.
  Unbuilt.
- **The sentinel is a literal, supplied by the operator.** Nothing detects that a stream uses a
  different one, and a wrong value silently protects nothing.
