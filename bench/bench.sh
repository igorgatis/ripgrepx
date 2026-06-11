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

echo "repo=$REPO  vs  $("$RG" --version | head -1)"
# Warm the daemon and wait until the index is ready.
"$RGX" "warmup_token_$$" "$REPO" >/dev/null 2>&1
for _ in $(seq 1 60); do
  ( cd "$REPO" && "$RGX" --server status 2>/dev/null | grep -qE 'state +ready' ) && break
  sleep 0.5
done
( cd "$REPO" && "$RGX" --server status 2>/dev/null | tr '\n' ' ' ); echo

# mean and stddev (ms) of a hyperfine run, as "mean stddev".
stats() { python3 -c "import json;r=json.load(open('$1'))['results'][0];print(r['mean']*1000,(r.get('stddev') or 0)*1000)"; }

printf "%-40s %14s %14s %9s %s\n" "pattern" "rg ms" "rgx ms" "speedup" "verdict"
regressed=0
for pat in "$@"; do
  "$HF" -N -w 1 -r "$RUNS" --export-json /tmp/_b_rg.json "$RG -n -- '$pat' '$REPO'" >/dev/null 2>&1
  read -r rg_m rg_s < <(stats /tmp/_b_rg.json)
  "$HF" -N -w 1 -r "$RUNS" --export-json /tmp/_b_rgx.json "$RGX '$pat' '$REPO'" >/dev/null 2>&1
  read -r rgx_m rgx_s < <(stats /tmp/_b_rgx.json)
  verdict=$(python3 -c "print('OK' if $rgx_m <= $rg_m*$TOLERANCE else 'REGRESSION')")
  [ "$verdict" = "REGRESSION" ] && regressed=1
  python3 -c "
print('%-40s %14s %14s %8.2fx %s' % ('''$pat''',
  '%.1f±%.1f' % ($rg_m, $rg_s), '%.1f±%.1f' % ($rgx_m, $rgx_s),
  $rg_m/max($rgx_m,0.001), '''$verdict'''))"
done
exit $regressed
