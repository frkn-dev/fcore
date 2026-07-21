# FRKN Pixel Analytics Backfill

One-shot CLI tool that reads a pixel log (plain or gzipped) and rebuilds the metric snapshot.

## What it does

- Reads a log file or stdin.
- Parses every line into pixel events.
- Resolves GeoIP country.
- Flushes aggregated samples into the same snapshot file used by `pixel-agent`.

## Files

- `pixel-agent-backfill-example.toml` — same shape as `pixel-agent-example.toml`, usually with a separate offset path.
- `pixel-agent-backfill.service` — oneshot systemd unit.
- `pixel-agent-backfill.timer` — runs the backfill every 6 hours.

## Usage

```bash
# From file
pixel-agent-backfill /etc/pixel-agent/backfill.toml /var/log/nginx/pixel.log

# From gzipped archive
pixel-agent-backfill /etc/pixel-agent/backfill.toml /var/log/nginx/pixel.log.1.gz

# From stdin
zcat /var/log/nginx/pixel.log.1.gz | pixel-agent-backfill /etc/pixel-agent/backfill.toml -
```

## Deploy from release

The backfill binary is installed by `deploy/pixel-agent-deploy.sh`. To enable periodic backfills:

```bash
sudo cp src/bin/pixel_agent/pixel-agent-backfill.service /etc/systemd/system/
sudo cp src/bin/pixel_agent/pixel-agent-backfill.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now pixel-agent-backfill.timer
```
