#!/usr/bin/env python3
"""Turn bench/results/*.json into the markdown tables used in BENCHMARKS.md.

    bench/report.py                   # main 100k-link results
    bench/report.py --tag 1m          # results saved with --tag 1m
"""

import argparse
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORDER = ["hop", "chhoto", "shlink", "yourls", "kutt"]
NAMES = {"hop": "hop 0.3.0", "chhoto": "Chhoto URL 7.10.5", "shlink": "Shlink 5.1.8", "yourls": "YOURLS 1.10.6",
         "kutt": "Kutt 3.2.6", "shlink-notrack": "Shlink, tracking off", "hop-nofile": "hop, open-file limit raised"}


def load(tag: str, variant_files: list[str]) -> dict:
    out = {}
    for s in ORDER:
        p = HERE / "results" / f"{s}{('-' + tag) if tag else ''}.json"
        if p.exists():
            out[s] = json.loads(p.read_text())
    for f in variant_files:
        d = json.loads((HERE / "results" / f).read_text())
        out[f.removesuffix(".json")] = d
    return out


def mib(b): return f"{b / 2**20:.1f}"
def mb(b): return f"{b / 1e6:.1f}"
def ms(x): return "-" if x is None else (f"{x:.2f}" if x < 10 else f"{x:.0f}")


def point(res, kind, rate):
    w = res.get("workloads", {}).get(kind)
    if not w:
        return None
    return next((p for p in w["points"] if p["rate"] == rate), None)


def cold_p99(r):
    """The cold-cache run is at 1k/s; it only means something for a system that holds 1k/s when warm."""
    warm = point(r, "redirect", 1000)
    if warm is None or warm["error_rate"] > 0.01:
        return "n/a (under 1k/s warm)"
    return ms(r["cold_cache_redirect"]["p99_ms"])


def headline(data):
    print("| | " + " | ".join(NAMES.get(s, s) for s in data) + " |")
    print("|---" * (len(data) + 1) + "|")
    rows = [
        ("Redirects: max rate with p99 < 50 ms", lambda r: f"{r['workloads']['redirect']['max_rate_p99_under_50ms']:,}/s"),
        ("Redirects: max throughput (64 conns)", lambda r: f"{point(r, 'redirect', 0)['achieved_rps']:,.0f}/s"),
        ("Redirect latency at 1k/s, p50 / p99 / p99.9 (ms)", lambda r: "{} / {} / {}".format(
            *(ms(point(r, 'redirect', 1000)[k]) for k in ("p50_ms", "p99_ms", "p999_ms")))),
        ("Missing slugs: max rate with p99 < 50 ms", lambda r: f"{r['workloads']['missing']['max_rate_p99_under_50ms']:,}/s"),
        ("Creates: max rate with p99 < 50 ms", lambda r: f"{r['workloads']['create']['max_rate_p99_under_50ms']:,}/s"),
        ("Creates: max throughput (64 conns)", lambda r: f"{point(r, 'create', 0)['achieved_rps']:,.0f}/s"),
        ("95/5 mix: max rate with p99 < 50 ms", lambda r: f"{r['workloads']['mix']['max_rate_p99_under_50ms']:,}/s"),
        ("Cold-cache redirects at 1k/s, p99 (ms)", cold_p99),
        ("CPU-seconds per 10k redirects (at 1k/s)", lambda r: f"{point(r, 'redirect', 1000)['cpu_s_per_10k']:.2f}"),
        ("CPU-seconds per 10k creates (at 200/s)", lambda r: f"{point(r, 'create', 200)['cpu_s_per_10k']:.2f}"),
        ("Install size (images, MB)", lambda r: mb(sum(r["install_bytes"].values()))),
        ("Idle memory, anon (MiB)", lambda r: mib(r["idle"]["anon"])),
        ("Peak memory under load, anon (MiB)", lambda r: mib(max(p["peak_anon"] for w in r["workloads"].values() for p in w["points"]))),
        ("Peak memory incl. page cache (MiB)", lambda r: mib(max(p["peak_total"] for w in r["workloads"].values() for p in w["points"]))),
        ("Disk after seeding (MB, net of empty)", lambda r: mb(r["disk_seeded"]["bytes"] - r["disk_empty"]["bytes"])),
        ("Inodes after seeding (net of empty)", lambda r: f"{r['disk_seeded']['inodes'] - r['disk_empty']['inodes']:,}"),
        ("Seeding rate via API (8 conns)", lambda r: f"{r['seed']['rate']:,}/s"),
        ("Cold start to first redirect (s)", lambda r: f"{r['cold_start_s']['median']:.2f}"),
    ]
    for label, f in rows:
        cells = []
        for r in data.values():
            try:
                cells.append(f(r))
            except (KeyError, TypeError, ValueError):
                cells.append("-")
        print(f"| {label} | " + " | ".join(cells) + " |")


def ladder(data, kind):
    rates = sorted({p["rate"] for r in data.values() for p in r.get("workloads", {}).get(kind, {}).get("points", [])},
                   key=lambda x: x or 10**9)
    print("| rate | " + " | ".join(NAMES.get(s, s) for s in data) + " |")
    print("|---" * (len(data) + 1) + "|")
    for rate in rates:
        cells = []
        for r in data.values():
            p = point(r, kind, rate)
            if not p:
                cells.append("")
                continue
            err = f", **{p['error_rate'] * 100:.1f}% err**" if p["error_rate"] > 0.001 else ""
            cells.append(f"{p['achieved_rps']:,.0f}/s: {ms(p['p50_ms'])} / {ms(p['p99_ms'])} / {ms(p['p999_ms'])}{err}")
        print(f"| {'max (64 conns)' if rate == 0 else f'{rate:,}/s'} | " + " | ".join(cells) + " |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", default="")
    ap.add_argument("--variant", action="append", default=[], help="extra result file to add as a column")
    a = ap.parse_args()
    data = load(a.tag, a.variant)
    print("### Summary\n")
    headline(data)
    for kind, title in [("redirect", "Redirects of random existing slugs"), ("missing", "Missing slugs"),
                        ("mix", "95% redirects / 5% creates"), ("create", "Creates")]:
        print(f"\n### {title}\n\nEach cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).\n")
        ladder(data, kind)


if __name__ == "__main__":
    main()
