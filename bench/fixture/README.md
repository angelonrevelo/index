`public-catalog.*` — 20 DummyJSON products (https://dummyjson.com/products?limit=20), frozen 2026-09-20.

# Fixtures — where the geometry came from

These are not synthetic. Every coordinate here was extracted from **maphy**'s own working tree on
2026-09-05, and the extraction commands are recorded so the files can be rebuilt rather than trusted.

`bench/roadmap/p9-sfc-2d.md` originally measured on clustered synthetic points and recorded the gap
in its own words — *"These are clustered synthetic points on real city centres, not maphy's actual
barangay centroids"*. These fixtures close it.

## `maphy-poi.csv` — 53,715 points

`lon,lat,layer`. Concatenated from `maphy/apps/web/public/data/poi/*.geojson`:

| layer | features |
|---|---|
| school | 50,412 |
| hospital | 2,070 |
| port | 1,209 |
| volcano | 24 |

Extent `114.28671..126.58184 E`, `4.58866..20.78719 N`. The western edge is Kalayaan, and it is the
reason `sfc_2d.rs` and `geo_join.rs` widened their grid from `116.9..126.6`: clamping those points
to the grid edge would have piled them into one cell and flattered every locality measurement.

## `maphy-province.txt` — 88 provinces, 991 rings, 37,507 vertices

From `maphy/.claude/worktrees/wonderful-agnesi-1f278a/data/processed/provinces.geojson`, which is
unclipped source geometry with PSGC codes — not tile output. Format:

```
P<TAB>psgc<TAB>name
R<TAB>ring_index<TAB>lon lat lon lat ...      # ring_index 0 = outer, >0 = hole
```

Ring sizes: mean 38, median 7, p95 206, **max 2,214**. That spread is the point — it is why the
vertex index beats the cell index here and loses on the municipal set.

## `maphy-municipal.txt` — 2,454 polygons, 73,519 vertices

Decoded from `maphy/apps/web/public/data/geoportal/municipal_covid19_summary.pmtiles` at **z8**,
via maphy's own `pmtiles` + `@mapbox/vector-tile` + `pbf` dependencies (54 tiles, polygon features
only, outer rings). Same format as above, one polygon per ring.

**Two caveats that matter, because they are not defects to hide but properties to measure against:**

- These are **tile-clipped fragments**, so a municipality spanning a tile boundary appears as
  several polygons and **fragments genuinely overlap**. Every arm in `geo-join` therefore resolves
  ties by lowest polygon id, so "first match" means the same thing everywhere.
- Coordinates are **tile-simplified** at z8, so ring sizes (mean 30, max 494) are smaller than the
  source. This set is used for the *many small polygons* regime, and the province set for the
  *few large polygons* regime.

## `maphy-place.txt` — 1,067 real places

`level<TAB>name<TAB>psgc<TAB>parent`. 17 regions, 84 provinces, 966 municipalities, decoded from the
feature properties (`region`, `prov`, `mun`, `psgc`) of
`maphy/apps/web/public/data/geoportal/municipal_covid19_summary.pmtiles` at z11.

This is the shape of maphy's `top.json`, which `scripts/export/place-index.ts` builds from
`region + province + municity`. **maphy's second search tier is absent**: `barangay.json` is ~42,000
more entries and `apps/web/public/data/place/` is empty in this checkout, so
`bench/roadmap/p12-maphy-place.md` measures the pool that loads first, not the shipped worst case.

**137 of the 1,067 entries (12.8 %) share a name with another place** — "Quezon" names six
municipalities, "San Isidro" six more. That is a property of Philippine place names, not of the
extraction, and any rank-1 metric over this fixture has to account for it.

## `blead-lead.tsv` — 25,979 real Philippine businesses

`name<TAB>industry<TAB>city`, extracted from `blead/data/lead-store.db` (`lead` table, `json`
column). 35 industries, of which **27 carry 100+ members**; 139 cities, of which only 8 do, which is
why `city` was not used as a facet.

**11.8 % of business names share a word with their own industry**, against 13.8 % for presyo's
products and categories. That near-match is what makes `bench/roadmap/p18-blead-industry.md` a fair
generalization test rather than an easier restatement of `p15`.

## Rebuilding

```sh
# points
python3 - <<'PY'
import json
row=[]
for k in ['school','hospital','port','volcano']:
    d=json.load(open(f'apps/web/public/data/poi/{k}.geojson',encoding='utf-8'))
    for f in d['features']:
        g=f.get('geometry') or {}
        if g.get('type')=='Point':
            row.append((g['coordinates'][0],g['coordinates'][1],k))
print('lon,lat,layer')
for r in row: print('%s,%s,%s'%r)
PY
```

Province and municipal extraction are recorded in the session transcript; the municipal one needs a
`FileSource` shim because `pmtiles` ships a `FetchSource` for HTTP only.

## Licence

Philippine government open data (DepEd/DOH facility lists, PSA PSGC boundaries, geoportal layers) as
redistributed by maphy. Present here only as benchmark input.
