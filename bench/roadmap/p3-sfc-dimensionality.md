# P4 — SFC dimensionality decision experiment (Tier 1, gate-excluded, promote-on-build)

**Why this is THE experiment:** it settles the entire "one ordered key space for all modalities" thesis with a single falsifiable test. The validation sweep predicts it fails by D=32 — but a prediction is not a measurement. Run it, and you either vindicate the unification thesis (publish) or get hard proof to commit to the federated architecture. Either outcome is a win; not running it is the only loss.

## Prompt / scenario
Map D-dimensional vectors to a 1D key via a space-filling curve (Hilbert). For a query vector, scan ±W keys around its key and measure how many true nearest neighbors that window recovers.

## Inputs (frozen, committed)
- Real embeddings (e.g. a public sentence/image embedding set), projected/sampled to D ∈ {2, 8, 32, 128}.
- Window W = 1% of dataset size. Brute-force exact kNN as ground truth. Seeded.

## Pass-criteria (mechanical) — DECISION gate
- **Thesis SURVIVES iff** recall@10 ≥ 0.9 at **D=128** with W ≤ 1%.
- Report recall@10 at every D. Expected curve: ~1.0 at D=2, collapsing toward random by D=32.

## What each outcome means
- **Survives (unexpected):** the unification thesis is real for vectors → escalate, this is a publishable result.
- **Fails (expected):** commit to the **federated** architecture — ordered-key engine for low-D spatial+tables, graph/IVF engine for vectors, viewport as a shared *logical* pre-filter envelope (never a shared physical key). Log the failing recall numbers as the justification in the roadmap.

## Red→green proof
This is a measurement experiment, not a guard, so "red→green" is reframed: the harness is *correct* if (a) at D=2 recall@10 ≈ 1.0 (sanity — SFC works in 2D) and (b) ground-truth brute force matches a known-good kNN library on a tiny fixture. If D=2 recall isn't ~1.0, the harness itself is broken, not the thesis.
