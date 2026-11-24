#!/bin/bash
# FRKN EU pool (shard 0001) -- apply one verified geo zone to its nft set.
#
# One script, one zone per run: the zone name and the set are the only inputs,
# so the `ai` group (`ai4.zone` -> set `ai4`) and the Chinese-AI direct
# whitelist (`aicn4.zone` -> set `aicn4`) share the whole verification path.
#
#   sudo /usr/local/bin/frkn-geo-ai4-refresh.sh      # ai4.zone  -> set ai4
#   sudo /usr/local/bin/frkn-geo-aicn4-refresh.sh    # aicn4.zone -> set aicn4
#
# Run hourly by frkn-geo-ai4.timer / frkn-geo-aicn4.timer.
#
# Source: https://api.frkn.org/router/geo/<zone> -- a list of /32s that the
# api host already verified by TLS/SNI for the group's domains. This script
# does not resolve anything itself; its job is to move the list into the kernel
# without ever exposing a half-filled set:
#
#   1. conditional GET (If-None-Match): 304 -> nothing to do, exit 0 -- unless
#      the kernel set is empty (someone rebuilt `inet frkn_geo`), in which case
#      the zone stored on disk is re-applied;
#   2. verify the sha256 the api publishes next to the list, so a truncated
#      download cannot pass;
#   3. shape/size check: every line an IPv4 CIDR, at least MIN_ELEMENTS, and
#      never below MIN_RATIO_PCT of the set currently loaded (a shrunken list
#      would silently stop routing a service -- keep the old one);
#   4. replace the set in ONE `nft -f` transaction (flush + add), which nft
#      commits atomically: lookups see either the whole old set or the whole
#      new one, never a gap;
#   5. only then persist the list and its ETag for the next round / boot seed.
#
# Failure at any step leaves both the kernel set and the on-disk files as they
# were. Fetch/network failures exit 0 (the previous generation keeps working);
# validation failures exit 1 so `systemctl --failed` shows real trouble.
set -euo pipefail

ZONE_NAME="${FRKN_GEO_ZONE_NAME:-ai4.zone}"
SET="${FRKN_GEO_SET:-ai4}"
TABLE="${FRKN_GEO_TABLE:-frkn_geo}"
BASE_URL="${FRKN_GEO_BASE_URL:-https://api.frkn.org/router/geo}"
DIR="${FRKN_GEO_DIR:-/etc/frkn/geo}"
MIN_ELEMENTS="${FRKN_GEO_MIN_ELEMENTS:-8}"
MIN_RATIO_PCT="${FRKN_GEO_MIN_RATIO_PCT:-60}"
TAG="frkn-geo-${ZONE_NAME%.zone}"

ZONE="$DIR/$ZONE_NAME"
ETAG_FILE="$DIR/${ZONE_NAME%.zone}.etag"
COUNT_FILE="$DIR/${ZONE_NAME%.zone}.count"
TMP=$(mktemp "$DIR/.$ZONE_NAME.XXXXXX")
HDR=$(mktemp "$DIR/.$ZONE_NAME.hdr.XXXXXX")
NFT_FILE=$(mktemp "$DIR/.$ZONE_NAME.nft.XXXXXX")
trap 'rm -f "$TMP" "$HDR" "$NFT_FILE"' EXIT

log() { printf '%s: %s\n' "$TAG" "$*" >&2; logger -t "$TAG" -- "$*" 2>/dev/null || true; }

mkdir -p "$DIR"

# How many elements the set is expected to hold: the count of the last list this
# script applied. An interval set with `auto-merge` can fold adjacent /32s into
# a range, so counting nft's own output under-reports a merged set -- the state
# file is exact. nft output is only the fallback for the very first run.
#
# `|| true` is load-bearing: on that first run the set is EMPTY, `grep` finds no
# address and exits 1, and with `set -o pipefail` the whole pipeline (and with
# `set -e` the script) fails before the first apply -- the zone could never be
# bootstrapped onto a node whose set had never been seeded.
live_count() {
    if [ -s "$COUNT_FILE" ]; then
        cat "$COUNT_FILE"
        return 0
    fi
    nft list set inet "$TABLE" "$SET" 2>/dev/null \
        | grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}' | wc -l || true
}

ensure_set() {
    if nft list set inet "$TABLE" "$SET" >/dev/null 2>&1; then
        return 0
    fi
    log "set inet $TABLE $SET is missing, creating an empty one"
    nft add set inet "$TABLE" "$SET" '{ type ipv4_addr; flags interval; auto-merge; }'
}

# Elements actually present in the kernel set. An interval set with `auto-merge`
# folds adjacent /32s into ranges, so this under-counts a merged set -- it is
# used only to tell "empty because the table was rebuilt" from "populated".
kernel_count() {
    nft list set inet "$TABLE" "$SET" 2>/dev/null \
        | grep -oE '([0-9]{1,3}\.){3}[0-9]{1,3}(-[0-9.]+)?(/[0-9]+)?' | wc -l || true
}

# One `nft -f` file per apply: nft commits it atomically, so a packet never
# finds the set empty or half-filled.
write_nft() { # write_nft <source file>
    ensure_set
    {
        echo "flush set inet $TABLE $SET"
        sed -e 's|[[:space:]]*$||' -e '/^$/d' "$1" \
            | sed "s|^|add element inet $TABLE $SET { |; s|$| }|"
    } > "$NFT_FILE"
}

# --- 1. conditional GET ------------------------------------------------------
etag=""
if [ -f "$ETAG_FILE" ]; then
    etag="$(cat "$ETAG_FILE")"
fi

code="$(curl -sS -o "$TMP" -D "$HDR" -w '%{http_code}' \
    --proto '=https' --tlsv1.2 --connect-timeout 10 --max-time 60 --retry 2 --retry-delay 3 \
    -H "If-None-Match: ${etag}" \
    "$BASE_URL/$ZONE_NAME" 2>/dev/null)" || code=000

case "$code" in
    304)
        # A rebuilt `inet frkn_geo` (frkn-msk-route.sh recreates it) leaves the
        # set empty while the published zone is unchanged, so a 304 alone does
        # not mean "nothing to do": re-apply the copy on disk.
        if [ -s "$ZONE" ] && [ "$(kernel_count)" -eq 0 ]; then
            log "unchanged (304) but set $SET is empty in the kernel, re-applying $ZONE"
            write_nft "$ZONE"
            if ! nft -f "$NFT_FILE"; then
                log "nft transaction failed, set left as it was"
                exit 1
            fi
            log "re-applied $ZONE_NAME from disk: 0 -> $(kernel_count) elements"
            exit 0
        fi
        log "unchanged (304), set keeps $(live_count) elements"
        exit 0
        ;;
    200) ;;
    *)
        log "fetch failed (HTTP $code), keeping the current set ($(live_count) elements)"
        exit 0
        ;;
esac

# --- 2. checksum published next to the list ---------------------------------
want="$(curl -fsS --connect-timeout 10 --max-time 30 "$BASE_URL/$ZONE_NAME.sha256" 2>/dev/null | awk '{print $1}' || true)"
got="$(sha256sum "$TMP" | awk '{print $1}')"
if [ -z "$want" ]; then
    log "no checksum from $BASE_URL/$ZONE_NAME.sha256, refusing to apply"
    exit 1
fi
if [ "$want" != "$got" ]; then
    log "checksum mismatch (api $want, download $got), keeping the current set"
    exit 1
fi
# The ETag is the same sha256; cross-check when the header is present.
header_etag="$(tr -d '\r' < "$HDR" | awk -F': ' 'tolower($1)=="etag"{print $2}')"
if [ -n "$header_etag" ] && [ "$header_etag" != "\"sha256:$got\"" ]; then
    log "ETag/checksum disagree ($header_etag vs sha256:$got), keeping the current set"
    exit 1
fi

# --- 3. shape and size ------------------------------------------------------
tr -d '\r' < "$TMP" > "$TMP.lf" && mv -f "$TMP.lf" "$TMP"
total=$(wc -l < "$TMP")
valid=$(grep -cE '^([0-9]{1,3}\.){3}[0-9]{1,3}(/([0-9]|[12][0-9]|3[0-2]))?$' "$TMP" || true)
if [ "$total" -ne "$valid" ]; then
    log "only $valid of $total lines are IPv4 CIDRs, keeping the current set"
    exit 1
fi
if [ "$valid" -lt "$MIN_ELEMENTS" ]; then
    log "only $valid elements (floor $MIN_ELEMENTS), keeping the current set"
    exit 1
fi
old_count="$(live_count)"
if [ "$old_count" -gt 0 ] && [ $((valid * 100)) -lt $((old_count * MIN_RATIO_PCT)) ]; then
    log "$valid elements is below ${MIN_RATIO_PCT}% of the loaded $old_count, keeping the current set"
    exit 1
fi

# --- 4. one atomic transaction ---------------------------------------------
# flush + add in a single `nft -f`: nft commits the whole file or nothing, so
# a packet never finds the set empty or half-filled.
write_nft "$TMP"

if ! nft -f "$NFT_FILE"; then
    log "nft transaction failed, keeping the current set"
    exit 1
fi
log "applied $ZONE_NAME: $old_count -> $valid elements (sha256 $got)"

# --- 5. persist for the next conditional GET and for the boot seed ----------
chmod 0644 "$TMP"
mv -f "$TMP" "$ZONE"
printf '%s\n' "\"sha256:$got\"" > "$ETAG_FILE.tmp"
mv -f "$ETAG_FILE.tmp" "$ETAG_FILE"
printf '%s\n' "$valid" > "$COUNT_FILE.tmp"
mv -f "$COUNT_FILE.tmp" "$COUNT_FILE"
log "stored $ZONE and ETag"
