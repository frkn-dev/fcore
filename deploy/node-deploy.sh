#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: $0 v0.x.x}"
ARCH=$(uname -m)

REPO="frkn-dev/fcore"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/fcore/node"
DATA_DIR="/var/lib/fcore/node"
SERVICE="node"

BIN_NAME=""
case "$ARCH" in
    x86_64) BIN_NAME="node-x86_64" ;;
    aarch64) BIN_NAME="node-aarch64" ;;
    armv7l) BIN_NAME="node-armv7" ;;
    *) echo "Architecture ${ARCH} not supported by current release assets; build manually." >&2; exit 1 ;;
esac

BIN_URL="https://github.com/${REPO}/releases/download/${VERSION}/${BIN_NAME}"

echo "Installing node ${VERSION} (${BIN_NAME})..."

mkdir -p "$CONFIG_DIR" "$DATA_DIR"

# Node manages network interfaces, so it runs as root.
curl -fsSL -o "${INSTALL_DIR}/${SERVICE}" "$BIN_URL"
chmod +x "${INSTALL_DIR}/${SERVICE}"

cp "src/bin/node/node.service" "/etc/systemd/system/${SERVICE}.service"

systemctl daemon-reload
systemctl enable "$SERVICE"

if [[ -f "${CONFIG_DIR}/config.toml" ]]; then
    echo "Config already exists at ${CONFIG_DIR}/config.toml; skipping."
else
    cp "src/bin/node/node-example.toml" "${CONFIG_DIR}/config.toml"
    echo "Example config copied to ${CONFIG_DIR}/config.toml; edit before starting."
fi

echo "Done. Start with: sudo systemctl start ${SERVICE}"
