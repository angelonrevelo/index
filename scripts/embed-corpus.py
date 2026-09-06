#!/usr/bin/env python
"""Produce REAL image embeddings for `image-corpus`, so `p49` can stop withholding its verdict.

`index-image` deliberately ships no model — `docs/research/image.md` §8 argues why, and the crate
doc comment states the cost of that choice plainly: *this crate cannot embed an image for you*.
This script is the other half of that bargain. It is the **host**, and it lives outside the engine
exactly as a real deployment's ingest would.

    # 1. ask the benchmark which documents it indexes, and in what order
    cargo run --release -p index-bench --bin image-corpus -- --emit-manifest manifest.tsv

    # 2. embed exactly those, in exactly that order
    python scripts/embed-corpus.py manifest.tsv embedding.bin

    # 3. now the recall verdict is real
    cargo run --release -p index-bench --bin image-corpus -- --embedding embedding.bin

# Why a manifest instead of walking the corpus again

The embedding file is **positional**: entry `i` is the vector for the benchmark's document `i`. If
this script walked the corpus itself it would have to reproduce the benchmark's sort order, its
decodability filter and its pixel cap exactly — three chances to drift silently, and a drift here
does not crash, it produces a *plausible wrong recall number*. So the benchmark emits the order it
actually used, and this script binds to it. The manifest's digest column lets a stale manifest be
detected rather than quietly mis-aligning every vector.

# The model, and why this one

**CLIP ViT-B/32** (`openai/clip-vit-base-patch32`), MIT-licensed, 151 M parameters, **512-d** — the
width `p49` prices its byte table at, so the measurement lands on the arithmetic that was specced
rather than a rescaled version of it.

It is not the best available encoder. §8 records SigLIP 2 at 85.0 % zero-shot ImageNet against
CLIP ViT-B/32's ~63 %. A better encoder would give better *retrieval*; it would not change what
`p49` measures, which is whether the binary-prefilter pipeline returns the same top-k as an exact
scan over **whatever** vectors it is given. Using the small MIT model keeps the run cheap and the
licence unambiguous.

# What this script does NOT establish

It produces vectors with real visual semantics, which is what `p49` needs and what seeded noise
could never provide. It does not turn this corpus into a semantic-relevance benchmark: the corpus
is a CDN scrape with no human judgements, and `p50`'s labelled set is built from duplicate groups
for that reason.
"""

from __future__ import annotations

import argparse
import struct
import sys
import time
from pathlib import Path

MODEL = "openai/clip-vit-base-patch32"
DIM = 512
# 8.6 GB of VRAM holds far more than this at ViT-B/32; the cap is chosen so the script also runs
# on a small GPU and on CPU without being rewritten.
BATCH = 64
# Above this share of unreadable files the run is not worth trusting, because every failure is a
# placeholder vector standing where a real one should be.
FAIL_RATIO_MAX = 0.01


def parse_manifest(path: Path) -> list[Path]:
    """One `<path>\\t<sha256>` per line, in the benchmark's own indexing order."""
    entry = []
    for n, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip():
            continue
        part = line.split("\t")
        if not part:
            sys.exit(f"{path}:{n}: empty line")
        entry.append(Path(part[0]))
    return entry


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("manifest", type=Path)
    ap.add_argument("out", type=Path)
    ap.add_argument("--batch", type=int, default=BATCH)
    ap.add_argument("--model", default=MODEL)
    ap.add_argument("--device", default=None, help="cuda / cpu; default picks cuda when present")
    arg = ap.parse_args()

    import torch
    from PIL import Image
    from transformers import CLIPModel, CLIPImageProcessor

    Image.MAX_IMAGE_PIXELS = None  # the benchmark applies its own pixel cap; do not second-guess it

    device = arg.device or ("cuda" if torch.cuda.is_available() else "cpu")
    # fp16 on GPU is a throughput decision, not an accuracy one at this size; the vectors are
    # written back as f32 either way, so the on-disk contract is unchanged.
    dtype = torch.float16 if device == "cuda" else torch.float32

    entry = parse_manifest(arg.manifest)
    print(f"manifest: {len(entry)} document, in the benchmark's indexing order")
    print(f"model:    {arg.model}  ({DIM}-d)")
    print(f"device:   {device}  dtype {dtype}")

    model = CLIPModel.from_pretrained(arg.model, torch_dtype=dtype).to(device).eval()
    proc = CLIPImageProcessor.from_pretrained(arg.model)

    dim = model.config.projection_dim
    if dim != DIM:
        print(f"note: this model is {dim}-d, not {DIM}-d; the benchmark reads the header, so this "
              f"is fine, but p49's byte table was priced at {DIM}")

    vector = [None] * len(entry)
    failed: list[Path] = []
    started = time.time()

    batch_image: list = []
    batch_index: list[int] = []

    def flush() -> None:
        if not batch_image:
            return
        px = proc(images=batch_image, return_tensors="pt")["pixel_values"].to(device, dtype)
        with torch.no_grad():
            emb = model.get_image_features(pixel_values=px)
            # L2-normalise here so cosine is a dot product downstream. `VectorColumn` normalises
            # again for Cosine, which is idempotent on a unit vector — doing it here as well keeps
            # the file meaningful on its own.
            emb = emb / emb.norm(dim=-1, keepdim=True).clamp_min(1e-12)
        out = emb.float().cpu().numpy()
        for slot, row in zip(batch_index, out):
            vector[slot] = row
        batch_image.clear()
        batch_index.clear()

    for i, path in enumerate(entry):
        try:
            with Image.open(path) as im:
                batch_image.append(im.convert("RGB"))
            batch_index.append(i)
        except Exception:
            # A placeholder, never a dropped entry: dropping would shift every subsequent vector
            # by one and silently corrupt the whole measurement. `e_0` is a valid unit vector, so
            # it cannot produce a NaN downstream, and it is counted and reported.
            failed.append(path)
            z = [0.0] * dim
            z[0] = 1.0
            vector[i] = z

        if len(batch_image) >= arg.batch:
            flush()
        if i and i % 2000 == 0:
            rate = i / max(time.time() - started, 1e-9)
            print(f"  ... {i}/{len(entry)}  ({rate:.0f} img/s)")

    flush()

    elapsed = time.time() - started
    rate = len(entry) / max(elapsed, 1e-9)
    print(f"embedded {len(entry)} in {elapsed:.1f}s  ({rate:.0f} img/s)")

    if failed:
        share = len(failed) / max(len(entry), 1)
        print(f"WARNING: {len(failed)} of {len(entry)} ({share:.2%}) could not be opened by PIL "
              f"and carry a PLACEHOLDER vector, not a real one:")
        for p in failed[:5]:
            print(f"    {p}")
        if len(failed) > 5:
            print(f"    ... and {len(failed) - 5} more")
        if share > FAIL_RATIO_MAX:
            print(f"FAIL: more than {FAIL_RATIO_MAX:.0%} of the corpus is placeholder. A recall "
                  f"figure over this file would be measuring the placeholders.")
            return 1

    with open(arg.out, "wb") as f:
        f.write(struct.pack("<II", len(entry), dim))
        for v in vector:
            f.write(struct.pack(f"<{dim}f", *v))

    size = arg.out.stat().st_size
    print(f"wrote {arg.out}  ({size} B = 8 + {len(entry)} x {dim} x 4)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
