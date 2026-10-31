# hop.rip 🐇

A tiny URL shortener you can run yourself. One Rust binary, `hop`, is the server, the web app, the
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

Each link is one JSON file at `<data>/links/<aa>/<bb>/<slug>.json`, where `aabb…` is the hex SHA-256
of the slug. That spreads files over 65,536 folders. New links are written to `<data>/tmp` and
hard-linked into place, so two people can't claim the same slug at once; edits are atomic renames.
Back up by copying the folder.

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
