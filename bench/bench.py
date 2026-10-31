#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["aiohttp>=3.9,<4"]
# ///
"""hop-rip benchmark harness: seeds each URL shortener with the same links, drives it with vegeta
at fixed request rates, and records latency, throughput and footprint.

    bench/bench.py all hop            # everything for one system, results in bench/results/hop.json
    bench/bench.py all hop --links 1000000 --tag 1m
    bench/bench.py up shlink          # just start a stack (with secrets from bench/.env)

Run bench/setup-host.sh first. The system under test runs in bench.slice (2 CPUs, 1 GiB, cores 0-1);
vegeta and this script run on the other cores (LOADGEN_CPUS, default 2-7).
"""

import argparse
import asyncio
import csv
import json
import os
import random
import resource
import secrets
import statistics
import subprocess
import sys
import threading
import time
import urllib.parse
import zlib
from pathlib import Path

import aiohttp

HERE = Path(__file__).resolve().parent
DATA = Path(os.environ.get("BENCH_DATA", "/srv/bench"))
WORK = DATA / "work"
SLICE = Path("/sys/fs/cgroup/bench.slice")
LOADGEN_CPUS = os.environ.get("LOADGEN_CPUS", "2-7")
DURATION = int(os.environ.get("BENCH_DURATION", "15"))  # seconds per run
RUNS = int(os.environ.get("BENCH_RUNS", "3"))  # runs per point; we report the median
SLO_P99_MS = 50.0

# Fixed-rate ladders (requests/s). A ladder stops after the first point that saturates.
LADDERS = {
    "redirect": [100, 200, 500, 1000, 2000, 5000, 10000, 20000, 40000],
    "missing": [100, 200, 500, 1000, 5000, 10000, 20000, 40000],
    "mix": [100, 200, 500, 1000, 2000, 5000, 10000, 20000],
    "create": [100, 200, 500, 1000, 2000, 5000, 10000],
}
MAX_WORKERS = 64  # concurrency for the max-throughput (closed-loop) runs

HOSTS = ["example.com", "example.org", "example.net", "en.wikipedia.org", "github.com", "www.iana.org"]
WORDS = ("alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar "
         "papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu").split()
SLUG_CHARS = "abcdefghijklmnopqrstuvwxyz0123456789"


# ---------------------------------------------------------------- systems

def env() -> dict:
    """Per-checkout secrets for the stacks (bench/.env, git-ignored), created on first use."""
    path = HERE / ".env"
    if not path.exists():
        names = ["CHHOTO_PASSWORD", "CHHOTO_API_KEY", "DB_ROOT_PASSWORD", "DB_PASSWORD", "SHLINK_API_KEY",
                 "YOURLS_PASSWORD", "KUTT_JWT_SECRET"]
        path.write_text("".join(f"{n}={secrets.token_urlsafe(48)}\n" for n in names))
        path.chmod(0o600)
    out = dict(line.split("=", 1) for line in path.read_text().split())
    out["KUTT_API_KEY"] = (DATA / "kutt.apikey").read_text().strip() if (DATA / "kutt.apikey").exists() else ""
    return out


class System:
    name = ""
    base = ""
    images: list[str] = []
    data_dirs: list[str] = []
    redirect_codes = {302}
    missing_codes = {404}
    create_codes = {200, 201}

    def __init__(self):
        self.env = env()

    def create(self, slug: str, url: str) -> tuple[str, str, dict, bytes]:
        raise NotImplementedError

    def create_ok(self, status: int, body: bytes) -> bool:
        return status in self.create_codes

    async def setup(self, session: aiohttp.ClientSession):
        """One-time setup after the first start (install, API keys)."""


def jbody(d: dict) -> bytes:
    return json.dumps(d).encode()


class Hop(System):
    name, base = "hop", "http://127.0.0.1:8080"
    images, data_dirs = ["hop-rip:bench"], ["hop"]

    def create(self, slug, url):
        return "POST", "/api/links", {"Content-Type": "application/json"}, jbody({"url": url, "slug": slug})


class Chhoto(System):
    name, base = "chhoto", "http://127.0.0.1:4567"
    images, data_dirs = ["sintan1729/chhoto-url:7.10.5"], ["chhoto"]
    redirect_codes = {307}

    def create(self, slug, url):
        h = {"Content-Type": "application/json", "X-API-Key": self.env["CHHOTO_API_KEY"]}
        return "POST", "/api/new", h, jbody({"shortlink": slug, "longlink": url})

    def create_ok(self, status, body):
        return status == 200 and b'"success":true' in body.replace(b" ", b"")


class Shlink(System):
    name, base = "shlink", "http://127.0.0.1:8080"
    images, data_dirs = ["shlinkio/shlink:5.1.8", "mariadb:11.4"], ["shlink-db"]

    def create(self, slug, url):
        h = {"Content-Type": "application/json", "X-Api-Key": self.env["SHLINK_API_KEY"]}
        return "POST", "/rest/v3/short-urls", h, jbody({"longUrl": url, "customSlug": slug})


class Yourls(System):
    name, base = "yourls", "http://127.0.0.1:8080"
    images, data_dirs = ["yourls:1.10.6-apache", "mariadb:11.4"], ["yourls-db"]
    redirect_codes = {301}
    missing_codes = {302}  # YOURLS sends unknown keywords to the site root

    def create(self, slug, url):
        form = {"action": "shorturl", "format": "json", "url": url, "keyword": slug,
                "username": "admin", "password": self.env["YOURLS_PASSWORD"]}
        h = {"Content-Type": "application/x-www-form-urlencoded"}
        return "POST", "/yourls-api.php", h, urllib.parse.urlencode(form).encode()

    def create_ok(self, status, body):
        return status == 200 and b'"status":"success"' in body.replace(b" ", b"")

    async def setup(self, session):
        async with session.post(self.base + "/admin/install.php", data={"install": "1"}) as r:
            text = await r.text()
        if "installed" not in text.lower() and "success" not in text.lower():
            raise RuntimeError(f"YOURLS install failed: {r.status} {text[:300]}")


class Kutt(System):
    name, base = "kutt", "http://127.0.0.1:3000"
    images, data_dirs = ["kutt/kutt:v3.2.6"], ["kutt"]
    missing_codes = {302}  # Kutt redirects unknown addresses to /404

    def create(self, slug, url):
        h = {"Content-Type": "application/json", "X-API-KEY": self.env["KUTT_API_KEY"], "Accept": "application/json"}
        return "POST", "/api/v2/links", h, jbody({"target": url, "customurl": slug})

    async def setup(self, session):
        h = {"Accept": "application/json"}
        pw = secrets.token_urlsafe(18) + "aA1!"
        async with session.post(self.base + "/api/auth/create-admin", json={"email": "bench@example.com", "password": pw}, headers=h) as r:
            body = await r.json(content_type=None)
            if r.status != 201:
                raise RuntimeError(f"kutt create-admin: {r.status} {body}")
        # create-admin's token has no usable user id on SQLite (Kutt 3.2.6), so log in for a fresh one
        async with session.post(self.base + "/api/auth/login", json={"email": "bench@example.com", "password": pw}, headers=h) as r:
            body = await r.json(content_type=None)
            if r.status != 200:
                raise RuntimeError(f"kutt login: {r.status} {body}")
        h["Cookie"] = f"token={body['token']}"  # Kutt reads the JWT from this cookie only
        async with session.post(self.base + "/api/auth/apikey", headers=h) as r:
            body = await r.json(content_type=None)
            if r.status != 201:
                raise RuntimeError(f"kutt apikey: {r.status} {body}")
        (DATA / "kutt.apikey").write_text(body["apikey"])
        self.env["KUTT_API_KEY"] = body["apikey"]


SYSTEMS = {c.name: c for c in [Hop, Chhoto, Shlink, Yourls, Kutt]}


# ---------------------------------------------------------------- helpers

def sh(cmd: list[str] | str, check=True, capture=False, **kw) -> str:
    r = subprocess.run(cmd, shell=isinstance(cmd, str), check=check, text=True,
                       stdout=subprocess.PIPE if capture else None, **kw)
    return r.stdout if capture else ""


VARIANT = os.environ.get("BENCH_VARIANT", "")  # e.g. "nofile" adds compose/<system>-nofile.yml


def compose(sysname: str, *args: str, capture=False):
    e = {**os.environ, **env(), "BENCH_DATA": str(DATA), "BENCH_UID": str(os.getuid()), "BENCH_GID": str(os.getgid())}
    files = ["-f", str(HERE / "compose" / f"{sysname}.yml")]
    if VARIANT:
        files += ["-f", str(HERE / "compose" / f"{sysname}-{VARIANT}.yml")]
    return sh(["docker", "compose", "-p", f"bench-{sysname}", *files, *args], env=e, capture=capture)


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


def cg_read(name: str) -> str:
    return (SLICE / name).read_text()


def cg_cpu_usec() -> int:
    return int(next(l.split()[1] for l in cg_read("cpu.stat").splitlines() if l.startswith("usage_usec")))


def cg_breakdown() -> dict:
    """Where the slice's charged memory sits: process memory, page cache, kernel (slab, dentries, ...)."""
    stat = dict(l.split() for l in cg_read("memory.stat").splitlines())
    return {k: int(stat[k]) for k in ("anon", "file", "kernel", "slab_reclaimable")} | {"current": int(cg_read("memory.current"))}


def drop_caches():
    sh("sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null")


def cg_mem() -> tuple[int, int]:
    """(anonymous memory, total charged incl. page cache) of the slice, bytes."""
    anon = int(next(l.split()[1] for l in cg_read("memory.stat").splitlines() if l.startswith("anon ")))
    return anon, int(cg_read("memory.current"))


class MemSampler:
    def __init__(self, interval=0.25):
        self.interval, self.peak_anon, self.peak_total = interval, 0, 0
        self._stop = threading.Event()
        self._t = threading.Thread(target=self._run, daemon=True)

    def _run(self):
        while not self._stop.is_set():
            a, t = cg_mem()
            self.peak_anon, self.peak_total = max(self.peak_anon, a), max(self.peak_total, t)
            self._stop.wait(self.interval)

    def __enter__(self):
        self._t.start()
        return self

    def __exit__(self, *exc):
        self._stop.set()
        self._t.join()


def disk_usage(sysobj: System) -> dict:
    sh("sync")
    total_bytes = total_inodes = 0
    for d in sysobj.data_dirs:
        p = DATA / d
        total_bytes += int(sh(["sudo", "du", "-sB1", str(p)], capture=True).split()[0])
        total_inodes += int(sh(["sudo", "du", "-s", "--inodes", str(p)], capture=True).split()[0])
    return {"bytes": total_bytes, "inodes": total_inodes}


def image_sizes(sysobj: System) -> dict:
    """Unpacked on-disk size of each image (what `docker image ls` shows), bytes."""
    units = {"B": 1, "kB": 1e3, "KB": 1e3, "MB": 1e6, "GB": 1e9}
    out = {}
    for img in sysobj.images:
        size = sh(["docker", "image", "ls", "--format", "{{.Size}}", img], capture=True).split()[0]
        num = size.rstrip("kKMGB")
        out[img] = int(float(num) * units[size[len(num):]])
    return out


def image_download_sizes(sysobj: System) -> dict:
    """Compressed size of each image (roughly what a pull downloads), bytes."""
    return {img: int(sh(["docker", "image", "inspect", "-f", "{{.Size}}", img], capture=True)) for img in sysobj.images}


def pct(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return float("nan")
    k = min(len(sorted_vals) - 1, max(0, int(round(p / 100 * len(sorted_vals) + 0.5)) - 1))
    return sorted_vals[k]


# ---------------------------------------------------------------- dataset

def dataset(n: int) -> list[tuple[str, str]]:
    """n (slug, long URL) pairs, identical for every system. Slugs are 7 random [a-z0-9]; URLs are unique."""
    rng = random.Random(20261031)
    seen, out = set(), []
    while len(out) < n:
        slug = "".join(rng.choice(SLUG_CHARS) for _ in range(7))
        if slug in seen or slug[0].isdigit():
            continue
        seen.add(slug)
        out.append((slug, long_url(rng, len(out))))
    return out


def long_url(rng: random.Random, i: int) -> str:
    words = "-".join(rng.choice(WORDS) for _ in range(rng.randint(4, 9)))
    return (f"https://{rng.choice(HOSTS)}/articles/{2015 + i % 12}/{words}-{i}"
            f"?utm_source=newsletter&utm_medium=email&utm_campaign={rng.choice(WORDS)}{rng.randint(1, 999)}")


class FreshSlugs:
    """New slugs for create runs: 'n' + 7 random chars, never colliding with the seeded 7-char slugs."""

    def __init__(self, seed: int):
        self.rng, self.i = random.Random(seed), 0

    def next(self) -> tuple[str, str]:
        self.i += 1
        return "n" + "".join(self.rng.choice(SLUG_CHARS) for _ in range(7)), long_url(self.rng, 10_000_000 + self.i)


# ---------------------------------------------------------------- seeding (API, 8 connections)

async def seed(sysobj: System, links: list[tuple[str, str]], concurrency=8) -> dict:
    conn = aiohttp.TCPConnector(limit=concurrency)
    failed: list[str] = []
    done = 0
    t0 = time.time()
    async with aiohttp.ClientSession(connector=conn, timeout=aiohttp.ClientTimeout(total=60)) as s:
        q: asyncio.Queue = asyncio.Queue()
        for l in links:
            q.put_nowait(l)

        async def worker():
            nonlocal done
            while not q.empty():
                slug, url = q.get_nowait()
                for attempt in range(5):
                    m, path, h, body = sysobj.create(slug, url)
                    try:
                        async with s.request(m, sysobj.base + path, headers=h, data=body) as r:
                            rb = await r.read()
                            if sysobj.create_ok(r.status, rb):
                                break
                            err = f"{r.status} {rb[:200]!r}"
                    except Exception as e:  # noqa: BLE001
                        err = repr(e)
                    await asyncio.sleep(0.2 * (attempt + 1))
                else:
                    failed.append(f"{slug}: {err}")
                done += 1
                if done % 20000 == 0:
                    log(f"  seeded {done}/{len(links)} ({done / (time.time() - t0):.0f}/s)")

        await asyncio.gather(*[worker() for _ in range(concurrency)])
    secs = time.time() - t0
    if failed:
        log(f"  {len(failed)} creates failed, e.g. {failed[:3]}")
    return {"links": len(links), "seconds": round(secs, 1), "rate": round(len(links) / secs), "failed": len(failed)}


async def verify(sysobj: System, links, sample=300) -> dict:
    """Checks that redirects go to the right long URL and that missing slugs don't."""
    rng = random.Random(7)
    bad = []
    async with aiohttp.ClientSession() as s:
        for slug, url in rng.sample(links, min(sample, len(links))):
            async with s.get(f"{sysobj.base}/{slug}", allow_redirects=False) as r:
                loc = r.headers.get("Location", "")
                if r.status not in sysobj.redirect_codes or urllib.parse.unquote(loc).rstrip("/") != url.rstrip("/"):
                    # normalisation differences (e.g. trailing slash, escaping) are fine; wrong targets are not
                    if r.status not in sysobj.redirect_codes or url.split("?")[0] not in urllib.parse.unquote(loc):
                        bad.append((slug, r.status, loc[:120]))
        for i in range(50):
            async with s.get(f"{sysobj.base}/zz{i:05d}x", allow_redirects=False) as r:
                if r.status not in sysobj.missing_codes:
                    bad.append((f"zz{i:05d}x", r.status, "missing slug"))
    if bad:
        log(f"  verify: {len(bad)} problems, e.g. {bad[:3]}")
    return {"checked": sample + 50, "problems": len(bad), "examples": bad[:3]}


async def wait_ready(sysobj: System, slug: str | None, timeout=300.0) -> float:
    """Seconds until GET /<slug> answers with a redirect (or, with no slug yet, any HTTP answer)."""
    t0 = time.monotonic()
    async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=2)) as s:
        while time.monotonic() - t0 < timeout:
            try:
                async with s.get(f"{sysobj.base}/{slug or ''}", allow_redirects=False) as r:
                    if slug is None and r.status < 500 or r.status in sysobj.redirect_codes:
                        return time.monotonic() - t0
            except Exception:  # noqa: BLE001
                pass
            await asyncio.sleep(0.01)
    raise TimeoutError(f"{sysobj.name} not ready after {timeout}s")


# ---------------------------------------------------------------- load (vegeta)

def write_get_targets(path: Path, base: str, slugs: list[str]):
    path.write_text("".join(f"GET {base}/{s}\n" for s in slugs))


def json_target(sysobj: System, method, path, headers, body=b"") -> str:
    t = {"method": method, "url": sysobj.base + path, "header": {k: [v] for k, v in headers.items()}}
    if body:
        import base64
        t["body"] = base64.b64encode(body).decode()
    return json.dumps(t, separators=(",", ":"))


def write_json_targets(path: Path, sysobj: System, n: int, fresh: FreshSlugs, get_slugs: list[str] | None,
                       create_every: int = 0):
    """create_every=0: all creates. create_every=20: one create per 20 requests (95/5), rest GETs."""
    rng = random.Random(n)
    with path.open("w") as f:
        for i in range(n):
            if get_slugs is not None and (create_every == 0 or rng.randrange(create_every) != 0):
                f.write(json_target(sysobj, "GET", "/" + rng.choice(get_slugs), {}) + "\n")
            else:
                f.write(json_target(sysobj, *sysobj.create(*fresh.next())) + "\n")


def vegeta(sysobj: System, kind: str, targets: Path, fmt: str, rate: int, out: Path) -> dict:
    """One run. rate=0 means closed-loop max throughput with MAX_WORKERS concurrent requests."""
    cmd = ["taskset", "-c", LOADGEN_CPUS, "vegeta", "attack", "-format", fmt, "-targets", str(targets),
           "-duration", f"{DURATION}s", "-redirects", "-1", "-timeout", "10s", "-output", str(out)]
    cmd += ["-rate", "0", "-workers", str(MAX_WORKERS), "-max-workers", str(MAX_WORKERS)] if rate == 0 else ["-rate", f"{rate}/s"]
    if fmt == "json":
        cmd.append("-lazy")
    cpu0 = cg_cpu_usec()
    with MemSampler() as mem:
        sh(cmd)
    cpu_s = (cg_cpu_usec() - cpu0) / 1e6
    r = summarize(sysobj, kind, out)
    r.update(rate=rate, cpu_seconds=round(cpu_s, 2), peak_anon=mem.peak_anon, peak_total=mem.peak_total,
             cpu_s_per_10k=round(cpu_s / max(1, r["requests"]) * 10_000, 3))
    out.unlink()
    return r


def summarize(sysobj: System, kind: str, out: Path) -> dict:
    lat, codes, errs = [], {}, 0
    t_first, t_last = None, 0
    ok = {"redirect": sysobj.redirect_codes, "missing": sysobj.missing_codes}.get(kind)
    p = subprocess.Popen(["vegeta", "encode", "-to", "csv", str(out)], stdout=subprocess.PIPE, text=True)
    for row in csv.reader(p.stdout):
        ts, code, latency = int(row[0]), int(row[1]), int(row[2])
        method = row[9] if len(row) > 9 else "GET"
        codes[code] = codes.get(code, 0) + 1
        expect = ok if ok is not None else (sysobj.redirect_codes if method == "GET" else sysobj.create_codes)
        if code not in expect:
            errs += 1
        lat.append(latency / 1e6)
        t_first = ts if t_first is None else min(t_first, ts)
        t_last = max(t_last, ts + latency)
    p.wait()
    n = len(lat)
    lat.sort()
    span = max(1e-9, (t_last - (t_first or 0)) / 1e9)
    return {"requests": n, "errors": errs, "error_rate": round(errs / max(1, n), 5),
            "achieved_rps": round((n - errs) / span), "p50_ms": round(pct(lat, 50), 3),
            "p99_ms": round(pct(lat, 99), 3), "p999_ms": round(pct(lat, 99.9), 3),
            "max_ms": round(lat[-1], 3) if lat else None, "codes": codes}


def median_run(runs: list[dict]) -> dict:
    keys = ["achieved_rps", "p50_ms", "p99_ms", "p999_ms", "error_rate", "cpu_s_per_10k", "requests"]
    m = {k: statistics.median(r[k] for r in runs) for k in keys}
    m.update(rate=runs[0]["rate"], peak_anon=max(r["peak_anon"] for r in runs),
             peak_total=max(r["peak_total"] for r in runs), runs=runs)
    return m


def saturated(m: dict) -> bool:
    return m["error_rate"] > 0.01 or (m["rate"] > 0 and m["achieved_rps"] < 0.9 * m["rate"])


def run_ladder(sysobj: System, kind: str, slugs: list[str], missing: list[str], fresh: FreshSlugs) -> dict:
    """Fixed rates up the ladder until one saturates, then (except for the mix) a max-throughput run."""
    wd = WORK / sysobj.name
    wd.mkdir(parents=True, exist_ok=True)
    gets = wd / f"{kind}.txt"
    if kind in ("redirect", "missing"):
        write_get_targets(gets, sysobj.base, slugs if kind == "redirect" else missing)
    points, best = [], 0

    def point(rate: int) -> dict:
        restarted = ensure_up(sysobj, slugs[0])
        runs = []
        for _ in range(RUNS):
            if kind in ("redirect", "missing"):
                tf, fmt = gets, "http"
            else:
                n = int((rate * DURATION if rate else max(best, 200) * DURATION * 3) * 1.15) + 2000
                tf, fmt = wd / f"{kind}.json", "json"
                write_json_targets(tf, sysobj, n, fresh, slugs if kind == "mix" else None, 20 if kind == "mix" else 0)
            runs.append(vegeta(sysobj, kind, tf, fmt, rate, wd / "out.bin"))
            time.sleep(2)
        m = median_run(runs)
        m["restarted_before"] = restarted
        log(f"  {kind:8} rate={rate or 'max':>6} achieved={m['achieved_rps']:>7.0f} p50={m['p50_ms']:.2f} "
            f"p99={m['p99_ms']:.2f} p99.9={m['p999_ms']:.2f} err={m['error_rate']:.4f}")
        return m

    for rate in LADDERS[kind]:
        m = point(rate)
        points.append(m)
        if saturated(m):
            break
        best = max(best, m["achieved_rps"])
    if kind != "mix":
        points.append(point(0))
    slo = max((p["rate"] for p in points if p["rate"] and p["p99_ms"] < SLO_P99_MS and not saturated(p)), default=0)
    return {"points": points, "max_rate_p99_under_50ms": slo}


def ensure_up(sysobj: System, slug: str) -> bool:
    """Before each point: if an overload in the last point took the server down, bring it back up, so
    one crash doesn't turn every later point into connection errors. Returns True if it had to."""
    try:
        asyncio.run(wait_ready(sysobj, slug, timeout=30))
        return False
    except TimeoutError:
        log(f"  [{sysobj.name}] not answering; restarting the stack")
        compose(sysobj.name, "up", "-d", "--quiet-pull")
        asyncio.run(wait_ready(sysobj, slug))
        return True


def cold_cache(sysobj: System, slugs: list[str], rate=1000) -> dict:
    wd = WORK / sysobj.name
    gets = wd / "cold.txt"
    rng = random.Random(99)
    write_get_targets(gets, sysobj.base, rng.sample(slugs, len(slugs)))
    runs = []
    for _ in range(RUNS):
        drop_caches()
        runs.append(vegeta(sysobj, "redirect", gets, "http", rate, wd / "out.bin"))
        time.sleep(2)
    m = median_run(runs)
    log(f"  cold-cache rate={rate} p50={m['p50_ms']:.2f} p99={m['p99_ms']:.2f} p99.9={m['p999_ms']:.2f} err={m['error_rate']:.4f}")
    return m


# ---------------------------------------------------------------- orchestration

def wipe(sysobj: System):
    compose(sysobj.name, "down", "--remove-orphans")
    for d in sysobj.data_dirs:
        sh(["sudo", "rm", "-rf", str(DATA / d)])
        (DATA / d).mkdir(parents=True)
    if sysobj.name == "kutt":
        (DATA / "kutt.apikey").unlink(missing_ok=True)


async def cold_start(sysobj: System, slug: str) -> float:
    compose(sysobj.name, "down")
    sh("sync")
    t0 = time.monotonic()
    compose(sysobj.name, "up", "-d", "--quiet-pull")
    await wait_ready(sysobj, slug)
    return time.monotonic() - t0


def run_all(name: str, n_links: int, tag: str, skip: set[str], reuse: bool = False):
    sysobj = SYSTEMS[name]()
    res: dict = {"system": name, "variant": VARIANT, "links": n_links, "duration_s": DURATION, "runs_per_point": RUNS,
                 "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    out = HERE / "results" / f"{name}{('-' + tag) if tag else ''}.json"
    save = lambda: out.write_text(json.dumps(res, indent=1))  # noqa: E731
    links = dataset(n_links)
    slugs = [s for s, _ in links]
    missing = [f"m{s[1:]}x" for s in slugs[:20000]]  # 8 chars, never seeded
    fresh = FreshSlugs(zlib.crc32((name + tag).encode()))  # tag: reruns on reused data need new slugs

    if reuse:  # keep the data a previous run left (for variants that only change the server's config)
        log(f"[{name}] reusing existing data")
        compose(name, "up", "-d", "--quiet-pull")
        asyncio.run(wait_ready(sysobj, slugs[0]))
        res["reused_data"] = True
        res["install_bytes"] = image_sizes(sysobj)
    else:
        log(f"[{name}] fresh start")
        wipe(sysobj)
        compose(name, "up", "-d", "--quiet-pull")
        asyncio.run(wait_ready(sysobj, None))
        asyncio.run(_setup(sysobj))
        sysobj.env = env()
        res["install_bytes"] = image_sizes(sysobj)
        res["download_bytes"] = image_download_sizes(sysobj)
        res["disk_empty"] = disk_usage(sysobj)

        log(f"[{name}] seeding {n_links} links")
        res["seed"] = asyncio.run(seed(sysobj, links))
        res["disk_seeded"] = disk_usage(sysobj)
        res["verify"] = asyncio.run(verify(sysobj, links))
        res["memory_after_seed"] = cg_breakdown()
        # Start the measurements like a restarted server with existing data, not straight after a write burst:
        # seeding leaves page cache and kernel metadata charged to the slice (for hop, ~900 MB of the 1 GiB).
        drop_caches()
        save()

    log(f"[{name}] cold starts")
    starts = [asyncio.run(cold_start(sysobj, slugs[0])) for _ in range(3)]
    res["cold_start_s"] = {"median": round(statistics.median(starts), 3), "all": [round(s, 3) for s in starts]}
    time.sleep(15)
    a, t = cg_mem()
    res["idle"] = {"anon": a, "total": t, "breakdown": cg_breakdown()}
    log(f"  cold start {res['cold_start_s']['median']}s, idle anon {a / 2**20:.1f} MiB")

    # warm-up: 30k redirects
    wd = WORK / name
    wd.mkdir(parents=True, exist_ok=True)
    write_get_targets(wd / "warm.txt", sysobj.base, slugs)
    vegeta(sysobj, "redirect", wd / "warm.txt", "http", 2000, wd / "out.bin")

    res["workloads"] = {}
    for kind in ["redirect", "missing", "mix", "create"]:
        if kind in skip:
            continue
        log(f"[{name}] {kind}")
        res["workloads"][kind] = run_ladder(sysobj, kind, slugs, missing, fresh)
        save()
    if "cold" not in skip:
        res["cold_cache_redirect"] = cold_cache(sysobj, slugs)
    res["disk_end"] = disk_usage(sysobj)
    res["finished"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    save()
    compose(name, "down")
    log(f"[{name}] done -> {out}")


async def _setup(sysobj: System):
    async with aiohttp.ClientSession() as s:
        await sysobj.setup(s)


def _cpus(part: str) -> range:
    lo, _, hi = part.partition("-")
    return range(int(lo), int(hi or lo) + 1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["all", "up", "down", "seed-only"])
    ap.add_argument("system", choices=list(SYSTEMS))
    ap.add_argument("--links", type=int, default=100_000)
    ap.add_argument("--tag", default="")
    ap.add_argument("--reuse", action="store_true", help="keep existing data: no wipe, setup or seeding")
    ap.add_argument("--skip", default="", help="comma list of redirect,missing,mix,create,cold")
    a = ap.parse_args()
    # this script (seeding, verification) runs on the load-generator cores, never on the server's
    # vegeta needs far more than 1,024 sockets; raise this process's limit (children inherit it).
    # Only the load generator gets this: the servers keep Docker's default (soft 1024).
    if resource.getrlimit(resource.RLIMIT_NOFILE)[1] < 1_048_576:
        sh(["sudo", "prlimit", "--pid", str(os.getpid()), "--nofile=1048576:1048576"])
    resource.setrlimit(resource.RLIMIT_NOFILE, (1_048_576, 1_048_576))
    os.sched_setaffinity(0, {c for part in LOADGEN_CPUS.split(",") for c in _cpus(part)})
    if a.cmd == "all":
        run_all(a.system, a.links, a.tag, set(filter(None, a.skip.split(","))), a.reuse)
    elif a.cmd == "up":
        compose(a.system, "up", "-d")
    elif a.cmd == "down":
        compose(a.system, "down")


if __name__ == "__main__":
    sys.exit(main())
