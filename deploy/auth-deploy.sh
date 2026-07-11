#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?usage: $0 v0.x.x}"
ARCH=$(uname -m)

REPO="frkn-dev/fcore"
INSTALL_DIR="/usr/local/bin"
CONFIG_DIR="/etc/fcore/auth"
DATA_DIR="/var/lib/fcore/auth"
SERVICE="auth"

BIN_URL="https://github.com/${REPO}/releases/download/${VERSION}/auth-x86_64"

case "$ARCH" in
    x86_64) ;;
    *) echo "Architecture ${ARCH} not supported by current release assets; build manually." >&2; exit 1 ;;
esac

echo "Installing auth ${VERSION}..."

mkdir -p "$CONFIG_DIR" "$DATA_DIR"

id -u fcore &>/dev/null || useradd --system --no-create-home fcore
chown -R fcore:fcore "$DATA_DIR"

curl -fsSL -o "${INSTALL_DIR}/${SERVICE}" "$BIN_URL"
chmod +x "${INSTALL_DIR}/${SERVICE}"

cp "src/bin/auth/auth.service" "/etc/systemd/system/${SERVICE}.service"

systemctl daemon-reload
systemctl enable "$SERVICE"

if [[ -f "${CONFIG_DIR}/config.toml" ]]; then
    echo "Config already exists at ${CONFIG_DIR}/config.toml; skipping."
else
    cp "src/bin/auth/auth-example.toml" "${CONFIG_DIR}/config.toml"
    echo "Example config copied to ${CONFIG_DIR}/config.toml; edit before starting."
fi

echo "Done. Start with: sudo systemctl start ${SERVICE}"
