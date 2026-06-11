#!/usr/bin/env bash
# Benchmark rgx (warm daemon) vs ripgrep, and flag regressions.
#
# Usage: bench/bench.sh <repo> <pattern> [pattern...]
# Env:   RG=/path/to/rg  RGX=/path/to/rgx  HF=/path/to/hyperfine  RUNS=4
#
# rgx must be at least as fast as rg on every query (a daemon already warmed). Exit 1 if any query
# regresses beyond TOLERANCE.
set -u

REPO="${1:?usage: bench.sh <repo> <pattern>...}"; shift
RG="${RG:-/opt/homebrew/bin/rg}"
RGX="${RGX:-$(dirname "$0")/../target/release/rgx}"
HF="${HF:-hyperfine}"
RUNS="${RUNS:-5}"
TOLERANCE="${TOLERANCE:-1.10}"   # allow rgx to be up to 10% slower before calling it a regression

echo "repo=$REPO"
# Warm the daemon and wait until the index is ready.
"$RGX" "warmup_token_$$" "$REPO" >/dev/null 2>&1
for _ in $(seq 1 60); do
  ( cd "$REPO" && "$RGX" --server status 2>/dev/null | grep -q "ready=true" ) && break
  sleep 0.5
done
( cd "$REPO" && "$RGX" --server status 2>/dev/null | tr '\n' ' ' ); echo

printf "%-44s %10s %10s %9s %s\n" "pattern" "rg(ms)" "rgx(ms)" "speedup" "verdict"
regressed=0
for pat in "$@"; do
  rg_ms=$("$HF" -N -w 1 -r "$RUNS" --export-json /tmp/_b_rg.json "$RG -n -- '$pat' '$REPO'" >/dev/null 2>&1 \
    && python3 -c "import json;print(json.load(open('/tmp/_b_rg.json'))['results'][0]['mean']*1000)")
  rgx_ms=$("$HF" -N -w 1 -r "$RUNS" --export-json /tmp/_b_rgx.json "$RGX '$pat' '$REPO'" >/dev/null 2>&1 \
    && python3 -c "import json;print(json.load(open('/tmp/_b_rgx.json'))['results'][0]['mean']*1000)")
  verdict=$(python3 -c "
rg=$rg_ms; rgx=$rgx_ms; tol=$TOLERANCE
print('OK' if rgx <= rg*tol else 'REGRESSION')")
  [ "$verdict" = "REGRESSION" ] && regressed=1
  python3 -c "
rg=$rg_ms; rgx=$rgx_ms
print('%-44s %10.1f %10.1f %8.2fx %s' % ('''$pat''', rg, rgx, rg/max(rgx,0.001), '''$verdict'''))"
done
exit $regressed
