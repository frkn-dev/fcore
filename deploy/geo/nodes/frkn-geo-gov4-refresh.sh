#!/bin/bash
# FRKN EU pool (shard 0001) -- apply the verified Russian state-site zone to the
# nft set `inet frkn_geo gov4` (routing group `gov`).
#
#   sudo /usr/local/bin/frkn-geo-gov4-refresh.sh
#
# Same refresh path as `frkn-geo-ai4-refresh.sh` (conditional GET, sha256,
# shape/ratio guards, one atomic `nft -f`); only the zone and the set differ.
# What the set means is decided on the routing side: `frkn-msk-route.sh` puts
# `ip daddr @gov4 meta mark set 0x7 return` above EVERY other group mark, so a
# destination in this zone rides the single-purpose gov exit (table 700 ->
# wg-gov-msk -> msk-tw, exit IP 217.149.29.118) and can never be captured by
# @ru4/@exch4/@ai4/@youtube4. That ordering is the whole point: the general RU
# hosting ranges are WAF-flagged for state sites (pochta.ru -> 417 in 0.5 s,
# mos.ru -> no answer), so a gov address landing in table 200 breaks the site.
#
# Fail direction: the group is fail-closed on the routing side (unreachable
# guard in table 700), but a STALE list only means "a newly moved state site may
# still take the general RU path until the next run" -- the timer is hourly, and
# the resolver publishes hourly.
set -euo pipefail

export FRKN_GEO_ZONE_NAME="gov4.zone"
export FRKN_GEO_SET="gov4"

exec /usr/local/bin/frkn-geo-ai4-refresh.sh "$@"
