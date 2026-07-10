#!/usr/bin/env bash
set -euo pipefail

# Backfills pixel-agent storage from rotated nginx pixel logs.
# Usage: sudo ./backfill-pixel-logs.sh /etc/pixel-agent/config.toml

CONFIG_PATH="${1:-/etc/pixel-agent/config.toml}"
LOG_DIR="/var/log/nginx"
BACKFILL_LOG="/tmp/pixel-backfill.log"

if [ ! -f "$CONFIG_PATH" ]; then
    echo "Config not found: $CONFIG_PATH"
    exit 1
fi

echo "Preparing backfill log at $BACKFILL_LOG..."
: > "$BACKFILL_LOG"

# Append older gzipped logs in reverse chronological order (oldest first).
# Adjust glob if your logrotate naming differs.
for gz in $(ls -1 "$LOG_DIR"/pixel.log.*.gz 2>/dev/null | sort -V); do
    echo "Unpacking $gz..."
    zcat "$gz" >> "$BACKFILL_LOG"
done

# Append uncompressed rotated logs if any.
for rotated in $(ls -1 "$LOG_DIR"/pixel.log.[0-9] 2>/dev/null | sort -V); do
    echo "Appending $rotated..."
    cat "$rotated" >> "$BACKFILL_LOG"
done

# Append current log.
if [ -f "$LOG_DIR/pixel.log" ]; then
    echo "Appending current $LOG_DIR/pixel.log..."
    cat "$LOG_DIR/pixel.log" >> "$BACKFILL_LOG"
fi

echo "Running backfill..."
pixel-agent-backfill "$CONFIG_PATH" "$BACKFILL_LOG"

echo "Cleaning up..."
rm -f "$BACKFILL_LOG"

echo "Backfill complete. Restart pixel-agent to pick up the updated snapshot."
