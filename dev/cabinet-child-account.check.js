// Cabinet sanity check: runs the real inline script of a subscription page in
// jsdom with a stubbed api, and asserts what a user account (is_child) sees.
//
// jsdom is not a dependency of either repo: install it in a scratch dir and
// point NODE_PATH at it.
//
//   mkdir -p /tmp/cab-check && (cd /tmp/cab-check && npm i jsdom)
//   NODE_PATH=/tmp/cab-check/node_modules node dev/cabinet-child-account.check.js \
//       ../frkn.org/subscription/index.html ru
//   NODE_PATH=/tmp/cab-check/node_modules node dev/cabinet-child-account.check.js \
//       ../frkn.org/en/subscription/index.html en
//
// No network: every fetch is answered from the fixture below. The point is the
// rendering contract — badge shown, install-token CTA and selfhosted blocks
// gone — not the api itself.

const fs = require("fs");
const path = require("path");
const { JSDOM } = require("jsdom");

const file = process.argv[2];
const lang = (process.argv[3] || "ru").toLowerCase();
const EXPECTED_LABEL = lang === "en" ? "User account" : "Пользователь";
const EXPECTED_ID = "11111111-2222-3333-4444-555555555555";
const PARENT_ID = "99999999-8888-7777-6666-555555555555";

const html = fs.readFileSync(file, "utf8");
const clientsJson = fs.readFileSync(
  path.join(path.dirname(file), "vpn-clients.json"),
  "utf8",
);

function fixture(opts) {
  return {
    id: EXPECTED_ID,
    expires: null,
    days: 0,
    ref_code: "abc123",
    locations: [],
    downlink: 0,
    uplink: 0,
    daily_downlink: 0,
    daily_uplink: 0,
    monthly_downlink: 0,
    monthly_uplink: 0,
    limit_bytes: 0,
    plan_kind: opts.selfhosted ? "selfhosted" : "standard",
    product: "standard",
    used_bytes: 0,
    remaining_bytes: null,
    env_traffic: [],
    connections: [],
    plans: [
      { plan: "standard", title: "Standard", active: false, expires_at: null, traffic_used: 0, traffic_limit: null, purchasable: true },
    ],
    shared_subs: [],
    scope: null,
    is_child: !!opts.isChild,
    parent_id: opts.isChild ? PARENT_ID : null,
  };
}

function ok(body) {
  return {
    ok: true,
    status: 200,
    json: async () => body,
    text: async () => JSON.stringify(body),
    headers: { get: () => "application/json" },
  };
}
function notFound() {
  return {
    ok: false,
    status: 404,
    json: async () => ({}),
    text: async () => "",
    headers: { get: () => null },
  };
}

async function run(pageOpts) {
  const vc = new (require("jsdom").VirtualConsole)();
  vc.on("jsdomError", (e) => console.log("  [jsdomError]", e.message));
  vc.on("error", (...a) => console.log("  [console.error]", ...a));
  const dom = new JSDOM(html, {
    url: `https://frkn.org/subscription/?id=${EXPECTED_ID}`,
    runScripts: "outside-only",
    pretendToBeVisual: true,
    virtualConsole: vc,
  });
  const { window } = dom;

  window.fetch = async (url) => {
    const u = String(url);
    if (u.includes("vpn-clients.json")) {
      return {
        ok: true,
        status: 200,
        json: async () => JSON.parse(clientsJson),
        text: async () => clientsJson,
      };
    }
    if (/\/subscription\/[0-9a-f-]{36}$/.test(u)) return ok(fixture(pageOpts));
    if (u.includes("/private/state"))
      return ok({ response: { servers: { used: 0, limit: 0, remaining: 0 }, users: { used: 0, limit: 0, remaining: 0 } } });
    // 404 here: a 200 makes the page jump to the short URL (s.frkn.org/xxxxx)
    // and skip rendering altogether.
    if (u.includes("/short?")) return notFound();
    if (u.includes("/referrals")) return ok({ response: { referrals: [] } });
    if (u.includes("/subscription/trial")) return ok({});
    if (u.includes("/devices")) return ok({ response: { devices: [] } });
    if (u.includes("/account")) return ok({ response: {} });
    if (u.includes("/healthcheck")) return notFound();
    return notFound();
  };
  // Canvas/QRCode and chart libs are CDN scripts, not loaded here.
  window.QRCode = { toCanvas: async () => {} };
  window.HTMLCanvasElement.prototype.getContext = () => null;
  window.matchMedia =
    window.matchMedia ||
    (() => ({ matches: false, addListener() {}, removeListener() {}, addEventListener() {}, removeEventListener() {} }));

  const scripts = [...dom.window.document.querySelectorAll("script")].filter(
    (s) => !s.src && s.textContent.trim(),
  );
  for (const s of scripts) window.eval(s.textContent);
  window.dispatchEvent(new window.Event("DOMContentLoaded"));
  // Let the fetch/await chain of the page settle.
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 10));
  }

  const doc = window.document;
  const pill = doc.getElementById("user-account-pill");
  const cta = doc.getElementById("openNodeInstall");
  const box = doc.getElementById("node-install");
  if (!pill || !cta || !box) throw new Error("cabinet markup missing (pill/CTA/box)");

  return {
    pillHidden: pill.classList.contains("hidden"),
    pillText: pill.textContent.trim(),
    ctaHidden: cta.style.display === "none",
    boxHidden: box.style.display === "none",
    refHidden: doc.getElementById("openReferralModal")
      ? doc.getElementById("openReferralModal").style.display === "none"
      : null,
    plansHidden: doc.getElementById("plans-card")
      ? doc.getElementById("plans-card").classList.contains("hidden")
      : null,
    connVisible: (() => {
      const el = doc.querySelector(".connection-link-section");
      if (!el) return null;
      return !el.classList.contains("hidden") && el.style.display !== "none";
    })(),
    mainVisible: doc.getElementById("main-content")
      ? doc.getElementById("main-content").style.display !== "none"
      : null,
  };
}

(async () => {
  let failures = 0;
  const check = (cond, msg) => {
    if (cond) {
      console.log(`  ok   ${msg}`);
    } else {
      console.log(`  FAIL ${msg}`);
      failures += 1;
    }
  };

  console.log(`[${lang}] child account`);
  const child = await run({ isChild: true });
  check(child.mainVisible === true, "page rendered (main content visible)");
  check(child.pillHidden === false, "user-account badge is shown");
  check(child.pillText === EXPECTED_LABEL, `badge reads "${EXPECTED_LABEL}" (got "${child.pillText}")`);
  check(child.ctaHidden === true, "install-token CTA (Своя нода) is hidden");
  check(child.boxHidden === true, "node-install block is hidden");
  check(child.refHidden === true, "referral button is hidden (paid block)");
  check(child.plansHidden === true, "plans card stays hidden (nothing to buy)");
  check(child.connVisible === true, "connection link stays visible (that is what a user is here for)");

  console.log(`[${lang}] selfhosted owner (regression)`);
  const owner = await run({ isChild: false, selfhosted: true });
  check(owner.pillHidden === true, "badge is hidden for the owner account");
  check(owner.ctaHidden === false, "owner keeps the install-token CTA");
  check(owner.boxHidden === false, "owner keeps the node-install block");

  console.log(`[${lang}] regular subscription`);
  const plain = await run({ isChild: false });
  check(plain.pillHidden === true, "badge is hidden for a regular subscription");
  check(plain.ctaHidden === false, "install-token CTA stays available");

  console.log(failures === 0 ? "RESULT: PASS" : `RESULT: FAIL (${failures})`);
  process.exit(failures === 0 ? 0 : 1);
})().catch((e) => {
  console.error("harness error:", e);
  process.exit(2);
});
