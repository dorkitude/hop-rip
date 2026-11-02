#!/usr/bin/env bash
# Prepare a Linux host (Ubuntu 24.04, cgroup v2, Docker with the systemd cgroup driver) to run the
# hop-rip benchmark. Idempotent; needs sudo.
#
#   bench/setup-host.sh
#
# - installs vegeta (the load generator) and uv (runs bench.py)
# - creates bench.slice: every container of the system under test runs inside it, so the whole stack
#   (app + its database) shares 2 CPUs (cores 0-1) and 1 GiB of RAM, with no swap
# - raises socket and file limits so the load generator isn't the bottleneck
# - builds the hop-rip image from this checkout
set -euo pipefail
cd "$(dirname "$0")/.."

VEGETA_VERSION=12.12.0
SERVER_CPUS=${SERVER_CPUS:-0-1}

if ! command -v vegeta >/dev/null || ! vegeta -version | grep -q "v$VEGETA_VERSION"; then
  tmp=$(mktemp -d)
  curl -fsSL -o "$tmp/vegeta.tgz" \
    "https://github.com/tsenart/vegeta/releases/download/v$VEGETA_VERSION/vegeta_${VEGETA_VERSION}_linux_amd64.tar.gz"
  tar xzf "$tmp/vegeta.tgz" -C "$tmp" vegeta
  sudo install -m 0755 "$tmp/vegeta" /usr/local/bin/vegeta
  rm -rf "$tmp"
fi
command -v uv >/dev/null || curl -LsSf https://astral.sh/uv/install.sh | sh

# The slice the system under test lives in. Docker containers join it with cgroup_parent: bench.slice.
sudo tee /etc/systemd/system/bench.slice >/dev/null <<SLICE
[Unit]
Description=hop-rip benchmark: system under test

[Slice]
CPUAccounting=yes
MemoryAccounting=yes
CPUQuota=200%
AllowedCPUs=$SERVER_CPUS
MemoryMax=1G
MemorySwapMax=0
SLICE
sudo systemctl daemon-reload
sudo systemctl start bench.slice

sudo sysctl -q -w net.core.somaxconn=65535 net.ipv4.tcp_max_syn_backlog=65535 \
  net.ipv4.ip_local_port_range="1024 65000" net.ipv4.tcp_tw_reuse=1 fs.file-max=2097152

sudo mkdir -p /srv/bench && sudo chown "$(id -u):$(id -g)" /srv/bench
docker build -q -t hop-rip:bench -f bench/hop/Dockerfile . >/dev/null
echo "host ready: vegeta $(vegeta -version | head -1), bench.slice on CPUs $SERVER_CPUS, hop-rip:bench built"
