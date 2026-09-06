# P54 — video as shots, not frames

**Tier:** T3 · **Bin:** `video-shot` · **API:** host-side ingest + `ImageIndex`
**Status: SPEC — expected RED until built. Deferred deliberately; see the gate at the bottom.**

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
