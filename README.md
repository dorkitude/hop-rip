# hop.rip 🐇

A tiny URL shortener you can run yourself. hop-rip is one Rust binary, `hop`, that is the server, the web app, the
API and the CLI. Links live as plain files in sharded folders, so there's no database to run.

The public instance is **https://hop.rip**.

## No accounts

Anyone can make a link. Creating one returns an **edit key**, and that key is the only way to
change or delete that link. The server stores only a SHA-256 hash of it. The web app remembers
keys in your browser, and the CLI saves them in `~/.config/hop/keys.json` (mode 600).

## CLI

```bash
cargo install --git https://github.com/dorkitude/hop-rip

hop new https://example.com/some/long/path            # prints https://hop.rip/x7k2m
hop new https://example.com/ --slug bowling           # custom name
hop get bowling                                       # where does it go?
hop edit bowling https://example.com/elsewhere        # uses the saved key, or --key
hop rm bowling
```

Point the CLI at another server with `--server https://short.example.com` or `HOP_SERVER`.

## API

| Method | Path | Body | Result |
| --- | --- | --- | --- |
| `POST` | `/api/links` | `{"url": "...", "slug": "optional"}` | `201` with `slug`, `url`, `short_url`, `edit_key` |
| `GET` | `/api/links/:slug` | | `200` with `slug`, `url`, `short_url`, `created`, `updated` |
| `PUT` | `/api/links/:slug` | `{"url": "..."}` | `200`; needs the key |
| `DELETE` | `/api/links/:slug` | | `204`; needs the key |
| `GET` | `/:slug` | | `302` to the long URL |
| `GET` | `/healthz` | | `ok` |

Send the key as `Authorization: Bearer <edit_key>` (or `X-Edit-Key: <edit_key>`). Errors are
`{"error": "..."}` with `400`, `401` (no key), `403` (wrong key), `404` or `409` (slug taken).

```bash
curl -s https://hop.rip/api/links -H 'content-type: application/json' -d '{"url":"https://example.com/"}'
```

Slugs are lowercase `a-z`, `0-9`, `-` and `_`, up to 64 characters; uppercase input is lowercased.
Random slugs are 5 characters from an alphabet without look-alikes (`l`, `o`, `0`, `1`). URLs must
be `http` or `https`.

## Storage

Links are packed into 65,536 shard files at `<data>/shards/<aa>/<bb>.json`, where `aabb` starts the
hex SHA-256 of the slug. Each shard is a plain JSON object of slug to link, so you can `cat` and
`grep` it, and a 4 KB disk block holds a dozen or more links. A million links take about 65,000
files and a few hundred MB, instead of a million files.

Writes lock their shard in memory, rewrite it to `<data>/tmp` and rename it into place; redirects
read without any lock, because the rename is atomic. One `hop` process owns a data folder (it holds an
exclusive lock on `<data>/hop.lock`), so run one server per folder. Back up by copying the folder.

Data from 0.1 and 0.2 (one file per link under `<data>/links/`) moves into shards the next time
the server starts.

## Benchmarks

Against four popular self-hosted shorteners, with 100,000 links each and every system held to 2 CPUs and 1 GiB:

| | hop-rip | Chhoto URL | Kutt | YOURLS | Shlink |
| --- | --- | --- | --- | --- | --- |
| Max redirects/s | **89,187** | 5,897 | 1,313 | 540 | 361 |
| Max creates/s | **451** | 406 | 184 | 21 | 24 |
| Idle memory (MiB) | **0.4** | 2.5 | 147 | 123 | 171 |
| Cold start (s) | **0.30** | 0.69 | 2.49 | 3.49 | 3.04 |
| Disk per 1M links (MB) | 450 | 691 | **≈303** | ≈670 | ≈544 |

≈ scales the 100k measurement. hop-rip's trade-offs are slower creates at the tail (each one fsyncs its
shard) and more disk on small instances, where its shard files are nearly empty. The method,
latency ladders, a million-link run and caveats are in [BENCHMARKS.md](BENCHMARKS.md), and the harness
to rerun it all is in [`bench/`](bench/).

## Run your own

```bash
hop serve --data ./data --addr 127.0.0.1:8080 --base-url https://short.example.com
```

Settings also come from `HOP_DATA`, `HOP_ADDR` and `HOP_BASE_URL`. Without a base URL, short links
use the request's `Host` header. Put a TLS proxy in front.

On a Linux host with systemd, from a checkout:

```bash
deploy/install.sh https://short.example.com          # builds, installs hop.service, starts it
```

Example Caddyfile:

```
short.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

## Develop

```bash
cargo test
cargo run -- serve --base-url http://localhost:8080
```

MIT licensed.
