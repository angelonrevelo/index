# P55 — face clustering: the primitive, and nothing else

**Tier:** T3 · **Bin:** `face-cluster` · **API:** `vector::cluster`
**Status: SPEC — expected RED until built. The blocker is legal, not technical.**

This is the one row in the image tier where the engineering is the easy part and would be misleading
to lead with. [`docs/research/image.md`](../../docs/research/image.md) §10 is the file that governs
it.

## What is technically trivial

A face embedding is a 512-d vector. `VectorColumn` (`p49`) already stores and searches those.
Clustering them is DBSCAN or agglomerative over cosine distance — a few hundred lines, and the same
arithmetic the vector column already does. Immich runs a modified DBSCAN (>=3 neighbours for a core
point, distance 0.3–0.7); PhotoPrism runs DBSCAN on L2-normalised embeddings; Apple runs two-pass
agglomerative clustering over face **and upper-body** embeddings.

So this row's deliverable is small: **`cluster()` over vectors the host supplies**, with no notion
that they are faces.

## What must not be built, and why

**No bundled model. No detector. No weights. Not now, not behind a flag.**

*The licence trap.* InsightFace's **code** is MIT; its **weights** — `buffalo_l`, `antelopev2`,
ArcFace, AdaFace's official checkpoints — are research-only and need a separate paid commercial
licence. The MIT badge does not cover the weights, and this is the single most common mistake in the
field. Ultralytics YOLOv8/v11-face is AGPL-3.0. The genuinely permissive options are thin: MagFace
(Apache-2.0, but MS-Celeb-1M lineage — a dataset Microsoft withdrew in 2019) and community
MobileFaceNet (Apache-2.0, unofficial).

*The law.* EU AI Act **Art. 5(1)(e)** absolutely prohibits creating or expanding facial-recognition
databases through **untargeted scraping** — no proportionality test, no exception. GDPR Art. 9
catches biometric data processed to uniquely identify, and the household exemption is read narrowly
(CJEU **Rynes, C-212/13**). BIPA damages are $1,000 negligent / $5,000 intentional per violation:
**Facebook $650 M**, **Google $100 M for Google Photos face-grouping**, **Clearview $51.75 M**.
Clearview's EU fines total roughly EUR 90 M and the UK ICO's was reinstated on appeal in Oct 2025.

Every European Clearview decision **rejects the "publicly available photos" defence**, and the Upper
Tribunal named **"clustering similar facial vectors"** as the triggering processing step. That is
this row's exact function, which is why the envelope is drawn tightly rather than left to the
integrator.

## The envelope

| | |
|---|---|
| **In scope** | `cluster()` over caller-supplied vectors, local, single-user, no notion of identity. |
| **In scope** | Unlabelled groups — "Person 1", "Person 2" — as a default UX. |
| **Opt-in only** | Attaching a real name to a cluster. |
| **Out of scope, permanently** | Any bundled face model or detector weight. |
| **Out of scope, permanently** | Untargeted scraping to seed or expand a gallery — flatly illegal under EU AI Act Art. 5 regardless of where it ships from. |
| **Out of scope, permanently** | Cross-user matching, a searchable identity gallery, or exporting biometric templates to a third party. |

## Acceptance

1. `cluster()` is correct on a labelled synthetic set: known clusters recovered, purity and
   completeness reported, **and the false-merge rate reported separately** — because §10's NIST FRVT
   finding is that demographic bias shows up in false **positives** at up to a ~7,203x differential
   versus ~3x for false negatives. A single "accuracy" number would hide exactly the failure that
   matters.
2. Determinism. Chinese Whispers is non-deterministic across runs; whatever is chosen here must not
   be, because `p51` requires a stable verdict.
3. The API must be **unable to express** a cross-corpus identity query. If it can, the envelope
   above is decoration.
4. Documentation states the accuracy limits plainly: children are near-unusable (**47.9%
   TAR@0.1%FAR at ages 0–4**), aging and masks compound, and casual photos are harder than the
   benchmark sets these numbers come from.

## Why this is T3

No named consumer, and the corpus of `p51` contains essentially no faces. Shipping a face feature
against a corpus that cannot test it would be building on an assumption — and in this row's legal
context, an untested assumption is the expensive kind.
