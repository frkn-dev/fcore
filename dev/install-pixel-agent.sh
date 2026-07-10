#!/bin/bash

set -e

# DO NOT EDIT
# OVERRIDE settings with dot env file corresponding to the env/machine

# Installation settings
PIXEL_AGENT_VERSION="${PIXEL_AGENT_VERSION:-v0.5.15}"
INSTALL_DIR="/opt/pixel-agent"
ARCH=$(uname -m)
PIXEL_AGENT_URL="https://github.com/frkn-dev/fcore/releases/download/$PIXEL_AGENT_VERSION/pixel-agent-$ARCH"
PIXEL_AGENT_CONFIG_PATH="$INSTALL_DIR/config.toml"

# Agent settings
LOG_LEVEL="${LOG_LEVEL:-info}"
LOG_PATH="${LOG_PATH:-/var/log/nginx/pixel.log}"
GEOIP_DB="${GEOIP_DB:-/usr/share/GeoIP/GeoLite2-Country.mmdb}"
ADMIN_LISTEN="${ADMIN_LISTEN:-0.0.0.0}"
ADMIN_PORT="${ADMIN_PORT:-9102}"
POLL_INTERVAL_SEC="${POLL_INTERVAL_SEC:-30}"
FLUSH_INTERVAL_SEC="${FLUSH_INTERVAL_SEC:-300}"
SNAPSHOT_INTERVAL_SEC="${SNAPSHOT_INTERVAL_SEC:-300}"
BUCKET_MINUTES="${BUCKET_MINUTES:-5}"
RETENTION_HOURS="${RETENTION_HOURS:-168}"
MAX_POINTS="${MAX_POINTS:-10000}"
RETENTION_SECONDS="${RETENTION_SECONDS:-604800}"

mkdir -p "$INSTALL_DIR"

cd "$INSTALL_DIR"

echo "Installing pixel-agent version $PIXEL_AGENT_VERSION..."
echo "$PIXEL_AGENT_URL"
curl -L -o pixel-agent "$PIXEL_AGENT_URL"
chmod +x pixel-agent

cat <<EOF | tee /etc/systemd/system/pixel-agent.service
[Unit]
Description=FRKN Pixel Analytics Agent
After=network.target

[Service]
Type=simple
User=pixel-agent
Group=pixel-agent
ExecStart=$INSTALL_DIR/pixel-agent $PIXEL_AGENT_CONFIG_PATH
Restart=on-failure
RestartSec=5
WorkingDirectory=$INSTALL_DIR

[Install]
WantedBy=multi-user.target
EOF

# Create user and data directories if they don't exist
id -u pixel-agent &>/dev/null || useradd --system --no-create-home pixel-agent
mkdir -p /var/lib/pixel-agent
chown pixel-agent:pixel-agent /var/lib/pixel-agent

systemctl daemon-reload
systemctl enable pixel-agent

if [[ -f "$PIXEL_AGENT_CONFIG_PATH" ]]; then
  echo "File $PIXEL_AGENT_CONFIG_PATH already exists. Skip."
else
  cat <<EOF | tee "$PIXEL_AGENT_CONFIG_PATH"
log_level = "$LOG_LEVEL"
log_path = "$LOG_PATH"
offset_path = "/var/lib/pixel-agent/offset.dat"
snapshot_path = "/var/lib/pixel-agent/metrics.snapshot"
geoip_db = "$GEOIP_DB"
admin_listen = "$ADMIN_LISTEN"
admin_port = $ADMIN_PORT
poll_interval_sec = $POLL_INTERVAL_SEC
flush_interval_sec = $FLUSH_INTERVAL_SEC
snapshot_interval_sec = $SNAPSHOT_INTERVAL_SEC
bucket_minutes = $BUCKET_MINUTES
retention_hours = $RETENTION_HOURS
max_points = $MAX_POINTS
retention_seconds = $RETENTION_SECONDS
EOF
  chown pixel-agent:pixel-agent "$PIXEL_AGENT_CONFIG_PATH"
fi

systemctl daemon-reload

echo "Installation complete. Use the following commands to start the service:"
echo "  sudo systemctl start pixel-agent"
echo "  sudo systemctl status pixel-agent"
echo ""
echo "Admin UI: http://<server>:$ADMIN_PORT/"
