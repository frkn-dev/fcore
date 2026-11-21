#!/usr/bin/env bash
# Refresh the public RU IPv4 zone list served by api.frkn.org at
# /router/geo/ru.zone. Run by frkn-geo-update.timer once a day; safe to run by
# hand as well:
#
#   sudo GEO_DIR=/var/lib/frkn/geo /usr/local/lib/frkn/geo/update-ru-zone.sh
#
# The served file is replaced only after a full validation pass, so a failed
# download, an HTML error page or a truncated list never reaches routers: on
# any failure the previous file stays in place and the unit exits non-zero.
set -euo pipefail

GEO_DIR="${GEO_DIR:-/var/lib/frkn/geo}"
GEO_URL="${GEO_URL:-https://www.ipdeny.com/ipblocks/data/aggregated/ru-aggregated.zone}"
# Same floor as the api (MIN_ZONE_LINES in src/bin/api/http/handlers/geo.rs):
# the real list has ~8.6k CIDRs, 1000 catches truncation and error pages.
MIN_LINES="${MIN_LINES:-1000}"
CONNECT_TIMEOUT="${CONNECT_TIMEOUT:-10}"
MAX_TIME="${MAX_TIME:-30}"

ZONE="${GEO_DIR}/ru.zone"
SHA_FILE="${ZONE}.sha256"

log() { printf 'geo-update: %s\n' "$*" >&2; }

command -v curl >/dev/null || { log "curl is required"; exit 1; }

# Debian has sha256sum; macOS has shasum (used when the script is run locally
# for a smoke test).
if command -v sha256sum >/dev/null; then
    sha256_of() { sha256sum "$1" | awk '{print $1}'; }
elif command -v shasum >/dev/null; then
    sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
else
    log "sha256sum (or shasum) is required"
    exit 1
fi

mkdir -p "$GEO_DIR"

# Temp files live next to the destination: the final mv must be a rename on
# the same filesystem (atomic), and a reader that already has the old file
# open keeps a consistent inode while the new one takes over the name.
tmp="$(mktemp "${GEO_DIR}/.ru.zone.XXXXXX")"
tmp_sha="$(mktemp "${GEO_DIR}/.ru.zone.sha256.XXXXXX")"
trap 'rm -f "$tmp" "$tmp_sha" "${tmp}.lf"' EXIT

log "downloading ${GEO_URL}"
curl -fsS --proto '=https' --tlsv1.2 \
    --connect-timeout "$CONNECT_TIMEOUT" --max-time "$MAX_TIME" \
    --retry 2 --retry-delay 3 --retry-connrefused \
    -o "$tmp" "$GEO_URL"

# Normalize line endings before validating: a proxy rewriting CRLF must not
# look like corruption.
tr -d '\r' < "$tmp" > "${tmp}.lf"
mv -f "${tmp}.lf" "$tmp"

# Shape + size check. Every line must be a.b.c.d/0..32 with decimal octets
# <= 255 (awk compares numerically, so "abc" would slip through a bare regex).
# Only the first few bad lines are logged: an HTML error page would otherwise
# fill the journal with hundreds of lines.
if ! lines="$(awk -v min="$MIN_LINES" -F'[./]' '
    $0 !~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+\/([0-9]|[12][0-9]|3[0-2])$/ {
        bad = 1
        badcount++
        if (badcount <= 5) printf "bad line %d: %s\n", NR, $0 > "/dev/stderr"
        next
    }
    ($1 > 255 || $2 > 255 || $3 > 255 || $4 > 255) {
        bad = 1
        badcount++
        if (badcount <= 5) printf "octet out of range at line %d: %s\n", NR, $0 > "/dev/stderr"
        next
    }
    { count++ }
    END {
        if (bad) {
            if (badcount > 5) printf "... %d bad lines total\n", badcount > "/dev/stderr"
            exit 1
        }
        if (count < min) {
            printf "only %d CIDRs, expected at least %d\n", count, min > "/dev/stderr"
            exit 2
        }
        print count
    }
' "$tmp")"; then
    log "validation failed — keeping the previous ${ZONE}"
    exit 1
fi

sha="$(sha256_of "$tmp")"
log "${lines} CIDRs, sha256 ${sha}"

# ipdeny re-publishes an identical file now and then. Keeping the old file
# keeps Last-Modified and the ETag stable, so routers answer 304 for free.
if [[ -f "$SHA_FILE" ]]; then
    previous="$(awk '{print $1}' "$SHA_FILE")"
    if [[ "$previous" == "$sha" ]]; then
        log "unchanged — keeping the current file"
        exit 0
    fi
fi

chmod 0644 "$tmp"
printf '%s  ru.zone\n' "$sha" > "$tmp_sha"
chmod 0644 "$tmp_sha"

# Zone first, checksum second: the api derives both from the zone file, and
# the router verifies after downloading — a sub-second window where the
# on-disk .sha256 is stale affects only an operator, never the api.
mv -f "$tmp" "$ZONE"
mv -f "$tmp_sha" "$SHA_FILE"
log "published ${ZONE}"
