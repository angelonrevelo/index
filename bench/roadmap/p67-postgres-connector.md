# P67 — reading Postgres' own storage, and a correctness-first connector: priced, not built

**Tier:** T2 · **API:** `index apply --require` (the one piece that shipped)
**Status: ASSESSED, 2026-09-06. §7.2 is two thirds done by accident. §7.1 is not worth doing.
One latent correctness bug in `p50` was found and guarded.**

`docs/research/landscape.md` §7 named five gaps. `p57`–`p66` took §7.5 and `p49`–`p56` took the
rest; §7.1 and §7.2 were left with the note *"bigger businesses and much harder."* This prices them
against the estate rather than against intuition.

## §7.2 — the connector. Mostly already true, for a reason worth naming

> A first-party, correctness-first Postgres→index connector — **with deletes**. Meilisearch publicly
> deprioritized it in June 2025; Typesense delegates; Algolia sells a crawler. The unowned hard parts
> are all **correctness**, not throughput: TOAST placeholders blanking fields, deletes needing a
> replica identity, DDL breaking the stream, re-snapshot as the only recovery.

Taking those one at a time, measured over **255 real tables** across `life`, `advo`, `mercial`,
`blueos`, `roundrobbin`, `presyo`, `sisia` and `tna`:

### Deletes and replica identity — a non-problem here, by design

| replica identity | tables |
|---|---|
| `DEFAULT` (primary key only) | **255** |
| `FULL` | **0** |
| `NOTHING` | 0 |

Every table in the estate is `DEFAULT`, which emits **only the primary key** on a delete and **no
before-image** on an update. For most connectors that is the problem: they need the old row to know
what to retire.

**`index apply` never needs one.** `p50` collapses the stream per key and treats the last record as
the state, so a delete needs the key and nothing else, and an update needs the new row and nothing
else. `--key after.sku,before.sku` reads whichever side carries it.

That was not luck — it fell out of choosing *"a change stream is a sequence of states"* over
*"a sequence of edits"* — but it was also not foreseen. **The estate needs no `REPLICA IDENTITY FULL`
change to be consumable**, which removes the single most invasive prerequisite a connector usually
imposes.

### TOAST placeholders — a real bug, in what `p50` shipped

This one lands, and it landed on me.

| database | tables holding TOASTed data | TOAST bytes |
|---|---|---|
| presyo | 55 / 55 | **7,781 MB** |
| sisia | 67 / 67 | 981 MB |
| tna | 52 / 52 | 108 MB |
| life | 8 / 8 | 70 MB |
| roundrobbin | 44 / 44 | 352 kB |

**Every table with a TOAST relation has data in it.** Under logical decoding, an `UPDATE` that does
not touch a TOASTed column emits a **placeholder instead of the value**.

`index apply` upserts the whole document from `after`, and **the engine deliberately stores no field
text** — that is why an index is 91 bytes per document. So there is nothing to merge against:

> **A partial update is not expressible in this architecture.** An unrelated `UPDATE` to a row with
> a large description would replace the document with one whose description is blank, and search
> would quietly stop finding it. No error, no signal.

That is precisely the failure class the rest of this repo refuses to have, and `p50` shipped with it.

**What shipped as a result:** `index apply --require NAME` refuses an upsert whose named field is
empty, with the key and the input path in the message. It converts a silent blanking into a loud
stop. `scripts/cli-smoke.sh` gates both directions — refused when blank, accepted when present.

**What `--require` is not:** a fix. The real fix is on the producer side — a connector that
**re-SELECTs the row by key on update** rather than trusting the stream's `after`. That is the
"correctness-first" in §7.2's title, and it is the honest reason a first-party connector would earn
its existence rather than being a `jq` incantation.

### The prerequisite nobody mentions

The estate runs `wal_level = replica`. **Logical decoding needs `logical` and a server restart.**
Every "just point it at your database" pitch — including this repo's own README recipe for
`pg_recvlogical` — silently assumes a production change the reader has not made. Stated here so it
stops being a surprise.

### DDL and re-snapshot — untouched and honest

`ALTER TABLE` still breaks a stream, and re-snapshot is still the only recovery. `index build` from
the source *is* the re-snapshot, and it is a single pipe, so recovery is cheap — but nothing detects
that it is needed. **No checkpointing either**: `apply` does not record a stream position and cannot,
because the position belongs to whoever runs the pipe.

## §7.1 — reading Postgres' own storage. Assessed and declined

> An embedded engine that reads the source database's own storage instead of copying it. ParadeDB
> and pg_textsearch put segments in Postgres blocks — a real improvement over a sidecar — but you
> still pay a full second copy, and **you cannot add an indexed column without a full REINDEX**.

Three reasons not to:

**It contradicts the tier that is actually open.** `p57`'s claim is one index file serving a browser,
a server and an object store. An index living inside Postgres' block storage serves exactly one of
those and cannot be fetched into a tab. Taking §7.1 costs §7.5.

**The measured pain is not storage, it is the query model.** `ROADMAP` records presyo's number:
GIN produces a bitmap, never an ordered iterator, so `ORDER BY … LIMIT 24` at 12 M rows goes
**54 ms → 188,442 ms**. Living inside Postgres' pages does not fix that; it inherits it. ParadeDB and
pg_textsearch had to bring their own iterator anyway.

**The category is fragmenting, not converging.** The survey found ParadeDB (AGPL), pg_textsearch
(PostgreSQL licence), VectorChord-BM25 (AGPL/ELv2) and pg_fts all competing, **none in core, none on
RDS or Cloud SQL**, and the honest one disclosing that *Elasticsearch still wins Top-10 at 341 vs
271.91 QPS*. Entering that fight requires a Postgres extension, a version matrix, and a licence
position, to arrive fourth.

**Verdict: declined.** Recorded in `docs/roadmap-rejected.md` so it is closed with a reason rather
than left open as a plausible-sounding suggestion.

## What this leaves

- **`--require` is a guard, not a fix.** The producer-side re-read is the real answer and is unbuilt.
- **No stream checkpointing.** Deliberate — the position belongs to the pipe's owner — but it means
  exactly-once is the caller's problem and nothing says so at runtime.
- **DDL still breaks streams silently.** A schema change mid-stream produces records whose fields no
  longer map, and `apply` will happily index them under the old paths.
- **The `wal_level` prerequisite should be in the README recipe**, not just here.
