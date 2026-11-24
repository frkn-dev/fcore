#!/usr/bin/env python3
"""Publish a verified geo zone: service domains -> verified IPv4 -> nft set.

Four zones are published with this one script, distinguished by `--group` /
`--domains` / `--zone-name`:

  * `ai4.zone` (group `ai`) — AI services that behave differently by country
    and therefore take a dedicated exit on the pool nodes;
  * `aicn4.zone` (group `aicn4`) — Chinese AI services (DeepSeek, Qwen, Kimi,
    GLM, MiniMax, Volcengine/Doubao) that are reachable from the EU as-is. The
    nodes use this zone as a *direct whitelist*: a destination in it is never
    marked, so it leaves from the node's own IP. Same pipeline, opposite
    routing decision, hence a separate set and a separate zone;
  * `exch4.zone` (group `exch4`) — Chinese crypto exchanges, which refuse or
    degrade US/EU addresses; the nodes mark them into the RU exit group;
  * `adult4.zone` (group `adult4`) — mainstream adult platforms and the CDN
    hosts they use. Second *direct whitelist*: those destinations must always
    leave from the NL entry node, never from the RU/US/KZ exit, so the nodes
    put `ip daddr @adult4 return` above every group mark;
  * `gov4.zone` (group `gov4`) — Russian state sites served by one dedicated
    exit (mark 0x7 -> table 700 -> msk-tw). This is the only group with the
    *relaxed trust* mode described below: a large part of the Russian public
    sector moved to certificates issued by the Минцифры (Russian Trusted
    Sub CA), whose root is not in any default trust store. `--extra-ca` adds
    that root next to the default store, and `--probe-via` moves the TLS probe
    onto a node that reaches these sites through the very tunnel the zone
    feeds (`--probe-source` binds the probe to the tunnel address). Every
    other group keeps strict default-store verification and verifies locally.

Run by `frkn-geo-{ai,aicn4,exch4,adult4}-update.timer` on the api host once an
hour; safe to run by hand:

    sudo GEO_DIR=/var/lib/frkn/geo ./deploy/geo/update-ai4-zone.py --verbose
    sudo GEO_DIR=/var/lib/frkn/geo ./deploy/geo/update-ai4-zone.py \
        --group adult4 --domains /etc/frkn/geo/domains/adult.txt \
        --zone-name adult4.zone --verbose

Pipeline (source of truth: a domains file, one suffix per line):

  1. Resolve every listed suffix — the entry itself plus `www.<entry>` when the
     entry has exactly two labels — as A records, from *several* resolvers
     (1.1.1.1 and 8.8.8.8), and union the answers.
  2. Verify every candidate IP by opening TLS to it with the host name as SNI
     and a full default certificate check (chain + name, wildcards included).
     Only IPs that actually serve that name survive. This is what makes the
     set safe to route on: a stale or unrelated A record is dropped here, not
     routed. Two gov-only relaxations exist (`--extra-ca`, `--probe-via`); both
     are refused for every other group. They never drop the hostname check and
     never accept a certificate that matches nothing: the extra bundle only
     widens *which CA* may have issued the chain, and the remote probe only
     changes *where* the handshake is made.
  3. If a domain produces no verified IP, reuse that domain's previous
     generation (the service is probably briefly unreachable) and say so in
     the log. If *most* domains produce nothing, the checker or the network is
     broken — refuse to publish instead of publishing a stale set.
  4. Refuse to publish a set smaller than --min-ratio (60 %) of the previous
     one. A shrunken list would silently stop routing a service; keeping the
     old generation for one more hour is strictly better.
  5. Write `<zone>` + `<zone>.sha256` + `<zone-without-.zone>.count`
     atomically (temp file in the same directory, then rename), so the api
     never serves a half-written list and an operator can see the size of the
     last generation without counting lines.

Nothing here restarts the api: the handler reads the file per request.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import ipaddress
import json
import os
import re
import shlex
import socket
import ssl
import subprocess
import sys
import tempfile
import time
from pathlib import Path

# Resolvers are queried in order; answers are unioned. Two independent
# recursive resolvers catch a poisoned or partially-broken first answer.
DEFAULT_RESOLVERS = ("1.1.1.1", "8.8.8.8")

# One TLS probe per (host, IP). The certificate alone is the trust anchor:
# a wrong/expired/mismatched chain is a verification failure, not a warning.
TLS_PORT = 443
TLS_TIMEOUT = 6.0
DNS_TIMEOUT = 4.0
WORKERS = 24

# Refuse to publish below this share of the previous generation.
DEFAULT_MIN_RATIO_PCT = 60
# ...and below this share of domains with a *fresh* verified answer: that is a
# broken checker (no outbound 443, DNS down), not a broken service.
DEFAULT_MIN_FRESH_PCT = 50

LABEL_RE = re.compile(r"^(?!-)[a-z0-9-]{1,63}(?<!-)$")

# Only this group may use the relaxed-trust / remote-probe mode. Russian state
# sites present Минцифры-issued chains and are unreachable from the CZ api
# host, so their zone has to be verified differently — but an AI, exchange or
# adult address verified that way would defeat the purpose of the check for
# groups where a wrong IP means a broken service or a leak.
RELAXED_TRUST_GROUP = "gov4"

# ssh target for the gov probe (a node whose table 700 is the gov exit) and the
# address the probe binds to on that node (the tunnel address, which the node's
# `ip rule from <addr> lookup 700` sends through wg-gov-msk).
DEFAULT_PROBE_TIMEOUT = 30.0
PROBE_KEY = os.environ.get("GEO_PROBE_KEY", "/etc/frkn/geo/probe_ed25519")
PROBE_KNOWN_HOSTS = os.environ.get("GEO_PROBE_KNOWN_HOSTS", "/etc/frkn/geo/probe_known_hosts")

# Log tag, set from `--group` in main(): `geo-ai4:` / `geo-aicn4:`. Journal
# lines are how an operator tells the two zones' runs apart.
LOG_NAME = "geo-ai4"



def log(message: str) -> None:
    print(f"{LOG_NAME}: {message}", file=sys.stderr, flush=True)


# --------------------------------------------------------------------------- #
# Domains file
# --------------------------------------------------------------------------- #


def load_domains(path: Path) -> list[str]:
    """One suffix per line, `#` comments, blank lines ignored, lowercased."""
    entries: list[str] = []
    seen: set[str] = set()
    for lineno, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw.split("#", 1)[0].strip().lower()
        if not line:
            continue
        labels = line.split(".")
        if len(labels) < 2 or not all(LABEL_RE.match(label) for label in labels):
            raise SystemExit(f"{path}:{lineno}: not a domain suffix: {raw!r}")
        if line in seen:
            continue
        seen.add(line)
        entries.append(line)
    if not entries:
        raise SystemExit(f"{path}: no domains listed")
    return entries


def hosts_for(entry: str) -> list[str]:
    """The name that must answer for this suffix.

    The entry itself always. `www.` is only added for a bare registrable-ish
    suffix (`claude.ai`): `www.gemini.google.com` never exists and would just
    burn a DNS lookup and a log line.
    """
    hosts = [entry]
    if len(entry.split(".")) == 2:
        hosts.append(f"www.{entry}")
    return hosts


# --------------------------------------------------------------------------- #
# DNS
# --------------------------------------------------------------------------- #


def resolve(host: str, resolver: str) -> set[str]:
    """A records for `host` via `dig @resolver`, IPv4 only.

    Falls back to the system resolver when dig is not installed: still a
    second opinion, just not one of the two named resolvers.
    """
    try:
        out = subprocess.run(
            ["dig", f"+time={int(DNS_TIMEOUT)}", "+tries=1", "+short", "A", host, f"@{resolver}"],
            capture_output=True,
            text=True,
            timeout=DNS_TIMEOUT + 2,
            check=False,
        ).stdout
    except FileNotFoundError:
        return _resolve_system(host)
    except subprocess.TimeoutExpired:
        return set()

    addrs: set[str] = set()
    for line in out.splitlines():
        line = line.strip()
        # `+short` also returns CNAME targets on some chains; only dotted
        # quads are answers for us.
        if not re.fullmatch(r"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}", line):
            continue
        try:
            addr = ipaddress.IPv4Address(line)
        except ValueError:
            continue
        addrs.add(str(addr))
    return addrs


def _resolve_system(host: str) -> set[str]:
    try:
        infos = socket.getaddrinfo(host, None, socket.AF_INET, socket.SOCK_STREAM)
    except OSError:
        return set()
    return {str(info[4][0]) for info in infos}


# --------------------------------------------------------------------------- #
# TLS / SNI verification
# --------------------------------------------------------------------------- #


def verify(host: str, ip: str, extra_ca: Path | None = None) -> tuple[bool, str]:
    """True only if `ip:443` answers a TLS handshake for `host` with a valid
    certificate for `host` (SAN or wildcard, chain verified).

    `check_hostname` + `CERT_REQUIRED` on the default context is exactly the
    check a browser makes, so a shared-CDN IP that serves a *different* name
    is rejected rather than routed.

    `extra_ca` (gov only) loads one more trust anchor *next to* the default
    store — ssl adds it to the same context, it does not replace the system
    roots. That is what makes Минцифры-issued certificates acceptable: the
    chain must still reach *a* trusted root and the leaf must still match the
    host name, so an arbitrary or self-signed leaf is still rejected.
    """
    ctx = ssl.create_default_context()
    if extra_ca is not None:
        ctx.load_verify_locations(cafile=str(extra_ca))
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED
    try:
        with socket.create_connection((ip, TLS_PORT), timeout=TLS_TIMEOUT) as sock:
            with ctx.wrap_socket(sock, server_hostname=host):
                return True, "ok"
    except ssl.SSLCertVerificationError as err:
        return False, f"cert: {err.verify_message or err.reason or 'mismatch'}"
    except ssl.SSLError as err:
        return False, f"tls: {err.reason or 'handshake failed'}"
    except (socket.timeout, TimeoutError):
        return False, "timeout"
    except OSError as err:
        return False, f"net: {err.strerror or err}"


# --------------------------------------------------------------------------- #
# Remote probe (gov only)
# --------------------------------------------------------------------------- #
#
# The api host sits in CZ; Russian state sites answer only Russian sources, so
# a local handshake there fails at TCP for most of them. The probe therefore
# runs on a node, bound to the gov tunnel address, i.e. through the exact path
# the published zone sends traffic down. One ssh per run: the whole job list
# goes in as JSON, the results come back as JSON, and the remote side runs this
# same file (`--probe-batch`) so local and remote checks cannot drift apart.


def run_remote_probe(
    target: str, jobs: list[tuple[str, str]], extra_ca: Path | None, source: str | None
) -> dict[tuple[str, str], tuple[bool, str]]:
    """Verify `jobs` on the node `target` over ssh; returns {(host, ip): result}.

    The node runs *this same file* (`-c <source> --probe-batch`), so the local
    and the remote check cannot drift apart. The job list and the extra CA
    travel as JSON on stdin, not as files on the node.
    """
    payload = {
        "jobs": [list(job) for job in jobs],
        "source": source,
        "ca_pem": extra_ca.read_text(encoding="utf-8") if extra_ca else None,
        "timeout": TLS_TIMEOUT,
    }
    try:
        source_text = Path(__file__).read_text(encoding="utf-8")
    except OSError as err:
        log(f"probe: cannot read own source for the remote side: {err}")
        return {}
    cmd = [
        "ssh",
        "-F",
        "/dev/null",
        "-i",
        PROBE_KEY,
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=accept-new",
        "-o",
        f"UserKnownHostsFile={PROBE_KNOWN_HOSTS}",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "LogLevel=ERROR",
        target,
        f"python3 -c {shlex.quote(source_text)} --probe-batch",
    ]
    try:
        proc = subprocess.run(
            cmd,
            input=json.dumps(payload),
            capture_output=True,
            text=True,
            timeout=DEFAULT_PROBE_TIMEOUT,
            check=False,
        )
    except subprocess.TimeoutExpired:
        log(f"probe: {target} did not answer within {DEFAULT_PROBE_TIMEOUT:.0f}s")
        return {}
    if proc.returncode != 0:
        log(f"probe: {target} failed (rc={proc.returncode}): {proc.stderr.strip()[:300]}")
        return {}
    results: dict[tuple[str, str], tuple[bool, str]] = {}
    for line in proc.stdout.splitlines():
        try:
            host, ip, ok, reason = line.split("\t", 3)
        except ValueError:
            continue
        results[(host, ip)] = (ok == "ok", reason)
    return results


def probe_batch_main() -> int:
    """Remote entry point: same checks, run on the node, JSON in / lines out."""
    global TLS_TIMEOUT

    payload = json.loads(sys.stdin.read())
    jobs = [(str(host), str(ip)) for host, ip in payload["jobs"]]
    timeout = float(payload.get("timeout") or TLS_TIMEOUT)
    source = payload.get("source") or None

    ca_path: Path | None = None
    if payload.get("ca_pem"):
        handle = tempfile.NamedTemporaryFile("w", suffix=".pem", delete=False)
        handle.write(payload["ca_pem"])
        handle.close()
        ca_path = Path(handle.name)

    TLS_TIMEOUT = timeout

    def one(job: tuple[str, str]) -> tuple[str, str, bool, str]:
        host, ip = job
        ok, reason = verify_through(host, ip, ca_path, source)
        return host, ip, ok, reason

    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=WORKERS) as pool:
            for host, ip, ok, reason in pool.map(one, jobs):
                print(f"{host}\t{ip}\t{'ok' if ok else 'fail'}\t{reason}", flush=True)
    finally:
        if ca_path is not None:
            try:
                ca_path.unlink()
            except OSError:
                pass
    return 0



def verify_through(
    host: str, ip: str, extra_ca: Path | None, source: str | None
) -> tuple[bool, str]:
    """`verify`, but from `source` when one is given (probe side only)."""
    if not source:
        return verify(host, ip, extra_ca)
    ctx = ssl.create_default_context()
    if extra_ca is not None:
        ctx.load_verify_locations(cafile=str(extra_ca))
    ctx.check_hostname = True
    ctx.verify_mode = ssl.CERT_REQUIRED
    try:
        with socket.create_connection(
            (ip, TLS_PORT), timeout=TLS_TIMEOUT, source_address=(source, 0)
        ) as sock:
            with ctx.wrap_socket(sock, server_hostname=host):
                return True, "ok"
    except ssl.SSLCertVerificationError as err:
        return False, f"cert: {err.verify_message or err.reason or 'mismatch'}"
    except ssl.SSLError as err:
        return False, f"tls: {err.reason or 'handshake failed'}"
    except (socket.timeout, TimeoutError):
        return False, "timeout"
    except OSError as err:
        return False, f"net: {err.strerror or err}"



# --------------------------------------------------------------------------- #
# State
# --------------------------------------------------------------------------- #


def load_state(path: Path) -> dict:
    try:
        state = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return state if isinstance(state, dict) else {}


def sort_ips(ips: set[str]) -> list[str]:
    """Numeric IPv4 order, so identical sets keep the same sha256 and the api
    keeps answering 304."""
    return sorted(ips, key=ipaddress.IPv4Address)


def zone_cidrs(ips: set[str]) -> list[str]:
    return [f"{ip}/32" for ip in sort_ips(ips)]


def atomic_write(path: Path, data: bytes, mode: int = 0o644) -> None:
    fd, tmp = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


# --------------------------------------------------------------------------- #
# Main
# --------------------------------------------------------------------------- #


def main() -> int:
    global LOG_NAME

    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--group", default=os.environ.get("GEO_GROUP", "ai4"), help="group name (log label)"
    )
    parser.add_argument(
        "--domains",
        type=Path,
        default=Path(os.environ.get("GEO_DOMAINS_DIR", "/etc/frkn/geo/domains")) / "ai.txt",
        help="domain list, one suffix per line",
    )
    parser.add_argument(
        "--out-dir", type=Path, default=Path(os.environ.get("GEO_DIR", "/var/lib/frkn/geo"))
    )
    parser.add_argument("--zone-name", default=os.environ.get("GEO_ZONE_NAME", "ai4.zone"))
    parser.add_argument(
        "--resolvers",
        default=os.environ.get("GEO_RESOLVERS", ",".join(DEFAULT_RESOLVERS)),
        help="comma-separated DNS resolvers",
    )
    parser.add_argument("--min-ratio", type=int, default=DEFAULT_MIN_RATIO_PCT)
    parser.add_argument("--min-fresh", type=int, default=DEFAULT_MIN_FRESH_PCT)
    parser.add_argument(
        "--extra-ca",
        type=Path,
        default=None,
        help=f"extra CA bundle loaded next to the default store ({RELAXED_TRUST_GROUP} only)",
    )
    parser.add_argument(
        "--probe-via",
        default=None,
        metavar="SSH_TARGET",
        help=f"run the TLS probe on this node over ssh ({RELAXED_TRUST_GROUP} only)",
    )
    parser.add_argument(
        "--probe-source",
        default=None,
        metavar="ADDR",
        help="source address the node-side probe binds to (the gov tunnel address)",
    )
    parser.add_argument("--probe-batch", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--force", action="store_true", help="skip the shrink/freshness guards")
    parser.add_argument("--dry-run", action="store_true", help="resolve and verify, write nothing")
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    # Remote entry point: the api sends this file over ssh and feeds a JSON job
    # list on stdin. Nothing else runs in this mode.
    if args.probe_batch:
        return probe_batch_main()

    LOG_NAME = f"geo-{args.group}"

    # Relaxed trust and a remote probe are for the state-site group only. An
    # `ai4`/`exch4`/`adult4` address verified that way would let a wrong IP
    # through exactly where the check matters most.
    if args.group != RELAXED_TRUST_GROUP and (
        args.extra_ca is not None or args.probe_via or args.probe_source
    ):
        parser.error(
            f"--extra-ca / --probe-via / --probe-source are {RELAXED_TRUST_GROUP}-only; "
            f"group {args.group!r} keeps strict default-store verification"
        )
    if args.probe_source and not args.probe_via:
        parser.error("--probe-source needs --probe-via")
    if args.extra_ca is not None and not args.extra_ca.is_file():
        parser.error(f"--extra-ca: {args.extra_ca} does not exist")

    started = time.time()
    resolvers = [r.strip() for r in args.resolvers.split(",") if r.strip()]
    entries = load_domains(args.domains)
    hosts: list[str] = []
    for entry in entries:
        hosts.extend(hosts_for(entry))
    # Two hosts can collapse to the same name (entry `www.x.y` + `x.y`? no,
    # but be explicit): keep the first occurrence only.
    hosts = list(dict.fromkeys(hosts))

    zone_path = args.out_dir / args.zone_name
    sha_path = args.out_dir / f"{args.zone_name}.sha256"
    count_path = args.out_dir / f"{args.zone_name.removesuffix('.zone')}.count"
    state_path = args.out_dir / args.zone_name.replace(".zone", ".state.json")
    previous = load_state(state_path)
    previous_by_domain: dict[str, list[str]] = {
        domain: [ip for ip in ips if _is_ipv4(ip)]
        for domain, ips in (previous.get("zones") or {}).items()
        if isinstance(ips, list)
    }
    previous_total = (
        len(zone_path.read_text(encoding="utf-8").splitlines()) if zone_path.is_file() else 0
    )

    # 1. DNS: every host, every resolver.
    resolved: dict[str, set[str]] = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=WORKERS) as pool:
        futures = {
            pool.submit(resolve, host, resolver): (host, resolver)
            for host in hosts
            for resolver in resolvers
        }
        for future in concurrent.futures.as_completed(futures):
            host, resolver = futures[future]
            try:
                answers = future.result()
            except Exception as err:  # noqa: BLE001 - one resolver must not kill the run
                log(f"dns {host} @{resolver}: {err}")
                answers = set()
            if answers:
                resolved.setdefault(host, set()).update(answers)

    candidates = {host: sorted(ips) for host, ips in resolved.items()}
    total_candidates = sum(len(ips) for ips in candidates.values())

    # 2. TLS: every (host, IP) pair, concurrently. Verification is the gate.
    verified: dict[str, set[str]] = {}
    failures: dict[str, str] = {}
    with concurrent.futures.ThreadPoolExecutor(max_workers=WORKERS) as pool:
        futures = {
            pool.submit(verify, host, ip, args.extra_ca): (host, ip)
            for host, ips in candidates.items()
            for ip in ips
        }
        for future in concurrent.futures.as_completed(futures):
            host, ip = futures[future]
            ok, reason = future.result()
            if ok:
                verified.setdefault(host, set()).add(ip)
            else:
                failures[f"{host}@{ip}"] = reason

    # 2b. Gov only: retry what the api host could not reach, from a node whose
    #     probe rides the gov tunnel — the path the published set feeds. The
    #     local pass stays the first opinion (a set that shrank because the
    #     tunnel was down would be worse than useless), and the union of both
    #     passes is what gets published.
    if args.probe_via:
        retry = [
            (host, ip)
            for host, ips in candidates.items()
            for ip in ips
            if ip not in verified.get(host, set())
        ]
        if retry:
            remote = run_remote_probe(args.probe_via, retry, args.extra_ca, args.probe_source)
            recovered = 0
            for host, ip in retry:
                ok, reason = remote.get((host, ip), (False, "probe: no answer"))
                if ok:
                    verified.setdefault(host, set()).add(ip)
                    failures.pop(f"{host}@{ip}", None)
                    recovered += 1
                else:
                    failures[f"{host}@{ip}"] = f"{failures.get(f'{host}@{ip}', '')} / via gov exit: {reason}"
            log(
                f"probe via {args.probe_via} (source {args.probe_source or 'default'}): "
                f"{len(remote)}/{len(retry)} answered, {recovered} recovered"
            )
        else:
            log("probe: nothing to retry, every candidate verified locally")


    # 3. Per-domain generation: fresh answers, else the previous generation.
    zones: dict[str, list[str]] = {}
    stale: list[str] = []
    fresh_domains = 0
    for entry in entries:
        ips: set[str] = set()
        for host in hosts_for(entry):
            ips |= verified.get(host, set())
        if ips:
            fresh_domains += 1
            zones[entry] = sort_ips(ips)
            continue
        if previous_by_domain.get(entry):
            zones[entry] = previous_by_domain[entry]
            stale.append(entry)
            log(f"{entry}: no verified IP, reusing previous generation ({len(previous_by_domain[entry])} IPs)")
        else:
            zones[entry] = []
            log(f"{entry}: no verified IP and no previous generation, contributing nothing")

    all_ips = {ip for ips in zones.values() for ip in ips}
    cidrs = zone_cidrs(all_ips)
    total_domains = len(entries)
    stale_share = len(stale) * 100 // max(total_domains, 1)
    fresh_share = fresh_domains * 100 // max(total_domains, 1)

    if args.verbose:
        for entry in entries:
            ips = zones.get(entry, [])
            mark = "stale" if entry in stale else ("fresh" if ips else "none")
            print(f"  {entry:<34} {len(ips):>3} IPs  {mark}", file=sys.stderr)
            if not ips:
                for host in hosts_for(entry):
                    for ip in candidates.get(host, []):
                        print(f"      {host}@{ip}: {failures.get(f'{host}@{ip}', 'not verified')}",
                              file=sys.stderr)

    # 4. Guards. Every one of them keeps the published generation intact.
    if not all_ips:
        log("no verified IPs at all — keeping the previous zone")
        return 5
    if not args.force and fresh_share < args.min_fresh:
        log(
            f"only {fresh_domains}/{total_domains} domains verified ({fresh_share}% < {args.min_fresh}%) "
            "— the checker or the network is broken; keeping the previous zone"
        )
        return 4
    if not args.force and previous_total and len(cidrs) * 100 < previous_total * args.min_ratio:
        log(
            f"{len(cidrs)} CIDRs is below {args.min_ratio}% of the previous {previous_total} "
            "— refusing to publish"
        )
        return 3

    content = ("\n".join(cidrs) + "\n").encode()
    summary = (
        f"{total_domains} domains, {len(hosts)} hosts, {total_candidates} candidate IPs, "
        f"{len(all_ips)} verified/kept, {len(stale)} stale domains, {len(cidrs)} CIDRs "
        f"(previous {previous_total})"
    )

    if args.dry_run:
        log(f"dry run: {summary}")
        return 0

    args.out_dir.mkdir(parents=True, exist_ok=True)
    if zone_path.exists() and zone_path.read_bytes() == content:
        log(f"unchanged: {summary}")
    else:
        atomic_write(zone_path, content)

    digest = hashlib.sha256(content).hexdigest()
    atomic_write(sha_path, f"{digest}  {args.zone_name}\n".encode())
    # Size of the published generation for operators and for the node-side
    # ratio guard's bookkeeping (`ai4.count` / `aicn4.count`).
    atomic_write(count_path, f"{len(cidrs)}\n".encode())

    state = {
        "group": args.group,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "zone": args.zone_name,
        "zones": {entry: zones.get(entry, []) for entry in entries},
        "stale": stale,
        "cidr_count": len(cidrs),
        "resolvers": resolvers,
        "elapsed_s": round(time.time() - started, 1),
    }
    atomic_write(state_path, (json.dumps(state, indent=1, sort_keys=True) + "\n").encode())
    log(f"published {zone_path} sha256 {digest}: {summary}")
    if stale:
        log(f"stale domains ({stale_share}% of the list): {', '.join(stale)}")
    return 0


def _is_ipv4(value: str) -> bool:
    try:
        ipaddress.IPv4Address(value)
    except (ValueError, TypeError):
        return False
    return True


if __name__ == "__main__":
    sys.exit(main())
