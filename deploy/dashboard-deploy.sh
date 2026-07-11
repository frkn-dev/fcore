#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: $0 v0.x.x}"
ARCH=$(uname -m)

REPO="frkn-dev/fcore"
INSTALL_DIR="/opt/dashboard"
CONFIG_DIR="/etc/dashboard"
DATA_DIR="/var/lib/dashboard"
SERVICE="dashboard"

BIN_URL="https://github.com/${REPO}/releases/download/${VERSION}/dashboard-x86_64"

case "$ARCH" in
    x86_64) ;;
    *) echo "Architecture ${ARCH} not supported by current release assets; build manually." >&2; exit 1 ;;
esac

echo "Installing dashboard ${VERSION}..."

mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$DATA_DIR"

id -u dashboard &>/dev/null || useradd --system --no-create-home dashboard
chown -R dashboard:dashboard "$DATA_DIR"

curl -fsSL -o "${INSTALL_DIR}/${SERVICE}" "$BIN_URL"
chmod +x "${INSTALL_DIR}/${SERVICE}"

cp "src/bin/dashboard/dashboard.service" "/etc/systemd/system/${SERVICE}.service"

systemctl daemon-reload
systemctl enable "$SERVICE"

if [[ -f "${CONFIG_DIR}/config.toml" ]]; then
    echo "Config already exists at ${CONFIG_DIR}/config.toml; skipping."
else
    cp "src/bin/dashboard/dashboard-example.toml" "${CONFIG_DIR}/config.toml"
    echo "Example config copied to ${CONFIG_DIR}/config.toml; edit before starting."
fi

echo "Done. Start with: sudo systemctl start ${SERVICE}"
echo "Nginx example: src/bin/dashboard/docs/nginx.conf"
