#!/usr/bin/env bash
# Install or upgrade hop-rip as a systemd service on a Linux host, from this checkout. Idempotent.
#
#   deploy/install.sh https://short.example.com [127.0.0.1:8080]
#
# Builds the release binary (needs cargo), installs /usr/local/bin/hop, a `hop` system user,
# data in /var/lib/hop, settings in /etc/hop/env, and hop.service. Put a TLS-terminating
# proxy (Caddy, nginx, a platform router) in front of the address it listens on.
set -euo pipefail

base_url=${1:?usage: deploy/install.sh <public base URL> [listen addr]}
addr=${2:-127.0.0.1:8080}
cd "$(dirname "$0")/.."

cargo build --release
sudo install -m 0755 target/release/hop /usr/local/bin/hop.new
sudo mv /usr/local/bin/hop.new /usr/local/bin/hop

id hop >/dev/null 2>&1 || sudo useradd --system --home /var/lib/hop --shell /usr/sbin/nologin hop
sudo install -d -o hop -g hop -m 0750 /var/lib/hop
sudo install -d -m 0755 /etc/hop
printf 'HOP_DATA=/var/lib/hop\nHOP_ADDR=%s\nHOP_BASE_URL=%s\n' "$addr" "$base_url" | sudo tee /etc/hop/env >/dev/null

sudo install -m 0644 deploy/hop.service /etc/systemd/system/hop.service
sudo systemctl daemon-reload
sudo systemctl enable hop.service >/dev/null
sudo systemctl restart hop.service
sleep 1
curl -fsS "http://$addr/healthz" >/dev/null && echo "hop-rip is up on $addr, serving $base_url"
