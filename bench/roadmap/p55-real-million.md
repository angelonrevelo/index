# P55 — the 5 ms bar at a real million, and a correction to p47

**Tier:** T1 · **Bin:** `real-million` (new)
**Status: MEASURED, 2026-09-06. The bar FAILS on real data too — 8.1–8.8 ms. `p47`'s framing was
too kind and is corrected here.**

`p47` closed the typo-tail investigation with a table separating the passing rows from the failing
ones by whether the documents were real, and a named missing input:

> **No real 1 M corpus exists to test against**, so the failing rows cannot be separated from the
> recombination artefact by measurement. That is the single most valuable missing input here.

`p51` found it while sweeping the estate for something else entirely: **presyo's `raw_product` holds
4,524,754 real rows.** One `psql | index build` pipe later, the question is answerable.

## The answer

1.2 M real product rows, real vocabulary growth, `p7`'s query construction unchanged:

| documents | terms | typo p50 | typo p99 |
|---|---|---|---|
| 50,000 | 60,134 | 137 us | 610 us |
| 250,000 | 131,789 | 444 us | 2,578 us |
| 500,000 | 156,040 | 777 us | 9,116 us |
| **1,000,000** | **202,727** | **1,068 us** | **8.1–8.8 ms** |

**The bar fails at a real million.** Three runs: 8.84, 8.69, 8.12 ms.

## What recombination was and was not doing

| 1 M documents | typo p99 | vocabulary |
|---|---|---|
| `scale`, recombined from 61,467 schools | **13.58 ms** | 41,069, **fixed** |
| `real-million`, presyo raw products | **8.3 ms** | 202,727, **grown** |

Recombination **overstated the tail by about 1.6x**, exactly as predicted — holding the vocabulary at
41,069 while multiplying documents makes every posting list longer than a real corpus produces, and
posting length is what the typo tail is made of.

**It did not invent the failure.** The bar is missed by 1.7x on genuinely real data with genuinely
real vocabulary growth.

## The correction to `p47`

`p47` wrote, and `README` and `ROADMAP` repeated:

> **On every corpus of real documents this project has, the bar is met.**

That was true when written and is **no longer the right way to say it**. It was true because the
largest real corpus available was 241,677 documents. It reads as *"real data passes, synthetic data
fails"*, and the honest statement is narrower:

> The bar is met on every real corpus **up to a quarter of a million documents**, and fails on real
> data at a million. Recombination made the failure look 1.6x worse than it is.

The separation `p47` drew was between *corpus sizes*, and it mistook that for a separation between
*real and synthetic*. Having the real million is what makes the difference visible.

## The lever, priced on real vocabulary this time

`p29` swept `search_capped` on the recombined corpus. Whether that trade survives real vocabulary is
a different question and the one an adopter has to answer:

| cap | typo p50 | typo p99 | top-10 identical | rank-1 identical |
|---|---|---|---|---|
| **16 (default, exact)** | 1,016 us | **8,262 us** | 100 % | 100 % |
| 8 | 1,012 us | 6,961 us | 99.45 % | 99.80 % |
| 4 | 960 us | 6,273 us | 96.90 % | 98.80 % |
| 2 | 844 us | **5,199 us** | 92.70 % | 96.60 % |

**Nothing reaches 5 ms.** Even cap 2 — which changes 7.3 % of top-10 answers — lands at 5.20 ms.
The curve is smoother on real vocabulary than on recombined (`p29` found cap 4 non-monotone; here it
is monotone), but the conclusion is the same one `p29`, `p27` and `p47` reached.

## Verdict

**The bar stays red and unraised**, for the fourth document running, and now for a reason nobody can
attribute to the fixture. What changed is that it is no longer *arguable*: the failure is on real
documents, real vocabulary and real queries, and the remaining options are unchanged —

- a genuinely different enumeration strategy (bucket-tiered candidate generation, still unbuilt);
- accept ~8 ms at a million and state it as the product's number;
- adopt a cap and publish the agreement figure beside it.

## Reproducing

The fixture is 54 MB and is **not vendored**. It is one pipe, and the bin prints the recipe when the
file is absent:

```sh
ssh bygelo 'sudo -u postgres psql -d presyo -At -c \
  "COPY (SELECT raw_name, vendor, source_listing_code FROM raw_product \
    WHERE raw_name IS NOT NULL LIMIT 1200000) TO STDOUT (FORMAT text)"' > presyo-1m.tsv
INDEX_MILLION_TSV=presyo-1m.tsv cargo run -p index-bench --release --bin real-million
```

That this is a one-liner is `p49`'s doing; before it, obtaining this corpus was the blocker.
