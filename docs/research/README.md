# docs/research — the evidence base

Every roadmap row cites a file here. Gathered 2026-09-05 across eleven in-house applications, six
web-research lanes, one cross-model X/practitioner sweep, and a crates.io maturity audit.

**Conventions.** Every performance number carries a source URL and a date. Anything that could not
be confirmed from a primary source is marked `UNVERIFIED` inline rather than dropped or smoothed
over. Where a lane corrected its own earlier draft, the correction is recorded in place — a research
file that hides its own errors is worthless.

| File | Answers |
|---|---|
| [`demand.md`](demand.md) | **Start here.** What eleven real apps on this machine actually need. Contains Finding 0, which reordered the project. |
| [`relevance.md`](relevance.md) | Ranking and fuzzy-matching SOTA. The BM25F/fieldnorm argument that justifies writing our own scorer. The production typo-tolerance consensus. Entity resolution and the unseen-entity cliff. |
| [`speed.md`](speed.md) | How the fastest systems get their speed: dynamic pruning (and why MaxScore beats BMW), posting-list compression, the memory wall, and a ranked top-ten. |
| [`landscape.md`](landscape.md) | The embedded/drop-in engine field. Who is alive, who is dead. The sync problem. Why Postgres FTS fails structurally. Five unfilled market gaps. |
| [`portability.md`](portability.md) | WASM limits, browser storage, native bindings, edge runtimes, in-database extensions, zero-copy. Ends with a ranked ten-item distribution strategy. |
| [`business.md`](business.md) | Who makes money selling search, with verified prices. Five ranked market positions. Whether a library can make money. |
| [`claim.md`](claim.md) | What practitioners say publicly, each with a credibility verdict. Includes the learned-index verdict reached independently of `demand.md`. |
| [`build-or-buy.md`](build-or-buy.md) | crates.io maturity audit. The rule, the verdicts, and the two Triage rows it closed. |

## The three findings that matter most

1. **The learned-index core solves a problem none of the consumer apps has** — reached
   independently from the demand side ([`demand.md`](demand.md) Finding 0) and the practitioner side
   ([`claim.md`](claim.md) §1).
2. **Sync, not price, is the pain that sells.** Price complaints come with numbers; sync complaints
   come with regret. Six-plus startups exist to paper over dual-write/CDC; zero exist purely to make
   search cheaper ([`business.md`](business.md) §2, corroborating [`landscape.md`](landscape.md) §5
   from the technical side).
3. **An mmap-based index format does not fail to port to WASM — it silently reads the whole index
   into linear memory.** This decides the format ([`portability.md`](portability.md) §1).
