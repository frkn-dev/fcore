#!/usr/bin/env bash
set -euo pipefail

# Build and deploy the pixel-agent binary to a remote server.
# Usage: ./deploy-pixel-agent.sh user@host

HOST="${1:?usage: $0 user@host}"
REMOTE_DIR="/usr/local/bin"
CONFIG_DIR="/etc/pixel-agent"
DATA_DIR="/var/lib/pixel-agent"

echo "Building release binary..."
cargo build --release --bin pixel-agent

echo "Uploading binary..."
scp target/release/pixel-agent "${HOST}:${REMOTE_DIR}/pixel-agent.new"
ssh "${HOST}" "mv ${REMOTE_DIR}/pixel-agent.new ${REMOTE_DIR}/pixel-agent && chmod +x ${REMOTE_DIR}/pixel-agent"

echo "Ensuring directories and user..."
ssh "${HOST}" "id -u pixel-agent &>/dev/null || useradd --system --no-create-home pixel-agent"
ssh "${HOST}" "mkdir -p ${CONFIG_DIR} ${DATA_DIR} && chown pixel-agent:pixel-agent ${DATA_DIR}"

echo "Uploading config if missing..."
ssh "${HOST}" "test -f ${CONFIG_DIR}/config.toml || echo 'Config not present, copy config-pixel-agent-example.toml manually'"

echo "Uploading systemd unit..."
scp dev/pixel-agent.service "${HOST}:/etc/systemd/system/pixel-agent.service"

echo "Reloading and restarting..."
ssh "${HOST}" "systemctl daemon-reload && systemctl enable --now pixel-agent && systemctl status pixel-agent --no-pager"

echo "Done."
