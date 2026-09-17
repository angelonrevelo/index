# 2026-09-17 — tin-shape on Wikipedia `20231101.en`

Apple M5 (4P + 6E), 24 GB, macOS; ordinary desktop load, 6.7–6.9 GB of swap in use. Runs one at a
time, 30 s per workload. `.time` files are `/usr/bin/time -l`.

| file | corpus | threads |
|---|---|---|
| `wiki-0-t8.md` | shard 00000, 706.2 MB, 156,289 docs | 8 |
| `wiki-0-t4.md` | shard 00000 | 4 |
| `wiki-012-t8.md` | shards 00000–00002, 1,844.7 MB, 468,867 docs | 8 |

Write-up: [`bench/roadmap/p93-tin-shape.md`](../../roadmap/p93-tin-shape.md).
