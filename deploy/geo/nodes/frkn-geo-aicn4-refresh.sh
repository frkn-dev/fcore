#!/bin/bash
# FRKN EU pool (shard 0001) -- apply the Chinese-AI DIRECT WHITELIST zone to
# the nft set `inet frkn_geo aicn4`.
#
#   sudo /usr/local/bin/frkn-geo-aicn4-refresh.sh
#
# Same refresh path as `frkn-geo-ai4-refresh.sh` (conditional GET, sha256,
# shape/ratio guards, one atomic `nft -f`); the only difference is the zone and
# the set. What the set means is decided on the routing side: `frkn-msk-route.sh`
# puts `ip daddr @aicn4 return` above every group mark, so a destination in this
# zone is never tagged and leaves from the node's own IP. The whitelist is
# therefore fail-safe in the opposite direction: a stale IP here means "some
# Chinese AI traffic exits directly anyway", never "blocked".
set -euo pipefail

export FRKN_GEO_ZONE_NAME="aicn4.zone"
export FRKN_GEO_SET="aicn4"

exec /usr/local/bin/frkn-geo-ai4-refresh.sh "$@"
