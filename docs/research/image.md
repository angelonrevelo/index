# Image — the retrieval landscape for pixels, September 2026

> Web sweep 2026-09-06, seven parallel lanes. Every performance number carries a source URL.
> `MEASURED` = someone ran it and published the method. `CLAIMED` = vendor-authored or a blog with
> no hardware disclosure. `UNVERIFIED` = searched for, not found — recorded rather than smoothed
> over, per [`README.md`](README.md).
>
> **The honest headline: the hard part of image search is not the search.** `rclip` indexes
> 1.28 M images in 3 hours on an M1 Max. Brute-force cosine over 1 M vectors runs at 79.7 QPS on a
> laptop. The measured failures in this field are all *architectural* — a server you must run, a
> metadata store that drifts from the file, three indexes that cannot answer one question.

---

## 1. Who is alive, and what they actually search

| Project | Semantic | Face | Store | Licence | Server needed |
|---|---|---|---|---|---|
| **Immich** | CLIP (ONNX) | InsightFace | Postgres + **pgvector** + Redis | **AGPL-3.0** | Yes — 4 processes |
| **PhotoPrism** | TensorFlow labels, no CLIP text search | YuNet + sface/facenet, DBSCAN | SQLite or MariaDB | Dual/commercial | Yes |
| **LibrePhotos** | CLIP via sentence-transformers | InsightFace/ArcFace ONNX | Postgres + **FAISS** | Permissive | Yes — Django + Flask + PG |
| **digiKam** | **None** — no text-to-image | In-house KNN+SVM | SQLite / MariaDB | **GPL** | Desktop app |
| **Nextcloud Memories** | None native | `recognize` (TensorFlow) | Nextcloud's DB | **AGPL-3.0** | Yes — whole Nextcloud |
| **Photoview** | None found | Undocumented | Any SQL | **AGPL-3.0** | Yes — **stalled**, last release v2.4.0 2024-06-23 |
| **Ente (OSS)** | MobileCLIP **on-device** | MobileFaceNet on-device | Go + Postgres + S3 | **AGPL-3.0** | Yes, for sync |

Sources: [Immich #1613](https://github.com/immich-app/immich/discussions/1613),
[PhotoPrism face-recognition docs](https://docs.photoprism.app/developer-guide/vision/face-recognition/),
[LibrePhotos 2026w25](https://docs.librephotos.com/blog/2026w25/),
[nextcloud/recognize](https://github.com/nextcloud/recognize),
[Photoview releases](https://github.com/photoview/photoview/releases),
[Ente search docs](https://ente.com/help/photos/features/search-and-discovery/).

**Two structural facts fall out of that table.** Five of seven are AGPL-3.0 — network copyleft, a
real blocker for commercial embedding. And every single one needs a server process to answer a
question about files already on your disk. Ente comes closest by running inference client-side, and
still needs a server for sync.

## 2. The measured failures

These are the numbers that make the gap falsifiable rather than rhetorical.

| Evidence | Source |
|---|---|
| Immich keyword search **~1 minute** on first query after idle | [#14539](https://github.com/immich-app/immich/issues/14539) |
| Immich **OOM at >1 M assets** — Node heap ~4.1 GB, `heap out of memory` | [#7373](https://github.com/immich-app/immich/issues/7373) |
| Immich Android reported unusable at **~15 k** and again **~53 k** photos | [#9614](https://github.com/immich-app/immich/discussions/9614), [#9164](https://github.com/immich-app/immich/discussions/9164) |
| Immich microservice memory leak killed host after **~10 k** photo import | [#9414](https://github.com/immich-app/immich/issues/9414) |
| Immich **multi-day** ML tail on ~100 k imports | [#3077](https://github.com/immich-app/immich/discussions/3077) |
| digiKam **~100 k practical ceiling** on default SQLite | [pixls.us](https://discuss.pixls.us/t/digikam-database-sqlite-or-mysql/26938) |
| PhotoPrism face recognition degrades past **"hundreds"** of distinct people | [#4328](https://github.com/photoprism/photoprism/issues/4328) |
| PhotoPrism SQLite-to-MariaDB migration needs third-party tooling, **loses indexes** | [#1214](https://github.com/photoprism/photoprism/discussions/1214) |

> **No FOSS photo system publishes a sub-second search benchmark at 100 k+ images.** Several
> publish the opposite. That absence is the same shape as the one
> [`landscape.md`](landscape.md) found for text engines, and it is the opening.

**The metadata-drift failure is separate and worse**, because it is silent. Immich's external-library
rescan ignores externally-made Date/Rating edits once Immich has written its own XMP sidecar
([#25152](https://github.com/immich-app/immich/issues/25152)); Google Takeout imports group by wrong
dates with inconsistent dedup ([#24917](https://github.com/immich-app/immich/issues/24917)); a
third-party tool, [`jmathai/immich-exif`](https://github.com/jmathai/immich-exif), exists purely to
push DB-side edits back into file EXIF. A bolt-on fix for drift is evidence the drift is real.

## 3. Finding 1 — the gap is fusion, not vectors

Immich and LibrePhotos architecturally separate the vector store (pgvector, FAISS) from the
relational metadata from a separately-scheduled face-clustering job. A compound question —
*this person, at the beach, in 2019, shot on the iPhone* — is answered by application-layer fan-out
across three subsystems and an intersection in application code.

**PhotoPrism is a genuine partial counterexample and must not be erased**: its filter-string syntax
really does combine person + date + location + keyword in one query
([docs](https://docs.photoprism.app/user-guide/search/filters/)). But that is combination at the
*UX* layer, resolved underneath by the same fan-out.

So the falsifiable claim is the narrow one:

> **No system answers text, facet, numeric-range, vector and perceptual-hash predicates in one
> query plan over one index, selecting top-k once.**

That is precisely the shape `index-text` already has. It carries BM25F, MaxScore top-k, categorical
facets, numeric ranges (`p31`), sort-by-value (`p32`) and multi-segment live update. Adding a
quantised vector column and a hash column to *that* is a smaller job than building a photo app, and
it is the only part of this whole space that is not already solved by someone.

## 4. Finding 2 — at this scale, an ANN index is the wrong tool

| Measurement | Result |
|---|---|
| MEASURED — 1 M x 384-d brute force, M4 MacBook Pro, NumPy matmul | **79.7 QPS** 1 thread (12 ms), **170.5 QPS** 10 threads |
| MEASURED — 8.84 M x 384-d, same rig | 9.34 QPS 1 thread (106 ms) |
| MEASURED — Qdrant binary quantisation, 100 K OpenAI vectors | **900 MB to 128 MB** (32x) |
| MEASURED — Qdrant binary + **3-4x oversample rerank**, DBpedia | recall **0.98–0.9966** |
| MEASURED — `rclip`, 1.28 M images, M1 Max | indexed in **3 hours** |
| MEASURED — `rclip`, 84,725 photos, Celeron J3455 | **15 hours** |

Sources: [softwaredoug](https://softwaredoug.com/blog/2026/07/29/just-brute-force-embeddings),
[Qdrant binary quantization](https://qdrant.tech/articles/binary-quantization/),
[rclip](https://github.com/yurijmikhalevich/rclip).

30 k images at 512-d is **1.9 MB as binary codes**. A popcount scan over that is sub-millisecond.
Building an HNSW graph over it costs build time, memory, recall and mutability, and buys nothing.

**The crossover is somewhere around 1 M vectors**, and this project's honest corpus ceiling is far
below it — the same limit [`ROADMAP.md`](../../ROADMAP.md) already records for text.

**Caveat recorded rather than buried:** every published binary-quantisation recall number above is
for **text** embeddings. No equivalent published figure for image embeddings was found. That is why
`p58` measures recall on this repo's own corpus instead of citing Qdrant's.

> **RESOLVED 2026-09-06 — this repo measured it.** 12,007 real CLIP ViT-B/32 embeddings over the
> `p60` corpus, binary-prefilter pipeline against the exact oracle at oversample 4:
> **recall@10 = 0.9870.** That lands inside the 0.98–0.9966 band Qdrant reports for *text*
> embeddings, so the published figure does transfer — but it is now a measurement rather than an
> assumption, and it is the first image-embedding number in this file that is ours.
>
> **A second finding came free, and it is an argument for the SYNTHETIC discipline.** The same
> benchmark on *seeded* vectors reported a fused p50 of 623 µs; on **real** embeddings it is
> **1,467 µs — 2.4x higher**. Real image embeddings on a corpus that is 42.69 % duplicates are
> densely clustered, so the Hamming shortlist survives with far more ties to rerank. A benchmark
> that had quietly used synthetic vectors would have understated its own latency by more than
> double.

## 5. Finding 3 — "1:1 lossless" has a hard, measured ceiling

The compression ask contains a contradiction worth stating plainly: byte-exact recovery *is*
lossless compression, and it is bounded by information theory. An already-JPEG-encoded corpus is
already entropy-coded, and generic compressors (zstd, brotli) win **1–3%** on it.

| Approach | Result | Status |
|---|---|---|
| **JPEG XL lossless JPEG transcode** | **~20% MEASURED** (13–22% range), reconstructs original bytes incl. EXIF/ICC/XMP | libjxl, **BSD-3**, alive |
| — its failure rate | **~1% of real JPEGs cannot be losslessly reconstructed** ([libjxl #3882](https://github.com/libjxl/libjxl/issues/3882)) | must be handled, not assumed away |
| **Lepton** (Dropbox), ~22% | **ABANDONED** — archived 2023-02-14 | dead |
| **PackJPG**, ~20% | **ABANDONED** — last release 2016-01-22 | dead |
| **Brunsli**, ~22% | survives only as JXL's internal transport | superseded |
| **FLIF / FUIF** | **DEAD** — folded into JPEG XL | dead |
| **Neural lossless** (L3C, CALLIC, FLLIC) | research-stage, GPU-only, no deployable decoder | not deployable |
| WebP lossless | 23% better than ZopfliPNG, 42% better than libpng | alive, PNG-source only |
| oxipng / +zopfli | ~12% / ~18% smaller, 0.7 s / **208 s** per image | alive, PNG-source only |

Sources: [Cloudinary](https://cloudinary.com/blog/the-case-for-jpeg-xl),
[siipo.la comparison](https://siipo.la/blog/whats-the-best-lossless-image-format-comparing-png-webp-avif-and-jpeg-xl),
[dropbox/lepton](https://github.com/dropbox/lepton), [FLIF #549](https://github.com/FLIF-hub/FLIF/issues/549).

**Browser delivery is not available.** Chrome 145 (Feb 2026) merged a Rust JXL decoder but it is
**still behind a flag as of Chrome 151**; Firefox 152 shipped it **disabled by default**; only
Safari 17+ is on by default, and without progressive decode. There is **no pure-Rust JXL encoder** —
encoding means the C++ libjxl.

**Dedup dominates transcode on a scraped corpus.** ~30% of LAION-2B is duplicated
([arXiv:2303.12733](https://arxiv.org/abs/2303.12733)); SemDeDup removed up to **37%** of
LAION-440M with no downstream loss. A contrasting study flagged only ~3% of 16.8 B images — a **12x
spread** that shows any single headline number here is a methodology choice, not a fact. For a
personal library, `UNVERIFIED` — no citable duplicate-rate study was found; do not assert one.

**Conclusion for this project: do not write a codec.** Compression is a host-side *dependency
decision* (adopt libjxl, verify the round-trip with a content hash, handle the 1% that fails). What
belongs in the engine is the **content address that proves the round-trip** and the **dedup** that
does most of the work.

## 6. The cheap tier — what is worth extracting for every image

Measured or well-established per-image costs, and what each buys:

| Signal | Cost | Bytes | Verdict |
|---|---|---|---|
| dHash / aHash 64-bit | <1 ms | 8 | always — catches re-encodes and resizes |
| pHash 64-bit (DCT) | ~1–5 ms | 8 | always — survives gamma/colour shifts |
| PDQ 256-bit | **~80 ms MEASURED** (Dalins et al. 2019, p90) | 32 | only for blocklist interop or crop robustness |
| OKLab palette, 5–8 swatch | ~2–10 ms on a downsampled copy | 24–64 | always — enables colour search |
| EXIF / XMP | <1 ms, no pixel decode | tens of bytes kept | always — usually absent, see §7 |
| JPEG quantisation table | <1 ms, already parsed | ~128 | free provenance narrowing |
| CLIP/SigLIP embedding | tens of ms CPU, low ms batched GPU | 2–6 KB f32 | second tier, GPU-batched |
| PRNU sensor noise | **seconds**, needs many reference images | ~image-sized | not cheap tier — lab forensics only |

**Under 10 ms and ~150 bytes** buys dedup, near-dup, colour search and opportunistic provenance,
with no model and no GPU.

**Thresholds, measured.** Meta's recommended PDQ threshold is **<=31 of 256 bits**; random unrelated
pairs average **128/256**, the theoretical 50%. For 64-bit pHash the popular "threshold 10" advice
is **too loose** — one measured corpus had **31 of 63** flagged matches be false positives at <=10,
falling to 6 at threshold 4. **Thresholds do not port across code lengths.**

**Limits, also measured.** PDQ fails past ~5 degrees rotation and >~5% crop, and **misses ~50% of
watermarked images** at threshold 30. A perceptual hash is a pixel/frequency-domain object and
structurally cannot see a semantic duplicate. Sources:
[Dalins et al. 2019](https://ar5iv.labs.arxiv.org/html/1912.07745),
[littlesvr false-positive study](http://littlesvr.ca/grumble/2015/04/27/perceptual-hash-comparison-phash-vs-blockhash-false-positives/),
[USENIX Security '22, Jain et al.](https://www.usenix.org/system/files/sec22summer_jain.pdf) on
adversarial evasion.

## 7. EXIF is usually gone, and that decides the API

X/Twitter, Instagram, Facebook, TikTok, Snapchat, LinkedIn and Reddit all strip EXIF from the
downloadable copy. Signal strips it *and* retains nothing server-side. WhatsApp, Telegram and
Discord preserve it only on the send-as-**file** path, not the send-as-**photo** path. Sources:
[Fastio](https://fast.io/resources/social-media-photo-metadata-platforms-strip/),
[Scanly](https://scanly.co/blog/social-media-exif-stripping),
[EXIFdata.org](https://exifdata.org/blog/do-social-media-sites-strip-exif-data-2025-test) — all
vendor/blog sources, mutually consistent, `CLAIMED` rather than peer-reviewed.

Two consequences. **Design**: missing metadata is the *normal* case in a scraped corpus, so the
index must rank what it has rather than reject the document. **Interpretation**: stripping protects
the uploader from other *users*, not from the *platform*, which reads and retains the original — so
absence at the download is not absence at the source.

`UNVERIFIED` — no rigorous academic survey giving a "% of photos carrying GPS" figure was found.
Do not cite one.

## 8. The model is an input, not a dependency

| Model | Dim | Params | Licence | Zero-shot IN |
|---|---|---|---|---|
| **SigLIP B/16** | 768 | ~203 M | **Apache-2.0** | 79.1% |
| **SigLIP 2 g/16** | 1536 | multi-GB | **Apache-2.0** | **85.0%** |
| **DINOv2-S/14** | 384 | **21 M** | **Apache-2.0** | image-to-image retrieval |
| OpenCLIP (LAION-2B) | 1024–1280 | 1 B+ | Apache-2.0 | >78% |
| CLIP ViT-B/32 | 512 | ~151 M | MIT | ~63% |
| MobileCLIP2-S0 | — | 74.8 M | **Apple sample licence** — not Apache/MIT | 71.5% |
| Jina CLIP v2 | 1024 (MRL to 64) | ~0.9 B | **CC BY-NC 4.0** | multilingual |
| DINOv3 | 384–4096 | 21 M–6.7 B | **bespoke Meta licence**, gated | SOTA dense |

Papers: SigLIP [2303.15343](https://arxiv.org/abs/2303.15343), SigLIP 2
[2502.14786](https://arxiv.org/abs/2502.14786), MobileCLIP2
[2508.20691](https://arxiv.org/abs/2508.20691), MRL
[2205.13147](https://arxiv.org/abs/2205.13147), RaBitQ
[2405.12497](https://arxiv.org/abs/2405.12497).

**Matryoshka caveat.** The widely-repeated "98% of performance at 8% of the dimension" is from the
MRL paper's ImageNet **classification** setup. `UNVERIFIED` — no per-dimension (512 to 256 to 128 to
64) *image retrieval* recall curve was found. Truncating a non-MRL CLIP embedding is not safe, and
the API must not imply it is.

**This is why `index-image` takes `&[f32]` and ships no model.** The licence spread above is a
minefield that changes yearly; the ingest belongs in the host, where the user picks. It also keeps
the crate dependency-free and the WASM artifact shippable — the constraint
[`portability.md`](portability.md) §1 already established.

### Ingest cost, honestly

`UNVERIFIED` — **no trustworthy published "N CLIP images/sec on WebGPU" figure exists.** The best
adjacent MEASURED datum is transformers.js on a *text* model (`all-MiniLM-L6-v2`, Chrome 122,
macOS): batch 32, **WASM 17,338 ms vs WebGPU 284.8 ms — 61x**. Extrapolating that to a vision
encoder is an assumption, and is labelled as one. WebGPU availability is itself **65–82%** depending
on which 2026 survey you take, `shader-f16` is an *optional* feature, and `GPUDevice.lost` on driver
timeout is common enough that Chrome shipped auto-recovery for it.

The one MEASURED end-to-end ingest number in the whole field is `rclip`'s: **1.28 M images in
3 hours** on an M1 Max, and **84,725 in 15 hours** on a Celeron J3455. Budget from that, not from
vendor fps claims.

> **PARTLY RESOLVED 2026-09-06 — this repo measured a native GPU figure.** `scripts/embed-corpus.py`
> embedded **12,007 images in 133 s — 90 img/s** with CLIP ViT-B/32, fp16, batch 64, on an
> **RTX 2060 SUPER (8.6 GB)**, end to end including PIL decode from disk. Extrapolated, 30,000
> images is **~5.5 minutes** and `rclip`'s 1.28 M corpus would be **~4 hours** — the same order as
> its 3 hours on an M1 Max, which is a reassuring cross-check on both.
>
> This is a *native CUDA* number and says nothing about WebGPU, which remains `UNVERIFIED`. Note
> also that throughput rose from 67 to 90 img/s across the run as the OS file cache warmed, so the
> bottleneck here is **disk and JPEG decode, not the GPU** — consistent with the ingest budget in
> §6 treating decode as the host's dominant cost.

## 9. Video — sample shots, not frames

| Finding | Number |
|---|---|
| PySceneDetect **AdaptiveDetector** | **F1 91.59** on BBC (vs ContentDetector 86.69) at near-identical runtime — 27.8 s vs 28.2 s. Free accuracy. BSD-3 |
| TransNetV2 | SOTA-class, Apache-2.0, ~200 fps CPU single-core |
| AutoShot | beats TransNetV2 by ~1–4% F1 |
| `ffmpeg -skip_frame nokey` | **2–3x MEASURED** speedup over full-frame filtering |
| NVDEC at 1080p | only **~1.29x** over CPU — not worth the complexity below 4K |
| Keyframe-aware selection | **63–99% fewer frames** than uniform sampling, at **R@1 63.9%** on MSR-VTT — *better*, not merely cheaper |
| Retrieval accuracy vs fps | flattens past **2 fps**; 1 fps is the safe floor |
| Typical 10-min video | ~120–170 shots (ASL 3.5–6 s) vs 18,000 frames |

Sources: [PySceneDetect benchmark](https://github.com/Breakthrough/PySceneDetect/blob/main/benchmark/README.md),
[TransNetV2](https://github.com/soCzech/TransNetV2), [KeyScore](https://arxiv.org/html/2510.06509).

**Patent note that decides the decoder:** AV1 is royalty-free by AOMedia commitment (`dav1d`,
`rav1d`). H.264/HEVC are not — a BSD-2 *code* licence on a pure-Rust H.264 decoder does not remove
the patent obligation, since patents cover the technique. HEVC runs ~$2.07/unit across pools. For
anything shipped, decode via the OS/browser (WebCodecs `VideoDecoder`, ~95.5% browser coverage) or
default to AV1.

Meta's **TMK+PDQF** and **vPDQ** are BSD-licensed and production-grade for video near-dup.

## 10. Face — the engineering is easy, the licence and the law are not

**The licence trap.** InsightFace's *code* is MIT; its **pretrained weights — `buffalo_l`,
`antelopev2`, ArcFace, AdaFace's official checkpoints — are research-only and require a separate
paid commercial licence** ([insightface.ai](https://www.insightface.ai/solutions/face-recognition-licensing)).
The MIT badge does not cover the weights. Ultralytics YOLOv8/v11-face is **AGPL-3.0**. Genuinely
permissive options are thin: **MagFace** (Apache-2.0, but MS-Celeb-1M lineage, a dataset Microsoft
withdrew in 2019) and community **MobileFaceNet** (Apache-2.0, unofficial). Detection is easier —
**YuNet** (MIT, ~230 KB) and **BlazeFace/MediaPipe** (Apache-2.0) are clean.

**Accuracy, measured.** NIST FRVT found false-positive differentials across demographic groups of up
to a factor of **~7,203**, versus ~3x for false negatives — bias shows up overwhelmingly in *wrong
matches* ([NISTIR 8280](https://nvlpubs.nist.gov/nistpubs/ir/2019/nist.ir.8280.pdf)). Children are
near-unusable: **47.9% TAR@0.1%FAR for ages 0–4**. `face-api.js` was **archived 2025-02-05**.

**How real systems cluster.** Immich: modified DBSCAN, >=3 neighbours to become a core point,
recommended max distance 0.3–0.7. PhotoPrism: DBSCAN on L2-normalised embeddings with per-model
calibrated thresholds. Apple: on-device agglomerative clustering over face **and upper-body**
embeddings, two-pass. dlib: Chinese Whispers, tolerance 0.6 — and "mixes up children easily".

**The law.** EU AI Act **Art. 5(1)(e)** *absolutely prohibits* creating or expanding facial-
recognition databases through **untargeted scraping** of internet or CCTV images — no proportionality
test, no exception; prohibitions applicable **2025-02-02**, general applicability **2026-08-02**.
GDPR Art. 9 catches biometric data processed *to uniquely identify*; the household exemption is read
narrowly (CJEU **Rynes, C-212/13**). BIPA: $1,000 negligent / $5,000 intentional per violation —
**Facebook $650 M**, **Google $100 M over Google Photos face-grouping**, **Clearview $51.75 M** paid
as 23% equity. Clearview's EU fines: CNIL EUR 20 M, Garante EUR 20 M, Greece EUR 20 M, Dutch DPA
EUR 30.5 M; the UK ICO's GBP 7.5 M was reinstated by the Upper Tribunal 2025-10-07 and remitted —
unresolved.

Every European Clearview decision **rejects the "publicly available photos" defence**: converting
scraped public images into biometric vectors and clustering them *is* the violation. The Upper
Tribunal treated **"clustering similar facial vectors"** as the triggering step.

> **The operative line.** Local + single-user + no third-party access + no cross-user gallery sits
> in Rynes territory. Scraping, operator-side access, sharing, or a searchable identity gallery is
> full GDPR/AI-Act/BIPA exposure. Google's $100 M shows even a *private-library* clustering feature
> needs proper notice and consent.
>
> **So `index-image` ships the vector primitive and no weights, no detector, and no bundled model.**
> Clustering embeddings a host supplies is arithmetic this crate already does. Supplying the face
> model, the consent flow and the jurisdiction is the host's decision, and must stay there.

## 11. OSINT — the tooling is thinner than the market implies

No first-party reverse-image API exists worth having: **Google Lens has no API at all**; Yandex has
none, so every "Yandex reverse image API" is third-party scraping; **TinEye is the only sanctioned
one, from $200/month for 5,000 searches**. And that gray market is under live legal attack — Google
sued SerpApi **2025-12-19**, dismissed **2026-07-21**, refiled narrower **2026-08-10**, motion to
dismiss **2026-08-25**, **unresolved**.

Among FOSS frameworks: SpiderFoot and Recon-ng have **no image search**; Maltego CE's image
capability is a thin wrapper over paid TinEye/Google Vision transforms. The closest real building
block is **`rclip`** (MIT, "grep for images", fully local) — but it is CLI-only, with no face
clustering, no metadata fusion and no compound query.

Market datapoints: PimEyes **$29.99–$299.99/month**; FaceCheck.ID **$6–$197** credit packs,
crypto-only since late 2024; Clearview's government contracts run **$18,000/yr (FBI)** to a **$9.2 M
ICE** deal. Every index-size figure any of them publishes is self-reported and unverifiable — the
only third-party-confirmed numbers in this space are the fines. In Aug 2026 a reverse-face-lookup
service exposed **9 million face images** through an unsecured backend.

---

## What this file decides

1. **Build the fusion, not the vector index.** §3. One query plan over one index is the only
   unoccupied ground, and `index-text` is 80% of it already.
2. **Brute force, binary prefilter, int8 rerank. No graph.** §4. The crossover is ~1 M; the honest
   corpus is far below it.
3. **The model is an input.** §8. Ship no weights — licence spread, and it is what keeps the crate
   dependency-free, MIT/Apache, and WASM-shippable against seven AGPL incumbents.
4. **Do not write a codec.** §5. ~20% is the measured lossless ceiling and libjxl already has it.
   Ship the content address that *proves* 1:1, and the dedup that beats the codec anyway.
5. **The cheap tier is genuinely cheap.** §6. <10 ms and ~150 bytes per image, no GPU.
6. **Face: primitive yes, gallery no.** §10. The law, not the engineering, sets the envelope.
