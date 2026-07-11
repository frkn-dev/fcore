#!/bin/bash

set -e

# DO NOT EDIT
# OVERRIDE settings with dot env file corresponding to the env/machine

# Installation settings
MRKTING_VERSION="${MRKTING_VERSION:-v0.5.16-dev}"
INSTALL_DIR="/opt/mrkting"
ARCH=$(uname -m)
MRKTING_URL="https://github.com/frkn-dev/fcore/releases/download/$MRKTING_VERSION/mrkting-$ARCH"
MRKTING_CONFIG_PATH="$INSTALL_DIR/config.toml"

mkdir -p "$INSTALL_DIR"

cd "$INSTALL_DIR"

echo "Installing mrkting version $MRKTING_VERSION..."
echo "$MRKTING_URL"
curl -L -o mrkting "$MRKTING_URL"
chmod +x mrkting

cat <<EOF | tee /etc/systemd/system/mrkting.service
[Unit]
Description=FRKN Marketing Service
After=network.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/mrkting $MRKTING_CONFIG_PATH
Restart=on-failure
RestartSec=5
WorkingDirectory=$INSTALL_DIR

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable mrkting

if [[ -f "$MRKTING_CONFIG_PATH" ]]; then
  echo "File $MRKTING_CONFIG_PATH already exists. skip."
else
    cat <<EOF | tee "$MRKTING_CONFIG_PATH"
[service]
listen = "${LISTEN:-127.0.0.1}"
port = ${PORT:-9103}
log_level = "${LOG_LEVEL:-info}"

[pg]
host = "${PG_HOST:-localhost}"
port = ${PG_PORT:-5432}
db = "${PG_DB:-api}"
username = "${PG_USERNAME:-postgres}"
password = "${PG_PASSWORD:-password}"

[api]
endpoint = "${API_ENDPOINT:-http://127.0.0.1:3000}"
token = "${API_TOKEN:-mysecrettoken}"

[smtp]
server = "${SMTP_SERVER:-smtp.yandex.ru}"
username = "${SMTP_USERNAME:-hehe@hehe.org}"
password = "${SMTP_PASSWORD:-PASSWORD}"
port = ${SMTP_PORT:-587}
from = "${SMTP_FROM:-Privacy Company <hehe@hehe.org>}"
title = "${SMTP_TITLE:-Подписка создана}"
company_name = "${COMPANY_NAME:-Privacy Company}"
support = "${SUPPORT_URL:-https://t.me/hehe_support}"
company_website = "${COMPANY_WEBSITE:-https://example.com}"

[email_encryption]
key = "${EMAIL_ENCRYPTION_KEY:-c29tZV9zZWNyZXRfa2V5X3doaWNoX2lzXzMyYnl0ZXM=}"

[trial]
days = ${TRIAL_DAYS:-3}
limit_bytes = ${TRIAL_LIMIT_BYTES:-10737418240}
enabled_envs = ${TRIAL_ENABLED_ENVS:-["dev", "wl", "ru"]}
enabled_tags = ${TRIAL_ENABLED_TAGS:-["VlessXhttpReality", "VlessTcpReality", "VlessGrpcReality", "Hysteria2", "Mtproto"]}
EOF
fi

systemctl daemon-reload

echo "Installation complete. Use the following commands to start services:"
echo "  sudo systemctl start mrkting"
echo ""
