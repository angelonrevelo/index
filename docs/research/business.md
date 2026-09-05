# Business — who makes money selling search, and where the opening is

> Web sweep 2026-09-05. Prices fetched from vendor pricing pages that day; revenue from SEC/SEDAR
> filings and IR releases; developer sentiment from the HN API. `UNVERIFIED` = single-source or
> unconfirmed. The sweep's 200-call search budget was exhausted, so **Reddit and X sentiment are
> entirely absent** — the practitioner sweep in [`claim.md`](claim.md) partly covers that gap.

---

## 1. Classic site search is a declining-multiple business

**The public comps are the reality check.**

| Company | Revenue | Growth | Signal |
|---|---|---|---|
| **Elastic** (NYSE:ESTC) | FY2026 **$1.739 B**; Q1 FY27 $478.1 M | +17 % / +15 % | **monthly self-serve Cloud grew +1 %**; **~7 % of staff cut Jun 2026** |
| **Coveo** (TSX:CVO) | FY26 **$148.3 M**; Q1 FY27 $38.5 M | +11 % / +8 % | **net expansion 99 %** — the installed base is net-contracting; market cap C$392 M, ~55 % off its 52-week high |
| **Algolia** | undisclosed | — | last mark **$2.25 B, Jul 2021**; **no round since**; CEO replaced Apr 2026; Vendr median ACV **$41,650** (n=144) |
| **Klevu** | — | — | **ceased to exist** — merged with Searchspring + Intelligent Reach into **Athos Commerce** under PSG Equity, 2025-01-13 |

**Meanwhile, retrieval-for-agents:**

- **Exa**: $17 M (Jul 2024) → $85 M @ $700 M (Sep 2025, Benchmark) → **$250 M @ $2.2 B (May 2026,
  a16z)** — 3.1× in 8.5 months.
- **turbopuffer**: $75 M ARR end-2025 → **$100 M run-rate Mar 2026** on sub-$1 M disclosed primary
  capital (**UNVERIFIED** — founder-reported, single source).
- **Pinecone**, by contrast, **hired bankers in Aug 2025** after losing Notion; no round since Apr
  2023 ($100 M @ $750 M).

> **Capital and revenue growth have moved from site search to retrieval-for-agents.** This
> corroborates [`claim.md`](claim.md) §4 from the money side: the querier got smart, and the market
> repriced accordingly.

## 2. Pricing — and the complaint that actually matters

Four meters exist: **per-search/per-record** (Algolia, Coveo, Meilisearch usage),
**per-resource-hour** (Typesense, Vespa, Elastic Hosted, Qdrant), **consumption-unit** (Elastic VCU,
Pinecone RU/WU, Zilliz vCU), **per-byte-scanned** (turbopuffer, Chroma, S3 Vectors).

Verified list prices, all fetched 2026-09-05:

| Vendor | Price |
|---|---|
| Algolia | **$0.50/1K searches, $0.40/1K records** (Grow); Grow Plus $1.75/1K searches |
| Elastic | Serverless $0.14 ingest VCU-hr / $0.09 search VCU-hr / $0.047 GB-mo; **Hosted floor $99/mo** |
| Typesense | $0.03–$17.57/hr by config, **no per-query charge at all**; HA ≈ 3.2× |
| Meilisearch | $0.40/1K searches + $0.30/1K docs, or XS tier **$23/mo** |
| Vespa | $0.05–0.18/vCPU-hr — but **Enterprise minimum $20,000/mo, Enclave $10,000/mo** |
| Pinecone | $0.33/GB-mo + $16–18 per M read units; **$50/$500 mo minimums** |
| turbopuffer | $0.33/GB-mo, $2.00/GB writes, $0.001/TB scanned; **$16/$256/$4,096 mo minimums** |
| Chroma | $0.33/GiB-mo, $2.50/GiB write, $0.0075/TiB queried; Team $250/mo |
| Weaviate | Flex $45/mo + $0.00465/1M dims; Premium $400/mo |
| Exa | **$7/1K searches** — was $5 in May 2025, a **40 % rise** |
| **Qdrant, LanceDB, ParadeDB, Coveo, Constructor, Athos** | **publish no dollar figures at all** |

**The complaints, with money attached:**

- Listen Notes: *"I had to pay $50k–$100k/month… like I have to raise a pre-seed round every
  month."*
- *"Algolia quoted one of my clients $250k/year"* — replaced with ES + Lambda at **1/5 the cost**.
- Discord on the 2020 repricing: *"an order of magnitude more expensive… What are the overages??!
  huge red flag."*
- The structural gripe: **each debounced keystroke bills as a search**, and replicas multiply billed
  records.

> **But the most-repeated grievance is not price — it is unpredictability:** *"impossible for me to
> know whether… Algolia will cost $100/month or $1,000/month or $10,000/month."*

**Elastic's licensing pain is reputational, not legal.** The 2024 AGPL thread (759 points) is
wall-to-wall broken trust: *"My trust was violated. Not lifting a finger to help them… AGPL +
copyright transfers vs SSPL is a choice between getting stabbed or shot."* Peter Zaitsev: *"bowing
to OpenSearch threat."*

**Vector cost:** TigerData's benchmark puts **pgvectorscale at $835/mo vs Pinecone $3,241/mo — 75 %
cheaper** at 50 M vectors. And **AWS S3 Vectors** (GA 2025-12-03) prices storage at **$0.06/GB-mo —
5.5× below Pinecone's and turbopuffer's identical $0.33.**

### The finding that reorders everything

> **Simon Willison:** *"I've built a number of systems that run a database and a separate search
> index… **The hardest part by far is keeping the search index in sync with the database.**"*
> ([HN, 2025-04-09](https://news.ycombinator.com/item?id=43628242))
>
> *"We completely underestimated the effort of keeping the two datastores in sync and are actually
> contemplating to get rid of elasticsearch."*
>
> *"Now we have 2 problems… If we could do away with ES and go back to MySQL, we would."*
>
> **GitHub lost data from its own issues index in April 2026.**

At least **six distinct 2025–26 products exist purely to kill the dual-write/CDC problem**
(PG-Capture, CameoDB, Polygres, pgsemantic, PGSync, Sequin) versus **zero** that exist purely to
make search cheaper without changing architecture.

**Price complaints come with numbers. Sync complaints come with regret.** That asymmetry is the
single most actionable finding in this file, and it aligns exactly with
[`landscape.md`](landscape.md) §5 and §7 gap 2 — reached independently from the technical side.

## 3. OSS → revenue — what actually converted

| Company | Path | Outcome |
|---|---|---|
| **ClickHouse** | Apache-2.0 engine, usage-based cloud | **$250 M ARR / 4,000 customers (May 2026)**; **$400 M Series D at $15 B (2026-01-16)** |
| **Redis** | SSPL (Mar 2024) → forked into **Valkey** → back to AGPL with Redis 8 (2025-05-01), admitting *"the change hurt our relationship with the Redis community"* | **$300 M ARR, Jan 2026.** Lesson: **the revenue was never coming from OSS users** |
| **Qdrant** | Kept Apache-2.0, monetized the **control plane** (Hybrid Cloud, Apr 2024) | **$50 M Series B, 2026-03-12**; $85.5 M total |
| **Meilisearch** | Split **MIT + BUSL-1.1 (2025-08-27)**, gating **sharding** — the exact feature you need to scale — plus analytics and RBAC | **no round since $15 M Series A (Oct 2022)** |
| **Typesense** | **Raised $0**; publicly refuses to paywall SSO | *"fully revenue-funded and profitable"* at **10 B searches/mo** |
| **ParadeDB** | AGPL-3.0 | **$12 M Series A, 2025-07-14 (Craft)**, 4 employees, cloud still unshipped three years in |

**Two acquisition facts verified against the rumours:**

- **ParadeDB was NOT acquired** — by Databricks or anyone. Repo active (v0.25.6, Aug 2026), founder
  still posting in HN "Who is hiring" Jun 2026. The confusion is with **Databricks/Mooncake Labs**
  (pg_mooncake, Oct 2025) and **Databricks/Neon** (~$1 B, May 2025).
- **Datadog acquired Quickwit, announced 2025-01-09**, explicitly for *"strict data residency,
  privacy, and regulatory requirements"* letting customers *"maintain full data ownership and
  control in their own environments."* **Tantivy is healthier post-acquisition** — MIT, 16,040
  stars, **Paul Masurel authoring commits on 2026-09-04**, release cadence up.

**New and material: DuckDB Labs is being acquired by AWS**, announced **2026-08-26**, with MIT
licensing and Foundation governance explicitly preserved.

## 4. Can a library make money? Yes — small, via three proven lines

**Line 1 — support + indemnity + consortium, with published prices.**
SQLite/Hwaci: **Consortium $150,000/yr** (23 staff-days, developers' cellphone numbers, priority),
Technical Support $8 k–$85 k/yr, AMS $1,500/yr, Warranty of Title $6,000
([sqlite.org/prosupport.html](https://www.sqlite.org/prosupport.html)).
DuckDB Foundation: **Gold ≥ €100,000/yr** (AWS, MotherDuck, Posit), **Silver ≥ €10,000/yr**.

**Line 2 — proprietary paid extensions.** SQLite SEE **$2,000 perpetual** ($3,500 with support),
CEROD $2,000, ZIPVFS $4,000 — **20+ years of evidence that people pay four figures for an encryption
extension to a free library.**

**Line 3 — dual license / OEM royalty, historically real, now decaying.** MySQL's embedded licenses
were per the EC *"the most important revenue source"* en route to a $1 B exit — but the same
regulator called that success *"not… rivaled by many other open source products (perhaps only by the
Linux kernel)."* Qt is the live case: FY2025 net sales €216.3 M, of which **per-device royalties
€56.8 M**, now cutting up to 200 jobs and guiding royalties **down**. Everything else landed small:
Sleepycat/Berkeley DB **≤ $10 M exit, 25 people**; Ingres $28 M in 2007.

> **AGPL on a *library* is radioactive.** Oracle's 2013 AGPL flip on Berkeley DB caused Debian to
> phase it out for LMDB. It is also what currently boxes ParadeDB out of every managed cloud.

**The documented failure mode is selling the embedded library itself.** **Realm** raised $40 M,
exited to MongoDB for **$39 M** (below capital), had **100,000 developers but 350 payers**, and
MongoDB deleted the paid sync product in Sept 2025 while keeping the free on-device DB.

**The modal modern outcome is acquihire**, and the variable is governance:
Quickwit → Datadog (**good** — OSS accelerated), DuckDB Labs → AWS (**good** — Foundation statutes
protect it), **Kuzu → Apple, Oct 2025 (bad — repo archived 2025-10-10, domain NXDOMAIN)**.

## 5. "No data movement" — real at enterprise scale, and nobody sells it self-serve

**Real, and getting more real.** AWS opened its **European Sovereign Cloud on 2026-01-14** —
**€7.8 B invested**, driven by *"data residency, operational control, and governance
independence."* SAP committed **€20 B** (Sep 2025); Airbus is migrating critical apps to a sovereign
EU cloud. **Datadog bought Quickwit for exactly this reason.**

**But every incumbent's answer is sales-gated.** ClickHouse BYOC *"requires customers to sign a
committed contract."* Qdrant Hybrid Cloud, Weaviate BYOC, Zilliz BYOC, Pinecone BYOC, LanceDB
Enterprise, ParadeDB Enterprise — **every single one is quote-only, enterprise-tier.** Qdrant's pitch
is the template: *"Keep your sensitive data within your secure premises… Qdrant Cloud only receives
telemetry through an outgoing connection."*

**Nobody offers self-serve sovereignty.**

**Counter-evidence, recorded honestly:** "your data never leaves" is overwhelmingly a
consumer/prosumer privacy pitch, not an SMB purchase driver. The honest framing of the cost half:
*"It is most often not the $64. **It is about being in sovereign control of your dataplane**"* —
versus *"If $64/month seems like a lot to you, just use pgvector."*

## 6. The agent shift — every vendor renamed itself in 2026

Pinecone → "knowledge platform" (*"the company that made RAG mainstream is now betting against it"*
— The New Stack). Zilliz/Milvus 3.0 → **"Vector Lakebase"**. LanceDB → "Multimodal Lakehouse."
Weaviate → memory (Engram, Jun 2026). Chroma → "search infrastructure." Elastic → "The Search AI
Company," shipping **Agent Builder at $0.025/execution**. Algolia → Agent Studio + MCP server. Coveo
→ MCP server. Athos → a **"GEO Assistant"** for answer engines.

> **Nobody sells a vector index in 2026, because nobody can charge for one.**

Agent-search API prices: Exa **$7/1K**; Parallel **$1–5/1K**; Brave **$5/1K**; Tavily
**$0.008/credit** (1,000 free/mo); Firecrawl $83/mo for 100 K credits. Funding: Exa **$352 M @
$2.2 B**; **Nebius acquired Tavily for $275 M, 2026-02-15**; Perplexity **$21.2 B**.

**And agents are pulling away from embeddings** — *"Why are Agents better at searching with grep than
embeddings?"* (Mar 2026); **Semble, "code search for agents that uses 98 % fewer tokens than grep,"
445 points, May 2026**; Airweave (YC X25) and Nia (YC S25) both funded to "let agents search your
app."

**Market size, skeptically.** MarketsandMarkets' enterprise-search report **redacts its own numbers
in the preview** — treat analyst TAM here as unusable. The honest proxy is vendor revenue: Elastic
$1.74 B, Redis $300 M, ClickHouse $250 M, Coveo $148 M, turbopuffer $100 M. For royalty-model
expectations, the *entire worldwide embedded DBMS market was ~$1.97 B in 2007* (IDC via EC decision
COMP/M.5529) — a useful floor.

## 7. Philippines / SEA

**No PH data-localization mandate.** RA 10173 imposes **accountability, not residency**: the PIC
"remains responsible for personal data transferred to third parties… whether domestically or
internationally." The NPC issued model contractual clauses guidance in May 2024. **So a PH company
can legally use a US SaaS — it just carries the accountability.**

**Vietnam is the outlier and just tightened.** **Law 91/2025/QH15 + Decree 356/2025**, effective
**2026-01-01**: transferring Vietnamese personal data to foreign servers requires a **Transfer Impact
Assessment filed with the Ministry of Public Security within 60 days**. **Indonesia**: PDP Law
enforceable 2024-10-17, but the **PDP Agency still is not formed** and the cross-border regulation is
unpublished — weak enforcement, live legal hook. Singapore/Malaysia/Thailand: accountability, no
localization.

> **Latency and region availability are a sharper practical lever than law.** Pinecone's only APAC
> region is **ap-southeast-1 Singapore**, requiring Builder tier or above. There is **no Jakarta or
> Manila region anywhere** in this vendor set. PH SME price sensitivity against a $50/mo Pinecone
> minimum or a $99/mo Elastic floor is the real barrier — not the DPA.

## 8. Competitor table

| Vendor | Sells | Price (verified 2026-09-05) | Weakness |
|---|---|---|---|
| **Algolia** | Hosted site search + agents | $0.50/1K searches, $0.40/1K records; Elevate = call | Per-keystroke billing; unpredictable bills; no round since 2021; CEO churn |
| **Elastic** | Search/observability/security | Hosted from $99/mo; Serverless $0.14/$0.09 VCU-hr | Trust broken by SSPL; OpenSearch is the default fork; Cloud +1 %; 7 % layoff |
| **Typesense** | GPL-3 OSS + dedicated cloud | $21.60–$12,650/mo; **$0/query** | RAM-bound; **no sharding** (issue open since 2021); bus factor ~1 |
| **Meilisearch** | MIT+BUSL OSS + cloud | From $20/mo; $0.40/1K searches | **Sharding moved behind BUSL** — the OSS ceiling is now commercial |
| **Vespa** | Apache OSS + cloud + Enclave | $0.05–0.18/vCPU-hr; **Ent min $20 K/mo** | Structurally unavailable below enterprise; steep learning curve |
| **Coveo** | Enterprise/commerce search | Unpublished; Vendr median $52.5 K ACV | 8 % growth, **99 % NRR**, ~55 % drawdown |
| **Constructor** | E-commerce discovery | Unpublished; ~$150 K avg ACV | Total opacity; e-commerce only; no round since Jun 2024 |
| **Klevu** | — | **Gone → Athos Commerce** | Three-way platform migration |
| **Pinecone** | Serverless vector | $20/$50/$500 mo min; $0.33/GB-mo | Ran a sale process; **5.5× S3 Vectors' storage price**; no OSS |
| **Qdrant** | Apache OSS + Hybrid Cloud | **Zero `$` published** | Pricing opacity blocks procurement; no serverless |
| **Weaviate** | OSS + cloud + memory | Flex $45/mo; Premium $400/mo | Entry went $25 → $45 in a year; thinnest capital ($67.7 M) |
| **Chroma** | Apache-2.0 embedded + cloud | $0.33/GiB-mo; Team $250/mo | **No announced round since $18 M seed @ $75 M (2023)**; scale unproven |
| **LanceDB** | Apache-2.0 embedded + Enterprise | **Nothing published** | Left vector DB to fight Databricks/Iceberg; no ARR |
| **turbopuffer** | Object-storage search (closed) | $0.33/GB-mo; $16/$256/$4,096 min | Closed source, no free tier, **no migration tools**; 3 OSS clones already |
| **ParadeDB** | AGPL pg_search | **Nothing published**; Cloud still beta | **AGPL locks it out of RDS/Azure/Cloud SQL**; Tiger's pg_textsearch shipped on Azure claiming 4.7× faster |
| **Zilliz/Milvus** | OSS + cloud + lakehouse | $4.00/M vCU + $0.30/GB-mo | Nothing raised since Aug 2022; CVEs |
| **Exa** | Web index API for agents | $7/1K search, $12–15/1K deep | Latency (40 s–2 min); $2.2 B on undisclosed revenue |

## 9. Five defensible positions, ranked by winnability

### 1. "Kill the sync pipeline" — search that reads the tables you already have

**The strongest evidence in this entire research programme.** Willison's *"the hardest part by far"*;
production teams saying they would rip out Elasticsearch if they could; GitHub losing data from its
own index in Apr 2026; ParadeDB raising **$12 M on this exact pitch with 4 people**; and **six-plus
startups selling shovels for the same pain**.

You do not compete on price or recall. You compete on **there is no pipeline**. Winnable because the
pain is universal, dated, quotable, and unsolved for the teams who *cannot run Postgres extensions*.

### 2. "Embedded, so it works where extensions can't be installed"

ParadeDB's founder publicly conceded the boxout: *"We would happily make it available on Azure (and
all other cloud providers!) if there were a way for us to earn a living in doing so"* (Jun 2026) —
and Microsoft shipped a permissively-licensed rival on Azure HorizonDB instead.

**Nearly all managed Postgres runs where `pg_search` cannot go** — RDS no, Supabase no, Neon
deprecated with a 2026-09-21 migration deadline ([`portability.md`](portability.md) §5). **A library
linked into the *application* rather than the *database* sidesteps the extension-permission wall
entirely.** This is the largest unserved segment adjacent to a validated, funded pitch.

### 3. "Cost floor of zero for the long tail"

Every serious competitor now has a monthly minimum: Pinecone $50/$500, Weaviate $45/$400,
turbopuffer $16/$256/$4,096, Elastic $99, **Vespa $20,000**. Algolia bills per keystroke.

An in-process library has **no minimum, no idle cost, and no egress.** Evidence it converts:
*"1,000,000 records… just worked on a little $8/mo Hetzner instance"*; Typesense reaching
profitability at 10 B searches/mo on **$0 raised**, explicitly by refusing per-query pricing. Also
the PH/SEA wedge — a $50/mo minimum is a real barrier at local salary levels, and **there is no
Manila region at any vendor**.

### 4. "Sovereign by construction" — nothing to move because there is nowhere to move it to

Real: AWS European Sovereign Cloud GA 2026-01-14 on €7.8 B; SAP €20 B; Airbus migrating; **Datadog
bought Quickwit explicitly for residency**. And **every incumbent's BYOC answer is contract-gated,
enterprise-only, quote-only.**

Ranked 4th, not 1st, because it is an enterprise checkbox that rarely closes SMB deals, PH has no
localization mandate, and the strongest demand is EU — far from this market. **A reinforcer of
positions 1–3, not a standalone wedge.**

### 5. "Retrieval primitive for local / agentic workloads"

Agents demonstrably prefer grep to embeddings; Chroma's own *Context Rot* research undercuts
stuff-the-context; Elastic and Algolia both meter agent executions. An embedded engine is the natural
substrate for on-device agent memory.

Ranked last because the category is **hyper-funded and fast-moving** — Exa at $2.2 B, Tavily acquired
for $275 M — and the buyer is not yet stable. **Good tailwind, bad beachhead.**

## 10. Business-model recommendation

**Do not lead with dual-license AGPL.** Library-copyleft is what killed Berkeley DB's ecosystem and
is currently boxing ParadeDB out of every managed cloud.

1. **Keep the core permissive** (the repo is already MIT OR Apache-2.0 — keep it).
2. **Put the IP somewhere credible.** The DuckDB Foundation structure is what let both Tantivy and
   DuckDB survive their acquisitions intact; Kuzu, without it, was archived ten days after Apple
   bought it.
3. **Monetize on the two lines with published, proven numbers:** support + indemnity + a consortium
   tier (SQLite **$150 K/yr**, DuckDB Foundation **€100 K/yr** — both have real paying members), and
   proprietary paid extensions (SQLite SEE at **$2,000 perpetual**, 20 years of evidence).
4. **The venture-scale line requires inventing a control plane you can sell** — Qdrant Hybrid Cloud
   is the template.
5. **Design against the Realm failure mode:** $40 M raised, $39 M exit, 100,000 developers,
   **350 payers.**

## Marked UNVERIFIED

turbopuffer's ARR (founder-reported, single source) and its round size/valuation; Algolia's ARR;
Exa's revenue; analyst TAM figures generally; IBPAP's $40 B / 1.9 M PH IT-BPM reporting year; Chroma
post-seed funding (no announced round exists); Klevu's pre-merger funding (aggregator-only);
**Reddit and X sentiment entirely** — both inaccessible this session. Everything above came from
direct fetches of vendor pricing pages, SEC/SEDAR filings, the HN Algolia API and the GitHub API.

---

## Addendum — second pass, same day

The business lane returned a fuller report after the above was written. It confirms every figure
already recorded and adds these, which change one ranking.

### ParadeDB's licence is now its biggest liability — and it created a competitor

TigerData built `pg_textsearch` **explicitly** because *"the leading Postgres BM25 extension,
ParadeDB, is guarded behind AGPL"*, claims **4.7x faster**, and **Microsoft shipped it on Azure
HorizonDB.** ParadeDB's founder is on record: *"We would happily make it available on Azure (and all
other cloud providers!) if there were a way for us to earn a living in doing so."* Also on record
from users: *"paradedb was extremely unstable, led to serious data corruption a bunch of times."*

> A permissive competitor did not out-engineer ParadeDB. It **out-licensed** it, and a hyperscaler
> shipped the permissive one. This is the clearest argument in the whole research programme for
> keeping this repo's core `MIT OR Apache-2.0`.

### A new position: "pgvector's operational failures, without leaving the database"

**"The Case Against pgvector"** (381 points, Oct 2025) is a precise, public specification of what an
embedded engine must beat:

- HNSW builds consuming **"10+ GB of RAM"** and hours **on the production database**
- IVFFlat cluster drift
- lock contention on live inserts
- filtered search forcing pre/post-filter tradeoffs — *"become a query planner expert"*
- hybrid search requiring custom glue

**Every one of those is an architecture problem, not a Postgres problem.** An engine that indexes
alongside the primary DB without competing for its buffer pool addresses all five. The
counter-argument to "just use Postgres" already exists and was written by someone else — you only
have to be the answer to it.

**Honest ceiling, from the same thread's top reply:** *"If $64/month seems like a lot to you, just
use pgvector."* And Discourse runs pgvector *"in thousands of databases... leveraged in most of the
billions of page views we serve."* Both are true, and **the gap between them is the product.**

This displaces "cost floor of zero" to third in the ranking — it is better-evidenced and it names a
specific, technical, already-documented failure set.

### Corrections and additions to the numbers above

- **turbopuffer's floor dropped $64 to $16/mo in Oct 2025.** Enterprise is +35%. Its weaknesses,
  stated by its own customers: **no free tier, no migration tools ("one-way door")**, and three OSS
  clones appeared within a year.
- **Typesense has no sharding and none planned**, is RAM-bound at 2-3x data, has an open Raft
  correctness bug (#3028), and a realistic production floor of **~$493/mo** — not the $21.60 headline.
- **Weaviate's storage pricing is inverted** — Dedicated $0.1505/GiB > Shared $0.10/GiB.
- **Pinecone's own DRN launch advertises "77-97% cost reduction"** off its own serverless billing.
- **Amazon S3 Vectors GA 2025-12-03** at **$0.06/GB-mo** — but Zilliz's rebuttal found real
  correctness artifacts: *"when we deleted 50% of data, TopK queries requesting 20 results returned
  only 15."* **Cheap and crude.**
- **Algolia had 39 admin keys found exposed (Mar 2026)**; its free tier was cut ~20x.
- **Realm's paid sync was EOL'd 2025-09-30**, MongoDB keeping only the free on-device DB.
- **Elastic NRR is 111%**, market cap $9.64B.
- **Chroma has no publicly announced Series A or B** — the premise that it raised beyond its $18M
  seed appears to be wrong.
- **No SEA-based search/database infrastructure company of note exists. The region is a pure
  importer.**

### Revised ranking of positions

1. **Kill the sync pipeline** (unchanged — best-evidenced, and *not* a price complaint, so it is not
   undercut when Algolia discounts).
2. **pgvector's operational failures without leaving the database** (new; displaces cost floor).
3. **Self-serve sovereignty below the ~$10k/mo enterprise floor** — demand proven at the top, supply
   uniformly gated. **Caveat for the home market: PH imposes accountability, not localization — in
   Manila this sells on latency and cost, not law.**
4. **Monetize as SQLite/DuckDB do** — permissive core, foundation-safe IP, paid support + indemnity
   + proprietary extensions. Winnable as a business, capped as an outcome. **Acquihire is now the
   modal good outcome**, and governance is the variable.
5. **Retrieval primitive for agents** — highest ceiling, lowest winnability. The most crowded,
   best-funded lane in the report. **The wedge's second act, not its first.**

### Two decisions to make before committing

1. **Licence.** Keep the core permissive. Copyleft on a library killed Berkeley DB's ecosystem and is
   currently locking ParadeDB out of managed Postgres — the exact segment position 2 targets.
2. **A control plane you can charge for**, because an in-process library has no natural cloud.
   **Qdrant Hybrid Cloud is the template**: Apache-2.0 engine, paid management, data never moves.
