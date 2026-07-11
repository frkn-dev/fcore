#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: $0 v0.x.x}"
ARCH=$(uname -m)

REPO="frkn-dev/fcore"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/pixel-agent"
DATA_DIR="/var/lib/pixel-agent"
SERVICE="pixel-agent"

BIN_URL="https://github.com/${REPO}/releases/download/${VERSION}/pixel-agent-x86_64"

case "$ARCH" in
    x86_64) ;;
    *) echo "Architecture ${ARCH} not supported by current release assets; build manually." >&2; exit 1 ;;
esac

echo "Installing pixel-agent ${VERSION}..."

mkdir -p "$CONFIG_DIR" "$DATA_DIR"

id -u pixel-agent &>/dev/null || useradd --system --no-create-home pixel-agent
chown -R pixel-agent:pixel-agent "$DATA_DIR"

curl -fsSL -o "${INSTALL_DIR}/${SERVICE}" "$BIN_URL"
chmod +x "${INSTALL_DIR}/${SERVICE}"

curl -fsSL -o "${INSTALL_DIR}/pixel-agent-backfill" "https://github.com/${REPO}/releases/download/${VERSION}/pixel-agent-backfill-x86_64"
chmod +x "${INSTALL_DIR}/pixel-agent-backfill"

cp "src/bin/pixel_agent/pixel-agent.service" "/etc/systemd/system/${SERVICE}.service"
cp "src/bin/pixel_agent/pixel-agent-backfill.service" "/etc/systemd/system/pixel-agent-backfill.service"
cp "src/bin/pixel_agent/pixel-agent-backfill.timer" "/etc/systemd/system/pixel-agent-backfill.timer"

systemctl daemon-reload
systemctl enable "$SERVICE"

if [[ -f "${CONFIG_DIR}/config.toml" ]]; then
    echo "Config already exists at ${CONFIG_DIR}/config.toml; skipping."
else
    cp "src/bin/pixel_agent/pixel-agent-example.toml" "${CONFIG_DIR}/config.toml"
    echo "Example config copied to ${CONFIG_DIR}/config.toml; edit before starting."
fi

if [[ -f "${CONFIG_DIR}/backfill.toml" ]]; then
    echo "Backfill config already exists at ${CONFIG_DIR}/backfill.toml; skipping."
else
    cp "src/bin/pixel_agent/pixel-agent-backfill-example.toml" "${CONFIG_DIR}/backfill.toml"
    echo "Example backfill config copied to ${CONFIG_DIR}/backfill.toml."
fi

echo "Done. Start with: sudo systemctl start ${SERVICE}"
echo "Enable periodic backfill with: sudo systemctl enable --now pixel-agent-backfill.timer"
