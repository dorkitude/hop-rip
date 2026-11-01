# Benchmarks

How hop compares with the leading open-source self-hosted URL shorteners on speed and footprint.
Everything here can be rerun with the scripts in [`bench/`](bench/). Raw results are in
[`bench/results/`](bench/results/).

Measured 2026-10-31 on hop 0.3.0 (`889226d`).

## Results

**In short:** hop serves redirects 10–15× faster than the next-best system (Chhoto URL), using a third of
its CPU per request, with the smallest memory footprint and the fastest cold start. Its weak spots are
creates (each one fsyncs its shard file, so tail latency is worse than SQLite's) and disk use for small instances (small shard
files each take a full 4 KB block).

- **Redirects:** hop holds a p99 under 50 ms up to 20,000 redirects/s and peaks at about 89,000/s with
  64 connections. Chhoto peaks at about 5,900/s, Kutt at 1,300/s, YOURLS at 540/s and Shlink at 360/s.
- **Shlink and YOURLS top out in the low hundreds per second** on 2 CPUs. Shlink records every visit in
  MariaDB; with tracking off it gets to 100/s within the latency bar (439/s max). Past their limit both
  stop answering within the 10 s timeout, and Shlink needed a restart before the next step (the harness
  does that and records it as `restarted_before`).
- **Creates:** Chhoto (SQLite) has the best create latency: p99 29 ms at 200/s against hop's 134 ms. At
  full load hop and Chhoto are close (451/s and 406/s); Kutt manages 184/s, Shlink 24/s and YOURLS 21/s.
- **Footprint:** hop idles at 0.4 MiB of process memory and peaks at 37 MiB under load. The PHP and Node
  systems idle at 120–170 MiB, and Shlink and YOURLS (with MariaDB) fill the whole 1 GiB.
- **Disk:** per million links, hop needs 450 MB, less than Chhoto's 691 MB (both measured). At 100,000
  links hop uses the most (211 MB), because its 65,536 shard files are then mostly a link or two each,
  and every file takes at least one 4 KB block. See [disk per million links](#disk-per-million-links).
- **Open-file limit:** at very high open-loop rates hop runs out of file descriptors under the usual
  soft limit of 1,024. With the limit raised (`hop, open-file limit raised`), missing-slug lookups hold
  the latency bar up to 40,000/s instead of 5,000/s. Since 0.3.2, hop raises its own soft limit to the hard
  limit at startup, so it gets the raised-limit numbers by default. See [caveats](#caveats).

| | hop 0.3.0 | Chhoto URL 7.10.5 | Shlink 5.1.8 | YOURLS 1.10.6 | Kutt 3.2.6 | Shlink, tracking off | hop, open-file limit raised |
|---|---|---|---|---|---|---|---|
| Redirects: max rate with p99 < 50 ms | 20,000/s | 2,000/s | 0/s | 200/s | 1,000/s | 100/s | 20,000/s |
| Redirects: max throughput (64 conns) | 89,187/s | 5,897/s | 361/s | 540/s | 1,313/s | 439/s | 44,637/s |
| Redirect latency at 1k/s, p50 / p99 / p99.9 (ms) | 0.20 / 4.63 / 37 | 0.20 / 8.23 / 53 | - | 7311 / 10022 / 10046 | 0.71 / 32 / 98 | - | 0.20 / 2.70 / 16 |
| Missing slugs: max rate with p99 < 50 ms | 5,000/s | 1,000/s | 0/s | 500/s | 200/s | 200/s | 40,000/s |
| Creates: max rate with p99 < 50 ms | 0/s | 200/s | 0/s | 0/s | 0/s | - | - |
| Creates: max throughput (64 conns) | 451/s | 406/s | 24/s | 21/s | 184/s | - | - |
| 95/5 mix: max rate with p99 < 50 ms | 1,000/s | 1,000/s | 0/s | 0/s | 200/s | - | - |
| Cold-cache redirects at 1k/s, p99 (ms) | 3.73 | 398 | n/a (under 1k/s warm) | n/a (under 1k/s warm) | 171 | n/a (under 1k/s warm) | - |
| CPU-seconds per 10k redirects (at 1k/s) | 1.10 | 3.33 | - | 32.97 | 6.67 | - | 1.14 |
| CPU-seconds per 10k creates (at 200/s) | 4.52 | 7.85 | - | - | 33.04 | - | - |
| Install size (images, MB) | 42.8 | 18.1 | 920.0 | 1214.0 | 491.0 | 920.0 | 42.8 |
| Idle memory, anon (MiB) | 0.4 | 2.5 | 170.9 | 123.0 | 147.3 | 168.4 | 0.4 |
| Peak memory under load, anon (MiB) | 37.2 | 115.4 | 891.1 | 612.4 | 307.7 | 830.7 | 495.4 |
| Peak memory incl. page cache (MiB) | 383.2 | 557.1 | 1024.0 | 1024.0 | 642.0 | 1023.5 | 837.9 |
| Disk after seeding (MB, net of empty) | 211.2 | 75.1 | 54.4 | 67.0 | 30.3 | - | 211.2 |
| Inodes after seeding (net of empty) | 51,568 | 0 | 0 | 0 | 0 | - | 51,568 |
| Seeding rate via API (8 conns) | 432/s | 612/s | 21/s | 38/s | 158/s | - | 804/s |
| Cold start to first redirect (s) | 0.30 | 0.69 | 3.04 | 3.49 | 2.49 | 4.15 | 0.28 |

### Disk per million links

| | hop 0.3.1 | Chhoto URL | Kutt | YOURLS | Shlink |
| --- | --- | --- | --- | --- | --- |
| MB per 1M links | 450 | 691 | ≈303 | ≈670 | ≈544 |
| How | measured at 1M | measured at 1M | 100k × 10 | 100k × 10 | 100k × 10 |

hop and Chhoto were seeded with a million links. Seeding Kutt, YOURLS and Shlink to a million through
their APIs would take 2 to 13 hours each (158, 38 and 21 links/s), so their figures scale the 100k
measurement linearly, which suits row storage in a database. hop's own size isn't linear: 100,000
links take 211 MB because the shard files are nearly empty, and the blocks fill up as links are added.
hop's figure is from 0.3.1, which writes shards as compact JSON (0.3.0's indented JSON took 480 MB; the
same data is 301 MB before block rounding). Raw numbers: `bench/results/hop-disk-0.3.1.json`.

### Redirects of random existing slugs

Each cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).

| rate | hop 0.3.0 | Chhoto URL 7.10.5 | Shlink 5.1.8 | YOURLS 1.10.6 | Kutt 3.2.6 | Shlink, tracking off | hop, open-file limit raised |
|---|---|---|---|---|---|---|---|
| 100/s |  |  | 100/s: 40 / 1119 / 1202 | 100/s: 4.84 / 49 / 58 |  | 100/s: 3.77 / 45 / 181 |  |
| 200/s |  |  | 0/s: 10001 / 10050 / 10058, **100.0% err** | 200/s: 3.70 / 41 / 61 |  | 200/s: 3.65 / 51 / 292 |  |
| 500/s |  |  |  | 499/s: 8.58 / 224 / 3938 |  | 407/s: 1930 / 3344 / 3399 |  |
| 1,000/s | 1,000/s: 0.20 / 4.63 / 37 | 1,000/s: 0.20 / 8.23 / 53 |  | 403/s: 7311 / 10022 / 10046, **32.8% err** | 1,000/s: 0.71 / 32 / 98 |  | 1,000/s: 0.20 / 2.70 / 16 |
| 2,000/s | 2,000/s: 0.19 / 2.30 / 4.09 | 2,000/s: 0.21 / 19 / 34 |  |  | 1,068/s: 4928 / 10002 / 10030, **11.0% err** |  | 2,000/s: 0.20 / 4.27 / 19 |
| 5,000/s | 5,000/s: 0.18 / 2.21 / 7.87 | 3,502/s: 0.22 / 5777 / 8227 |  |  |  |  | 4,999/s: 0.24 / 28 / 47 |
| 10,000/s | 10,000/s: 0.15 / 9.37 / 55 |  |  |  |  |  | 9,999/s: 0.17 / 26 / 51 |
| 20,000/s | 20,000/s: 0.08 / 1.75 / 13 |  |  |  |  |  | 19,998/s: 0.33 / 30 / 50 |
| 40,000/s | 9,381/s: 0.22 / 85 / 122, **71.7% err** |  |  |  |  |  | 14,749/s: 190 / 2372 / 2499 |
| max (64 conns) | 89,187/s: 0.39 / 2.51 / 24 | 5,897/s: 2.63 / 96 / 385 | 361/s: 149 / 401 / 456 | 540/s: 84 / 734 / 1268 | 1,313/s: 40 / 90 / 316 | 439/s: 128 / 394 / 468 | 44,637/s: 0.42 / 42 / 59 |

### Missing slugs

Each cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).

| rate | hop 0.3.0 | Chhoto URL 7.10.5 | Shlink 5.1.8 | YOURLS 1.10.6 | Kutt 3.2.6 | Shlink, tracking off | hop, open-file limit raised |
|---|---|---|---|---|---|---|---|
| 100/s |  |  | 100/s: 6.75 / 217 / 291 | 100/s: 2.71 / 35 / 45 | 100/s: 1.77 / 40 / 85 | 100/s: 3.29 / 18 / 174 |  |
| 200/s |  |  | 200/s: 6.44 / 177 / 367 | 200/s: 2.03 / 39 / 51 | 200/s: 1.44 / 33 / 39 | 200/s: 2.20 / 44 / 187 |  |
| 500/s |  |  | 330/s: 3270 / 7640 / 7722 | 500/s: 2.64 / 48 / 103 | 500/s: 7.64 / 58 / 95 | 374/s: 2929 / 5073 / 5109 |  |
| 1,000/s | 1,000/s: 0.20 / 2.31 / 3.97 | 997/s: 41 / 49 / 51 |  | 528/s: 1450 / 10002 / 10005, **11.7% err** | 864/s: 331 / 10000 / 10002, **1.5% err** |  | 1,000/s: 0.21 / 23 / 38 |
| 5,000/s | 4,999/s: 0.24 / 29 / 48 | 4,979/s: 41 / 69 / 110 |  |  |  |  | 5,000/s: 0.16 / 2.10 / 4.15 |
| 10,000/s | 9,959/s: 2.47 / 55 / 74 | 2,647/s: 4496 / 10884 / 13401, **17.9% err** |  |  |  |  | 10,000/s: 0.13 / 1.66 / 3.79 |
| 20,000/s | 1,438/s: 300 / 798 / 899, **91.4% err** |  |  |  |  |  | 19,998/s: 0.12 / 12 / 32 |
| 40,000/s |  |  |  |  |  |  | 39,961/s: 1.73 / 42 / 64 |
| max (64 conns) | 39,299/s: 0.42 / 50 / 64 | 2,490/s: 8.21 / 90 / 98 | 342/s: 164 / 406 / 444 | 745/s: 55 / 578 / 1124 | 927/s: 82 / 111 / 202 | 340/s: 181 / 440 / 604 | 63,622/s: 0.41 / 3.45 / 42 |

### 95% redirects / 5% creates

Each cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).

| rate | hop 0.3.0 | Chhoto URL 7.10.5 | Shlink 5.1.8 | YOURLS 1.10.6 | Kutt 3.2.6 | Shlink, tracking off | hop, open-file limit raised |
|---|---|---|---|---|---|---|---|
| 100/s |  |  | 100/s: 6.47 / 220 / 624 | 69/s: 116 / 10001 / 10001, **4.0% err** | 100/s: 3.89 / 67 / 76 |  |  |
| 200/s |  |  | 188/s: 550 / 972 / 1098 |  | 200/s: 3.30 / 49 / 65 |  |  |
| 500/s | 500/s: 0.44 / 49 / 102 | 500/s: 0.23 / 6.94 / 55 | 0/s: 10001 / 10005 / 10014, **100.0% err** |  | 498/s: 15 / 410 / 551 |  |  |
| 1,000/s | 1,000/s: 0.85 / 48 / 63 | 1,000/s: 0.21 / 17 / 130 |  |  | 559/s: 4417 / 10001 / 10041, **7.0% err** |  |  |
| 2,000/s | 1,999/s: 1.86 / 61 / 120 | 2,000/s: 0.20 / 78 / 366 |  |  |  |  |  |
| 5,000/s | 3,012/s: 1.92 / 222 / 10001, **0.6% err** | 3,745/s: 1846 / 10000 / 10001, **1.4% err** |  |  |  |  |  |

### Creates

Each cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).

| rate | hop 0.3.0 | Chhoto URL 7.10.5 | Shlink 5.1.8 | YOURLS 1.10.6 | Kutt 3.2.6 | Shlink, tracking off | hop, open-file limit raised |
|---|---|---|---|---|---|---|---|
| 100/s | 100/s: 3.16 / 78 / 150 | 100/s: 2.71 / 14 / 28 | 0/s: 10001 / 10004 / 10009, **100.0% err** | 3/s: 10001 / 10039 / 10040, **94.4% err** | 91/s: 381 / 6038 / 6541 |  |  |
| 200/s | 200/s: 5.51 / 134 / 160 | 200/s: 2.48 / 29 / 55 |  |  | 102/s: 2252 / 10006 / 10028, **19.2% err** |  |  |
| 500/s | 493/s: 4.92 / 716 / 818 | 324/s: 555 / 9614 / 9849 |  |  |  |  |  |
| 1,000/s | 198/s: 0.81 / 2161 / 10001, **80.2% err** |  |  |  |  |  |  |
| max (64 conns) | 451/s: 125 / 316 / 408 | 406/s: 127 / 480 / 501 | 24/s: 2545 / 3611 / 3725 | 21/s: 2475 / 6215 / 7823 | 184/s: 321 / 1548 / 3998 |  |  |


## What was compared

| System | Version | Stack | Storage used here |
| --- | --- | --- | --- |
| **hop** | 0.3.0 | Rust (axum/tokio), one binary | 65,536 JSON shard files (`--data`) |
| [Chhoto URL](https://github.com/SinTan1729/chhoto-url) | 7.10.5 | Rust (actix-web) | SQLite, WAL mode, `synchronous=FULL` (its documented recommendation) |
| [Shlink](https://shlink.io) | 5.1.8 | PHP 8.5 on RoadRunner | MariaDB 11.4 (Shlink's docs call SQLite "testing only") |
| [YOURLS](https://yourls.org) | 1.10.6 | PHP 8.5, Apache mod_php (official image) | MariaDB 11.4 |
| [Kutt](https://github.com/thedevs-network/kutt) | 3.2.6 | Node 22 (Express) | SQLite via better-sqlite3 (its default compose file) |

Each runs from its official Docker image with its documented settings. hop runs from an image built
from this checkout ([`bench/hop/Dockerfile`](bench/hop/Dockerfile): the release binary on
`distroless/cc-debian12`). The compose files are in [`bench/compose/`](bench/compose/).

Every system runs as shipped, including features hop doesn't have. Shlink records every visit in its
database, YOURLS logs every click, Kutt queues a visit record, and Chhoto increments a hit counter.
hop does none of that, so a redirect is less work for it. To measure how much that matters, Shlink
was also run with `DISABLE_TRACKING=true`.

## Setup

- **Hardware:** one exe.dev VM: 8 vCPUs on an AMD EPYC 9354P, 32 GB RAM, 100 GB ext4 on a virtio
  disk. Ubuntu 24.04, kernel 6.12, Docker 29.1 (cgroup v2, systemd driver).
- **One machine, split in two.** The plan was a separate load-generator VM. exe.dev VMs have no
  network path to each other: each sits alone behind NAT (both report `10.42.0.42`), and the only
  way in is exe.dev's authenticated HTTPS proxy, which adds a proxy hop and TLS. So the server and the
  load generator share the VM, pinned to separate cores:
  - the **system under test** runs in a systemd slice, `bench.slice`, on cores 0-1 with
    `CPUQuota=200%`, `MemoryMax=1G` and no swap. All of a stack's containers join that slice
    (`cgroup_parent`), so **an app and its database share the same 2 CPUs and 1 GiB**. Each
    container also has `--cpus=2 --memory=1g`. hop runs in Docker like the others.
  - **vegeta** and the harness run on cores 2-7 (`taskset`).
  - All containers use host networking, so no docker-proxy or NAT sits in the request path.
- **Data:** every system is seeded through its own create API with the same 100,000 links: random
  7-character slugs (`[a-z0-9]`) and unique long URLs (about 130 characters, six hosts). Seeding uses
  8 concurrent connections. A sample of 300 redirects and 50 missing slugs is then checked against the
  expected targets. Every system passed.
- After seeding, the harness runs `sync` and drops the page cache. Measurement then starts like a
  restarted server with existing data, not straight after a 100,000-write burst. (Why that matters
  for hop: see [caveats](#caveats).)

## Method

- **Load tool:** [vegeta](https://github.com/tsenart/vegeta) 12.12.0. It runs at **fixed request
  rates** (open loop), so a slow server can't slow the client down and hide its own latency
  (coordinated omission). It doesn't follow redirects; a 3xx counts as the answer.
- **Workloads:**
  - **redirect:** `GET /<slug>` over the 100,000 seeded slugs, shuffled
  - **missing:** `GET` on 20,000 slugs that don't exist
  - **mix:** 95% redirects, 5% creates, randomly interleaved
  - **create:** API creates with fresh custom slugs and fresh URLs
- **Rates:** each workload climbs a fixed ladder (redirects: 100, 200, 500, 1k, 2k, 5k, 10k, 20k,
  40k/s; creates: 100, 200, 500, 1k, 2k, 5k, 10k/s) and stops after the first step that **saturates**:
  more than 1% errors, or under 90% of the target rate achieved. Then one **max-throughput** run sends
  closed-loop load with 64 concurrent requests (not for the mix). Before each step the harness checks
  that the server still answers, and restarts the stack if an overload left it hung.
  The hop, Chhoto and Kutt redirect runs predate the 100-500/s steps, which they all pass.
- **Each point is 3 runs of 15 seconds; tables show the median.** The harness pauses 2 s between runs.
- **Success** means the system's normal answer: its redirect status for existing slugs (hop and Shlink
  `302`, Chhoto `307`, YOURLS `301`, Kutt `302`), its not-found answer for missing slugs (hop, Chhoto
  and Shlink `404`; YOURLS and Kutt redirect to a page, `302`), and `200/201` for creates. Anything
  else, including timeouts (10 s), counts as an error.
- **"Max rate with p99 < 50 ms"** is the highest fixed rate on the ladder with a p99 under 50 ms and
  no saturation. The ladder is coarse, so the real limit is somewhere between that step and the next.
- **Cold cache:** before each of 3 runs, `sync; echo 3 > /proc/sys/vm/drop_caches`, then redirects at
  1k/s. This empties the OS page cache only. Databases keep their own buffer pools (MariaDB's
  InnoDB pool, SQLite's page cache inside the process).
- **Footprint:**
  - *Install size:* the unpacked size of the images in the stack (`docker image ls`), database
    image included.
  - *Memory:* the slice's cgroup counters. **anon** is process memory (heap, stacks; roughly RSS
    without file mappings). **total** also counts the page cache and kernel memory charged to the
    stack. Idle is measured 15 s after a cold start; peaks are sampled every 250 ms during the load runs.
  - *CPU:* the slice's `cpu.stat` usage during the 1k/s redirect runs and the 200/s create runs,
    per 10,000 requests.
  - *Disk and inodes:* `du` of the data directory (for MariaDB stacks, the database volume) after
    seeding, minus the same right after setup.
  - *Cold start:* `docker compose down`, then `up -d`, until `GET /<seeded slug>` returns a
    redirect (polled every 10 ms). It includes container creation and, for Shlink and YOURLS, MariaDB
    startup. Median of 3.

## 1,000,000 links

hop and Chhoto URL were also seeded with a million links (redirect and create workloads only).

| | hop 0.3.0 | Chhoto URL 7.10.5 |
|---|---|---|
| Redirects: max rate with p99 < 50 ms | 2,000/s | 2,000/s |
| Redirects: max throughput (64 conns) | 51,070/s | 4,116/s |
| Redirect latency at 1k/s, p50 / p99 / p99.9 (ms) | 0.21 / 6.56 / 33 | 0.22 / 2.46 / 4.45 |
| Missing slugs: max rate with p99 < 50 ms | - | - |
| Creates: max rate with p99 < 50 ms | 100/s | 200/s |
| Creates: max throughput (64 conns) | 574/s | 763/s |
| 95/5 mix: max rate with p99 < 50 ms | - | - |
| Cold-cache redirects at 1k/s, p99 (ms) | 5.14 | 12 |
| CPU-seconds per 10k redirects (at 1k/s) | 1.25 | 3.29 |
| CPU-seconds per 10k creates (at 200/s) | 4.75 | 8.63 |
| Install size (images, MB) | 42.8 | 18.1 |
| Idle memory, anon (MiB) | 0.4 | 3.8 |
| Peak memory under load, anon (MiB) | 32.7 | 41.7 |
| Peak memory incl. page cache (MiB) | 658.3 | 1024.0 |
| Disk after seeding (MB, net of empty) | 480.4 | 690.9 |
| Inodes after seeding (net of empty) | 65,792 | 0 |
| Seeding rate via API (8 conns) | 698/s | 601/s |
| Cold start to first redirect (s) | 0.41 | 3.79 |

### Redirects of random existing slugs

Each cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).

| rate | hop 0.3.0 | Chhoto URL 7.10.5 |
|---|---|---|
| 1,000/s | 1,000/s: 0.21 / 6.56 / 33 | 1,000/s: 0.22 / 2.46 / 4.45 |
| 2,000/s | 2,000/s: 0.24 / 23 / 37 | 2,000/s: 0.19 / 2.11 / 4.67 |
| 5,000/s | 5,000/s: 0.60 / 74 / 99 | 4,526/s: 0.22 / 5986 / 10000, **0.1% err** |
| 10,000/s | 4,461/s: 1.62 / 186 / 212, **41.2% err** | 2,576/s: 6845 / 10391 / 11999, **21.2% err** |
| max (64 conns) | 51,070/s: 0.88 / 12 / 40 | 4,116/s: 3.01 / 128 / 564 |

### Creates

Each cell: achieved rate: p50 / p99 / p99.9 latency in ms (median of 3 runs).

| rate | hop 0.3.0 | Chhoto URL 7.10.5 |
|---|---|---|
| 100/s | 100/s: 3.22 / 16 / 67 | 100/s: 3.25 / 35 / 60 |
| 200/s | 200/s: 2.55 / 104 / 164 | 200/s: 2.33 / 40 / 74 |
| 500/s | 500/s: 17 / 550 / 573 | 500/s: 7.70 / 318 / 417 |
| 1,000/s | 369/s: 1.03 / 1736 / 10001, **63.1% err** | 655/s: 2611 / 10001 / 10004, **6.8% err** |
| max (64 conns) | 574/s: 94 / 312 / 369 | 763/s: 75 / 206 / 296 |

At a million links hop's p99 crosses 50 ms above 2,000 redirects/s at fixed rates, while its closed-loop
maximum is still 51,000/s (Chhoto: 4,100/s). Chhoto's database grows to 691 MB against hop's 480 MB
(449.5 MB with 0.3.1's compact JSON).

## Caveats

- **One machine, loopback.** The load generator and the server share one VM on separate cores, so there is
  no real network in the path. Absolute numbers will be lower behind a proxy and TLS; the ratios are the
  point.
- **Features differ.** Shlink, YOURLS and Kutt record visits, and Chhoto counts hits; hop does neither.
  The Shlink tracking-off run shows that tracking is part of the gap, not all of it.
- **Open files.** The measured hop 0.3.0 kept the default soft limit of 1,024 file descriptors (Docker's
  and systemd's default). Each connection and each shard read uses one, so open-loop overload turned into
  errors sooner than with a higher limit, as the variant run shows. hop 0.3.2 raises its soft limit to the
  hard limit at startup, and `deploy/hop.service` sets `LimitNOFILE=65536:524288`.
- **Write bursts.** Straight after 100,000 creates the kernel is still writing back tens of thousands
  of dirty shard files, which slows reads for a while. That's why the harness syncs and drops the page
  cache before measuring: the numbers describe a server restarted with existing data.
- **Cold cache** is only measured at 1,000 redirects/s, so it reads "n/a" for systems that can't hold that
  rate when warm.
- **Ladders are coarse.** "Max rate with p99 < 50 ms" is a step on the ladder; the real limit lies
  between that step and the next.
- **Reruns.** Shlink, YOURLS and Kutt's missing-slug and mix workloads were rerun on their seeded data after
  the ladders gained 100, 200 and 500/s steps; each result file lists what was rerun under `reruns`.


## Rerun it

On a Linux host with Docker (cgroup v2, systemd cgroup driver) and sudo:

```bash
bench/setup-host.sh                 # vegeta, uv, bench.slice, sysctls, builds the hop image
bench/bench.py all hop              # one system: seeds, runs everything, writes bench/results/hop.json
bench/run-all.sh                    # everything in this file (several hours)
bench/report.py                     # the tables above, from bench/results/
```

Useful knobs: `BENCH_DURATION` (seconds per run, default 15), `BENCH_RUNS` (default 3),
`LOADGEN_CPUS` (default `2-7`), `--links N`. Secrets for the stacks are generated into `bench/.env`
(git-ignored).
