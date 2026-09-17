#!/usr/bin/env python3
"""Wikipedia parquet shards -> `title<TAB>text` lines for the `tin-shape` bench (p93).

    hf download wikimedia/wikipedia --repo-type dataset \
        --include "20231101.en/train-0000[0-1]-of-00041.parquet" --local-dir <dir>
    python3 scripts/tin-corpus.py <dir>/20231101.en/train-00000-of-00041.parquet ... > wiki.tsv

Tabs, newlines and carriage returns become spaces; nothing else is touched, so the engine's own
analyzer sees the article text as written.
"""
import sys

import pyarrow.parquet as pq

out = sys.stdout
for path in sys.argv[1:]:
    t = pq.read_table(path, columns=["title", "text"])
    for title, text in zip(t["title"].to_pylist(), t["text"].to_pylist()):
        clean = lambda s: s.replace("\t", " ").replace("\n", " ").replace("\r", " ")
        out.write(clean(title) + "\t" + clean(text) + "\n")
