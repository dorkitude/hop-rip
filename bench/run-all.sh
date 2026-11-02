#!/usr/bin/env bash
# The full benchmark, as run for BENCHMARKS.md: every system with 100,000 links, then the sensitivity
# variants, then 1,000,000 links for the two Rust systems. Takes several hours. Run setup-host.sh first.
set -euo pipefail
cd "$(dirname "$0")"
for s in chhoto kutt yourls shlink hop; do ./bench.py all "$s"; done
# Shlink with visit tracking off, on the data the run above left (redirect workloads only)
BENCH_VARIANT=notrack ./bench.py all shlink --reuse --tag notrack --skip mix,create,cold
# hop-rip with a raised open-file limit (redirect workloads only)
BENCH_VARIANT=nofile ./bench.py all hop --tag nofile --skip mix,create,cold
# a million links: does lookup cost grow with the store?
for s in hop chhoto; do ./bench.py all "$s" --links 1000000 --tag 1m --skip missing,mix; done
