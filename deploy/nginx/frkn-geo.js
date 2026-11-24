// /etc/nginx/frkn-geo.js — /router/geo/* served straight from the zone files.
//
// The zone lists in /var/lib/frkn/geo are written hourly/daily by the
// resolvers in deploy/geo. Serving them used to be a code path in the api
// binary (`src/bin/api/http/handlers/geo.rs`, with a hard-coded `ZONES`
// allowlist), so every new zone needed an api rebuild and a release. This
// handler reads the same files from nginx, and a new zone is a file plus a
// name in the nginx whitelist plus `nginx -s reload`.
//
// The response shape is deliberately identical to that handler, because
// consumers depend on it:
//   * strong ETag `"sha256:<hex of the zone file>"` — the EU pool refresh
//     scripts discard a body whose ETag is not its own sha256
//     (`frkn-geo-ai4-refresh.sh`), and routers keep the ETag for
//     If-None-Match;
//   * one ETag and one Last-Modified (the zone file's mtime) for all three
//     assets of a zone — `.zone`, `.zone.sha256` and `.zone.count` are the
//     same generation;
//   * `.count` is counted from the file, not read from `<zone>.count`: that
//     file is for operators, `ru` has none, and a stale sidecar must never
//     report a generation the zone does not have;
//   * 304 carries only the validators and the cache policy;
//   * a file that is empty, truncated or above the size cap is refused with
//     503 (`Cache-Control: no-store`), same as the api: handing a consumer a
//     half list routes the wrong prefixes.

import fs from 'fs';
import crypto from 'crypto';

const GEO_DIR = '/var/lib/frkn/geo/';
const MAX_ZONE_BYTES = 4 * 1024 * 1024;
const CACHE_CONTROL = 'public, max-age=3600';

// Minimum CIDR count per zone. Mirrors ZONES/`min_lines` in geo.rs: a floor
// catches an empty file or an HTML error page saved under a zone name, while
// the publisher's own ratio guards defend real generations. Only `ru` is
// large; every verified zone is a few dozen `/32`s.
const MIN_RU_LINES = 1000;
const DEFAULT_MIN_LINES = 8;

// Names accepted from the path. The nginx location whitelists the zone names;
// this is the second gate, so a misconfigured location still cannot join a
// path onto GEO_DIR.
const RESOURCE = /^[a-z0-9]+\.zone(?:\.sha256|\.count)?$/;

const CIDR = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})\/(\d{1,2})$/;

function plain(r, status, body) {
    r.headersOut['Content-Type'] = 'text/plain; charset=utf-8';
    // Errors are never cached: a consumer that cached a 503 would keep a
    // stale list until the entry expires.
    r.headersOut['Cache-Control'] = 'no-store';
    r.return(status, body);
}

function unavailable(r, why) {
    r.error('geo: refusing to serve ' + why);
    return plain(r, 503, 'geo list unavailable\n');
}

// `ai4.zone` -> zone, `ai4.zone.sha256` -> sha256, `ai4.zone.count` -> count.
function parseResource(resource) {
    if (resource.endsWith('.zone.sha256')) {
        return [resource.slice(0, -'.sha256'.length), 'sha256'];
    }
    if (resource.endsWith('.zone.count')) {
        return [resource.slice(0, -'.count'.length), 'count'];
    }
    if (resource.endsWith('.zone')) {
        return [resource, 'zone'];
    }
    return null;
}

function isValidCidr(line) {
    const m = CIDR.exec(line);
    if (m === null) {
        return false;
    }
    for (let i = 1; i <= 4; i++) {
        if (parseInt(m[i], 10) > 255) {
            return false;
        }
    }
    return parseInt(m[5], 10) <= 32;
}

// RFC 9110 §13.1.2: the header may list several validators, `*` matches any
// representation and comparison is weak (a `W/` prefix is ignored).
function ifNoneMatchHits(header, etag) {
    if (!header) {
        return false;
    }
    const wanted = etag.replace(/^W\//, '');
    return header.split(',').some(function (candidate) {
        const value = candidate.trim();
        return value === '*' || value.replace(/^W\//, '') === wanted;
    });
}

// njs 0.8 hands back `mtime` as a Date; keep the numeric shapes working too so
// a module upgrade cannot silently drop Last-Modified.
function lastModified(st) {
    let date;
    if (st.mtime instanceof Date) {
        date = st.mtime;
    } else if (typeof st.mtimeMs === 'number') {
        date = new Date(st.mtimeMs);
    } else {
        date = new Date(Number(st.mtime) * 1000);
    }
    // `toUTCString()` is IMF-fixdate, the only format HTTP senders must use.
    return date.toUTCString();
}

function serve(r) {
    if (r.method !== 'GET' && r.method !== 'HEAD') {
        return plain(r, 405, 'HTTP method not allowed');
    }

    const resource = r.uri.slice('/router/geo/'.length);
    const parsed = RESOURCE.test(resource) ? parseResource(resource) : null;
    if (parsed === null) {
        return plain(r, 404, 'not found\n');
    }

    const name = parsed[0];
    const asset = parsed[1];
    const minLines = name === 'ru.zone' ? MIN_RU_LINES : DEFAULT_MIN_LINES;
    const path = GEO_DIR + name;

    let st;
    try {
        st = fs.statSync(path);
    } catch (e) {
        if (String(e).indexOf('No such file') !== -1 || String(e).indexOf('ENOENT') !== -1) {
            // Known zone, timer has not published it yet.
            return plain(r, 404, name + ' not published\n');
        }
        r.error('geo: stat ' + path + ' failed: ' + e);
        return plain(r, 500, 'geo list unavailable\n');
    }

    if (st.size > MAX_ZONE_BYTES) {
        return unavailable(r, path + ' (' + st.size + ' bytes is above the cap)');
    }
    if (st.size === 0) {
        return unavailable(r, path + ' (empty file)');
    }

    let bytes;
    try {
        bytes = fs.readFileSync(path);
    } catch (e) {
        r.error('geo: reading ' + path + ' failed: ' + e);
        return plain(r, 500, 'geo list unavailable\n');
    }

    const lines = bytes.toString('utf8').split('\n');
    const count = validate(lines, minLines, r, name);
    if (count === null) {
        return unavailable(r, name + ' (failed the shape/floor check)');
    }

    const sha256 = crypto.createHash('sha256').update(bytes).digest('hex');
    const etag = '"sha256:' + sha256 + '"';
    const modified = lastModified(st);

    r.headersOut['ETag'] = etag;
    r.headersOut['Last-Modified'] = modified;
    r.headersOut['Cache-Control'] = CACHE_CONTROL;

    if (ifNoneMatchHits(r.headersIn['If-None-Match'], etag)) {
        // `r.return(304)` would add `Content-Length: 0` and a content type;
        // the api's 304 carries only the validators and the cache policy, so
        // send the headers as they are. `default_type ""` in the nginx
        // location keeps nginx from filling in a content type.
        r.status = 304;
        r.sendHeader();
        return r.finish();
    }

    let body;
    if (asset === 'sha256') {
        body = Buffer.from(sha256 + '  ' + name + '\n');
    } else if (asset === 'count') {
        body = Buffer.from(String(count) + '\n');
    } else {
        body = bytes;
    }

    r.headersOut['Content-Type'] = 'text/plain; charset=utf-8';
    r.headersOut['Content-Length'] = String(body.length);

    // HEAD: same call as GET — nginx keeps the headers (Content-Length
    // included) and drops the body, which is exactly what the api did.
    return r.return(200, body);
}

// Same rules as `validate_zone` in geo.rs: one IPv4 CIDR per line, no blanks,
// at least `minLines` entries, CRLF and stray whitespace tolerated. Returns the
// CIDR count on success and `null` when the file must not be served.
function validate(lines, minLines, r, name) {
    if (lines.length > 0 && lines[lines.length - 1] === '') {
        // `str::lines()` does not yield the empty string after a final \n.
        lines.pop();
    }
    let count = 0;
    for (let i = 0; i < lines.length; i++) {
        const line = lines[i].trim();
        if (line === '') {
            r.error('geo: ' + name + ' line ' + (i + 1) + ' is empty');
            return null;
        }
        if (!isValidCidr(line)) {
            r.error('geo: ' + name + ' line ' + (i + 1) + ' is not an IPv4 CIDR');
            return null;
        }
        count++;
    }
    if (count < minLines) {
        r.error('geo: ' + name + ' has ' + count + ' CIDRs, expected at least ' + minLines);
        return null;
    }
    return count;
}

export default { serve };
