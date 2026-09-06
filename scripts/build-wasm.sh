#!/usr/bin/env bash
# Build every shippable WebAssembly artifact side by side, into `dist/`.
#
# Until now the recipe was four cargo invocations spread across the README, and two of them wrote to
# the SAME path: the `simd128` analytics build overwrites the baseline one, because RUSTFLAGS is not
# part of the target directory. `bench/roadmap/p35-simd128.md` recorded that as a limit -- comparing
# the two artifacts meant rebuilding between runs and remembering which one was on disk.
#
# Each build gets its own CARGO_TARGET_DIR, so the outputs coexist and a host can pick between them.
#
#   ./scripts/build-wasm.sh
#
# Sizes are reported raw and gzipped, because gzipped is what a browser actually downloads.

set -euo pipefail
cd "$(dirname "$0")/.."

TARGET=wasm32-unknown-unknown
OUT=dist
mkdir -p "$OUT"

build() {
  local crate=$1 artifact=$2 out=$3 dir=$4
  shift 4
  echo "  building $out ..."
  # A distinct target dir per FLAG SET. Without this the simd and baseline analytics builds are the
  # same path and silently replace each other.
  #
  # `-` means the DEFAULT target dir, and the default builds use it deliberately: `js/smoke.mjs`
  # and CI both read `target/$TARGET/release/`, so sending these somewhere else meant running this
  # script and then the smoke test tested a STALE artifact. Only the flagged build needs its own.
  if [ "$dir" = "-" ]; then
    "$@" cargo build -q --release -p "$crate" --target "$TARGET"
    cp "target/$TARGET/release/$artifact" "$OUT/$out"
  else
    env CARGO_TARGET_DIR="target/$dir" "$@" cargo build -q --release -p "$crate" --target "$TARGET"
    cp "target/$dir/$TARGET/release/$artifact" "$OUT/$out"
  fi
}

echo "index :: building WebAssembly artifacts into $OUT/"

build index-wasm      index_wasm.wasm      index.wasm            -
build index-geo-wasm  index_geo_wasm.wasm  index-geo.wasm        -
build index-accel     index_accel.wasm     index-accel.wasm      -
build index-accel     index_accel.wasm     index-accel.simd.wasm wasm-simd \
  RUSTFLAGS="-C target-feature=+simd128"

echo
printf "  %-24s %10s %12s\n" "artifact" "bytes" "gzipped"
for f in "$OUT"/*.wasm; do
  raw=$(wc -c < "$f" | tr -d ' ')
  gz=$(gzip -9 -c "$f" | wc -c | tr -d ' ')
  printf "  %-24s %10s %12s\n" "$(basename "$f")" "$raw" "$gz"
done

cat <<'NOTE'

  index.wasm            search: build, query, typo-correct, facet, range, sort, clauses,
                        paging, phrases, live segments, the IMAGE tier. C ABI v12, 61 symbols.
  index-geo.wasm        point-in-polygon over a cell index.
  index-accel.wasm      analytics kernels, baseline. What onegrid's ABI ships today.
  index-accel.simd.wasm same kernels with simd128: 9-16x on `bitmap_op`, unchanged elsewhere,
                        and ONLY when the output buffer does not alias an input. See p35.
                        Requires a SIMD-capable engine; onegrid's `detect.ts` already probes it.
NOTE
