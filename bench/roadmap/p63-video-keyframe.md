# P63 — video as shots, not frames

**Tier:** T3 · **Bin:** `video-shot` · **API:** host-side ingest + `ImageIndex`
**Status: BUILT AND RUN, 2026-09-06. `OVERALL: PASS`, one verdict withheld.**
27 real videos (49 files, 18 exact duplicates collapsed), 65.7 video-minutes, via the system
`ffmpeg 8.1.1` out of process — no Rust decoder linked, so the H.264/HEVC patent question stays
where it already is.

| Measured | |
|---|---|
| Keyframe-only vs full decode | **1.87x** — does NOT reproduce §9's 2–3x, and says so |
| Shots at threshold 0.3 | **265 over 27 video — 4.03 per video-minute** |
| Frames per video-minute | every frame 1476 · 1 fps 59.9 · I-frame 33.3 · **shot 4.0** |
| Storage per video-minute | 1 fps 154.5 KB → **shot 10.4 KB — 14.9x less** |
| **Acceptance 4**, near-duplicate video | **CLOSED** — see below |
| **Acceptance 3**, recall vs 1 fps | **WITHHELD** — needs relevance judgements this corpus lacks |

**Acceptance 4 found three real near-duplicate pairs that SHA-256 could not.** Hashing each keyframe
and comparing videos as a bag of shot hashes scores `gelatomainvid` ↔ `gelatomainvid2`,
`brand-hero-mobile` ↔ `brand-hero`, and `why-upvc-hero-mobile` ↔ `why-upvc-hero` at **1.000 in both
directions under both code lengths**, with the next pair down at **0.067**.

**Weight those three unequally.** Only the gelato pair is strong evidence: both 1920x1080, both
19.8 s, **7 of 7 shots matched**, 33.70 MB against 15.40 MB — a lower-bitrate re-encode that exact
dedup is structurally blind to. The two `fourlinq` pairs are 2x downscales carrying **one shot
each**, so a score of 1.000 there is a single hash comparison wearing a fraction's clothes. Three
pairs at 1.000 reads like three equivalent results and is not one; the run prints the resolution,
duration and shot count beside each pair precisely so the difference cannot be skimmed past. Self-comparison is 1.0 across all 216
checks and the pair table is byte-stable across runs.

Two limits are stated rather than discovered later. It is **order-insensitive** — a bag of shot
hashes, not a true vPDQ alignment — so it detects re-uploads, re-encodes, resizes and trims, but
scores a re-ordered edit as a duplicate. And `Hash256::pdq` returns an **all-zero code for a frame
with no 2-D structure** (a fade to black), which would make every flat frame match every other; the
run counts them, and reports **0** here.

**Two findings worth keeping.** The cheap decode does **not** pay for the shot detection: scene
scoring needs every frame and costs 0.74 s per video-minute, more than the full decode it rides on.
And this corpus yields ~40 shots per 10 minutes against §9's 120–170, because hero loops and
screen-recorded decks have a 14.9 s mean shot length — a property of the corpus, not a refutation.

**Nothing in `index-image` or `index-text` changed to make any of this work.** A shot is an ordinary
`ImageDoc` with a timestamp, and its hash is the hash column an image document already carries.
Video needed **no new index type**, which is this row's own strongest evidence for `p59`.

The ask was "frame-by-frame image search on videos". The research says frame-by-frame is both more
expensive and *worse*, so this row rejects the framing and keeps the goal.

## The four numbers that decide the pipeline

| MEASURED | |
|---|---|
| Keyframe-aware selection vs uniform sampling | **63–99% fewer frames**, at **R@1 63.9%** on MSR-VTT — better, not merely cheaper |
| Retrieval accuracy vs sampling rate | flattens past **2 fps**; 1 fps is the safe floor |
| PySceneDetect `AdaptiveDetector` vs `ContentDetector` | **F1 91.59 vs 86.69** on BBC, at 27.8 s vs 28.2 s — free accuracy |
| `ffmpeg -skip_frame nokey` vs full decode | **2–3x** faster |
| NVDEC vs CPU at 1080p | only **~1.29x** — not worth the complexity below 4K |

A 10-minute video is ~18,000 frames and ~120–170 shots (average shot length 3.5–6 s). Sampling one
frame per shot is a **~100x** reduction that the retrieval literature says costs nothing.

So: **decode I-frames only, detect shots, embed the middle frame of each shot** (middle, not first —
the first frame of a shot is often still mid-transition and blurred), and store a shot as a document
with a timestamp. A search result is a *shot*, which is what a user wants anyway; sub-shot precision
is a second pass inside the one shot already found.

## What must be built

Almost nothing in this crate. A shot is an [`ImageDoc`] with a `(video_id, start_ms, end_ms)` and an
embedding — the fusion layer, the vector column and the hash column already index it. The work is
**host-side**: decode, shot-detect, pick the frame.

That asymmetry is the point, and it is the strongest available evidence that the `p59` architecture
is the right one: video search needed **no new index type at all**.

## Acceptance

1. Shot count and wall-clock per video-minute, measured, against a stated decode strategy.
2. Storage per video-minute: embedding + hash bytes, versus the 1-fps baseline it replaces.
3. A retrieval check on a labelled clip set: shot-sampled recall must be **>=** uniform 1 fps at
   materially fewer frames. If it is not, this row is wrong and goes to `docs/roadmap-rejected.md`.
4. **Near-duplicate video** via a per-shot hash sequence (Meta's vPDQ shape, BSD-licensed).

## The patent constraint, which is not negotiable

**AV1 is royalty-free** by AOMedia commitment (`dav1d`, `rav1d`). **H.264 and HEVC are not.** A
BSD-2 licence on a pure-Rust H.264 decoder does **not** remove the patent obligation, because
patents cover the technique, not the implementation; HEVC runs ~$2.07/unit across the pools.

So a shipped product decodes through the **OS or browser** (`WebCodecs VideoDecoder`, ~95.5% browser
coverage, which puts the licence on the platform where it already is), or through AV1. This crate
must not link a bundled H.264 decoder, and the benchmark must record which path it used.

## Why this is T3 and not T1

Two honest reasons, stated rather than dressed up as sequencing:

1. **No named consumer.** `ROADMAP.md`'s rule is that no row lands without one whose measured pain
   it closes. The image rows have the scraped corpus of `p60`; video has no corpus on this machine
   and no application asking for it.
2. **It is the only row here that needs a heavyweight dependency** (ffmpeg or a platform decoder),
   which is exactly the cost the rest of the tier was designed to avoid.
