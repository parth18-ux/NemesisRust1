// Browser smoke test of the whole website: the app shell (W2), the dashboard and alert views (W3), the network view (W4), the
// transaction and investigation views (W5), the assistant and report views (W6), the scam and behavioural views (W7) and the W8 audit
// (review actions, server-side alert search, demo labels, every route's console, CSP, accessibility and layout), against a running
// rusetect-server in demo mode (see the Website section of README.md; `npm ci` here installs the pinned Playwright):
//   RUSETECT_DEMO=1 RUSETECT_RATE_LIMIT=100000 rusetect-server --port 8011
//   BASE=http://localhost:8011 VIEWPORT=desktop node tests/e2e/smoke.js      (VIEWPORT: desktop 1440, tablet 834, phone 390)
// (the report and screenshot steps send more than the default 240 requests per 10 s from one client, so raise the limit)
// Uses Playwright (Apache-2.0) and Chromium. SHOTS=1 also writes screenshots of every view at desktop, tablet and phone width to
// docs/screenshots/.
const {chromium} = require("playwright");
const path = require("path");

const BASE = process.env.BASE || "http://localhost:8011";
const SHOTS = process.env.SHOTS === "1";
const VIEWPORTS = {desktop: [1440, 900], tablet: [834, 1112], phone: [390, 844]};
const VIEWPORT = process.env.VIEWPORT || "desktop";
const SIZES = [[1440, 900, "desktop"], [834, 1112, "tablet"], [390, 844, "phone"]];
const OUT = path.join(__dirname, "..", "..", "docs", "screenshots");
const results = [];

async function check(name, fn) {
  const t0 = Date.now();
  try { await fn(); results.push(["PASS", name, `${Date.now() - t0} ms`]); } catch (e) { results.push(["FAIL", name, e.message.split("\n")[0]]); }
}
function assert(c, msg) { if (!c) throw new Error(msg); }

(async () => {
  const started = Date.now();
  if (!VIEWPORTS[VIEWPORT]) throw new Error("VIEWPORT must be one of " + Object.keys(VIEWPORTS));
  const browser = await chromium.launch();
  const [vw, vh] = VIEWPORTS[VIEWPORT];
  const ctx = await browser.newContext({viewport: {width: vw, height: vh}, deviceScaleFactor: 2});
  // CSP violations are recorded in the page (the browser also logs them to the console as errors)
  await ctx.addInitScript(() => { window.__csp = []; document.addEventListener("securitypolicyviolation", (e) => window.__csp.push(`${e.violatedDirective} ${e.blockedURI}`)); });
  const page = await ctx.newPage();
  const errors = [];
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  page.on("pageerror", (e) => errors.push(e.message));
  // narrow layouts hide the section menu behind the menu button
  const openMenu = async () => { if (await page.locator("#menu").isVisible()) { if ((await page.getAttribute("#menu", "aria-expanded")) !== "true") await page.click("#menu"); } };

  await check("dashboard renders from the API with no console error", async () => {
    await page.goto(BASE + "/app#/dashboard");
    await page.waitForSelector(".kpi", {timeout: 15000});
    const kpis = await page.locator(".kpi").count();
    assert(kpis >= 8, `${kpis} KPI cards`);
    assert(await page.locator("text=Model and API health").count() === 1, "health card");
    assert(await page.locator("text=Detection performance").count() === 1, "detection card");
    assert(await page.locator(".kpi .v.na").count() >= 1, "confirmed fraud shown as not available");
    await page.waitForTimeout(300);
    assert(errors.length === 0, "console errors: " + errors.join(" | "));
  });

  await check("every section link routes; unbuilt sections say so", async () => {
    const links = await page.locator('#nav a[href^="#/"]').evaluateAll((as) => as.map((a) => [a.getAttribute("href"), a.textContent.trim()]));
    assert(links.length === 10, `${links.length} section links`);
    for (const [href] of links) {
      await openMenu();
      await page.click(`#nav a[href="${href}"]`);
      await page.waitForFunction((h) => location.hash === h && document.querySelector('#nav a[aria-current="page"]')?.getAttribute("href") === h, href);
      await page.waitForSelector("#main > *:not([role=status])", {timeout: 10000});
      const built = ["#/dashboard", "#/transactions", "#/scam", "#/behaviour", "#/network", "#/alerts", "#/investigations", "#/assistant", "#/reports", "#/pilot"].includes(href);
      if (!built) assert(await page.locator("#main >> text=coming in a later stage").count() === 1, `${href} has no later-stage note`);
    }
    for (const href of ["/", "/console", "/investigate", "/report", "/docs"]) {
      const r = await page.request.get(BASE + href);
      assert(r.status() === 200, `${href} returned ${r.status()}`);
    }
    assert(errors.length === 0, "console errors: " + errors.join(" | "));
  });

  await check("pilot health: drift PSI, alert volume per day and latency come from the API", async () => {
    // a demo server may have no webhook decision yet, so the latency card can show its empty state
    await page.goto(BASE + "/app#/pilot");
    await page.waitForSelector("#pl-drift, #pl-latency", {timeout: 15000});
    assert(await page.locator("#pl-drift tbody tr").count() >= 1, "PSI rows");
    assert(await page.locator("#pl-drift >> text=score").count() >= 1, "score PSI row");
    assert(await page.locator("#pl-volume").count() === 1, "volume card");
    assert(await page.locator("#pl-latency >> text=Server latency p50").count() === 1, "latency card");
    assert(await page.locator("#main >> text=not yet measured").count() === 1, "accuracy caveat");
    const over = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
    assert(over <= 0, `page scrolls sideways by ${over}px`);
    assert(errors.length === 0, "console errors: " + errors.join(" | "));
  });

  // ---- network intelligence (W4). The Cytoscape instance is read from its container (_cyreg) to find where an
  // edge is drawn, so the test clicks the real canvas.
  const cyEval = (fn, arg) => page.evaluate(([f, a]) => new Function("cy", "a", "return (" + f + ")(cy, a)")(document.querySelector("#nw-graph")._cyreg.cy, a), [fn.toString(), arg]);
  const counts = () => page.textContent("#nw-counts");

  await check("network: empty state is labelled and loads the demonstration network", async () => {
    await page.goto(BASE + "/app#/network");
    await page.waitForSelector("#main section.card");
    assert(await page.locator("text=A connection is not evidence of wrongdoing.").count() === 1, "disclaimer caption");
    if (await page.locator("#nw-seed").count()) {
      assert(await page.locator("text=No stored payment yet").count() === 1, "empty state text");
      await page.click("#nw-seed");
    }
    await page.waitForFunction(() => document.querySelector("#nw-graph")?._cyreg?.cy?.edges(".pay").length > 0, null, {timeout: 60000});
    const [nodes, pays] = await cyEval((cy) => [cy.nodes(".account").length, cy.edges(".pay").length]);
    assert(nodes >= 2 && pays >= 9, `${nodes} accounts, ${pays} payments`);
    assert(/account=MULE01/.test(page.url()), "deep link written: " + page.url());
    const na = await page.textContent(".na");
    assert(/Not available in this data/.test(na) && /devices/.test(na) && /customers and merchants/.test(na), na);
    assert(await page.locator(".banner.demo").count() === 1, "demo banner");
  });

  await check("network: search focuses a node and opens its details", async () => {
    await page.fill("#nw-search", "COLL01");
    await page.press("#nw-search", "Enter");
    await page.waitForFunction(() => document.querySelector("#nw-panel h3")?.textContent.includes("COLL01"));
    const sel = await cyEval((cy) => cy.$("node:selected").map((n) => n.id()));
    assert(sel.length === 1 && sel[0] === "account:0300:COLL01", "selected: " + sel);
    const p = await page.textContent("#nw-panel");
    assert(/Money in/.test(p) && /MULE01/.test(p) && /pass through/.test(p), "panel: " + p.slice(0, 200));
    assert(await page.locator('#nw-panel a[href^="#/alerts/"]').count() > 0, "alert links");
  });

  await check("network: clicking an edge shows its evidence, linked to the transaction", async () => {
    await page.locator("#nw-graph").scrollIntoViewIfNeeded();
    await page.waitForTimeout(400); // let the previous test's centring animation finish
    const box = await page.locator("#nw-graph").boundingBox();
    const pt = await cyEval((cy) => { const e = cy.edges(".pay").filter((x) => x.source().id() !== x.target().id())[0]; cy.fit(e.connectedNodes(), 60); return e.renderedMidpoint(); });
    await page.mouse.click(box.x + pt.x, box.y + pt.y);
    await page.waitForSelector("#nw-ev-txn a", {timeout: 5000});
    const href = await page.getAttribute("#nw-ev-txn a", "href");
    assert(/^#\/transactions\/./.test(href), "in-app link: " + href);
    const r = await page.request.get(BASE + "/api/v2/transactions/" + href.slice(15));
    assert(r.status() === 200, `${href} returned ${r.status()}`);
  });

  await check("network: minimum amount and direction filters change the counts", async () => {
    const before = await counts();
    await page.fill("#nw-min", "15000");
    await page.dispatchEvent("#nw-min", "change");
    const after = await counts();
    assert(after !== before, `${before} -> ${after}`);
    const pays = await cyEval((cy) => cy.edges(".pay").map((e) => e.id()));
    assert(pays.length > 0 && pays.length < 9, `${pays.length} payments at 15000 or more`);
    await page.fill("#nw-min", "0");
    await page.dispatchEvent("#nw-min", "change");
    assert(await counts() === before, "reset");
    await page.selectOption("#nw-dir", "out");
    assert(await counts() !== before, "direction did not change the counts");
    await page.selectOption("#nw-dir", "all");
  });

  await check("network: expand, depth and deep link", async () => {
    await page.goto(BASE + "/app#/network?account=COLL01&bank=0300");
    await page.waitForFunction(() => document.querySelector("#nw-graph")?._cyreg?.cy?.nodes(".account").length > 0);
    const n1 = await cyEval((cy) => cy.nodes(".account").length);
    await page.click('#nw-panel button[data-node="account:0200:MULE01"]');
    await page.click('#nw-panel [data-expand="account:0200:MULE01"]');
    await page.waitForFunction((n) => document.querySelector("#nw-graph")._cyreg.cy.nodes(".account").length > n, n1, {timeout: 10000});
    await page.selectOption("#nw-depth", "2");
    await page.waitForFunction(() => /depth=2/.test(location.hash));
  });

  await check("network: View in network from the alert drawer", async () => {
    await page.goto(BASE + "/app#/alerts");
    await page.waitForSelector("#al-table tbody [data-open]");
    await page.locator("#al-table [data-open]").first().click();
    await page.click("#al-drawer >> text=View in network >> nth=1");
    await page.waitForFunction(() => location.hash.startsWith("#/network?account=") && document.querySelector("#nw-graph")?._cyreg?.cy?.nodes().length > 0);
    assert(await page.locator("#al-drawer").count() === 0, "drawer closed");
  });

  await check("no console error in the network view", async () => {
    assert(errors.length === 0, errors.join(" | "));
  });

  await check("alerts: demonstration alerts load in demo mode", async () => {
    await page.goto(BASE + "/app#/alerts");
    await page.waitForSelector("#al-table table");
    if (await page.locator("#al-seedbox:not([hidden])").count()) {
      await page.click("#al-seed");
    }
    await page.waitForSelector("#al-table tbody [data-open]", {timeout: 60000});
    assert(await page.locator(".banner.demo >> text=Demonstration data").count() === 1, "demo banner");
  });

  // the alert list is searched, filtered, sorted and paged by the server: wait for the response each change asks for
  const alertsReq = (fn, re = /\/api\/v2\/alerts\?/) => Promise.all([page.waitForResponse((r) => re.test(r.url())), fn()]);
  await check("alerts: server-side search, band, status, state, priority and assignee filters", async () => {
    const [r] = await alertsReq(() => page.fill("#al-q", "COLL01"), /\/api\/v2\/alerts\?.*q=COLL01/);
    assert(r.status() === 200, "search status " + r.status());
    await page.waitForFunction(() => [...document.querySelectorAll("#al-table tbody tr td:nth-child(6)")].every((t) => t.textContent.includes("COLL01")));
    const ents = await page.locator("#al-table tbody tr td:nth-child(6)").allTextContents();
    assert(ents.length > 0, "no search hit");
    assert(/filtered from/.test(await page.textContent("#al-pager")), "pager shows the unfiltered count");
    await alertsReq(() => page.fill("#al-q", "no-such-alert-zz"));
    await page.waitForSelector("#al-table >> text=No alert matches these filters.");
    await alertsReq(() => page.fill("#al-q", ""));
    await alertsReq(() => page.selectOption("#al-band", "verify"));
    const bands = await page.locator("#al-table tbody tr td:nth-child(3) .pill").allTextContents();
    assert(bands.length > 0 && bands.every((t) => t === "verify"), "band filter: " + bands.join(","));
    await alertsReq(() => page.selectOption("#al-band", ""));
    for (const [sel, v] of [["#al-status", "open"], ["#al-state", "new"], ["#al-prio", "none"], ["#al-assignee", "unassigned"]]) {
      const [x] = await alertsReq(() => page.selectOption(sel, v));
      assert(x.status() === 200 && (await page.locator("#al-table tbody tr[data-id]").count()) > 0, `${sel}=${v}`);
      await alertsReq(() => page.selectOption(sel, ""));
    }
    const api = await (await page.request.get(BASE + "/api/v2/alerts?q=COLL01&limit=1")).json();
    assert(api.alerts.length === 1 && api.total >= 1 && typeof api.all === "number", "paged API form");
    assert(Array.isArray(await (await page.request.get(BASE + "/api/v2/alerts")).json()), "no parameter: the plain array as before");
  });

  await check("alerts: sort by every column, risk both ways, server-side", async () => {
    // the response arrives before the page redraws: wait for the header state, then for the rows in that order
    const sorted = (k, dir) => page.waitForFunction(([k, dir]) => document.querySelector(`th:has([data-sort="${k}"])`)?.getAttribute("aria-sort") === dir, [k, dir]);
    const ordered = (desc) => page.waitForFunction((d) => { const s = [...document.querySelectorAll("#al-table tbody tr td:nth-child(3) .num")].map((x) => Number(x.textContent));
      return s.length > 0 && s.every((v, i) => i === 0 || (d ? s[i - 1] >= v : s[i - 1] <= v)); }, desc, {timeout: 5000})
      .catch(async () => { throw new Error(`${desc ? "descending" : "ascending"}: ${await page.locator("#al-table tbody tr td:nth-child(3) .num").allTextContents()}`); });
    await alertsReq(() => page.click('[data-sort="score"]'), /sort=score&order=desc/);
    await sorted("score", "descending");
    await ordered(true);
    await alertsReq(() => page.click('[data-sort="score"]'), /sort=score&order=asc/);
    await sorted("score", "ascending");
    await ordered(false);
    for (const k of ["id", "source", "typology", "entity", "txn", "assignee", "status", "prio", "time"]) {
      const [r] = await alertsReq(() => page.click(`[data-sort="${k}"]`), new RegExp(`sort=${k === "prio" ? "priority" : k}&`));
      assert(r.status() === 200, "sort " + k);
      await page.waitForFunction((k) => document.querySelector(`th:has([data-sort="${k}"])`)?.getAttribute("aria-sort") !== "none", k);
    }
    assert(await page.locator("#al-table tbody tr[data-id]").count() > 0, "rows after sorting");
  });

  await check("alerts: demo rows are labelled per alert, and the banner counts them", async () => {
    const rows = await page.locator("#al-table tbody tr[data-id]").count();
    assert(rows > 0 && await page.locator("#al-table tbody tr[data-id] .p-demo").count() === rows, "every demo row has its Demo label");
    assert(/marked Demo/.test(await page.textContent("#al-demo")) && await page.locator("#al-demo").isVisible(), "banner");
    const all = await (await page.request.get(BASE + "/api/v2/alerts")).json();
    assert(all.every((a) => a.demo === true), "/assess alerts on a demo instance carry demo: true");
    // the signed webhook's alerts are a bank's events; tests/intel.rs checks they carry demo: false
    await alertsReq(() => page.click("#al-refresh"));
    await page.check("#al-all");
    assert(/Create investigation \(\d+\)/.test(await page.textContent("#al-case")), "select all on the page");
    await page.uncheck("#al-all");
    assert(await page.locator("#al-prev[disabled]").count() === 1, "first page: Previous disabled");
  });

  await check("alerts: the drawer's URL is the alert's deep link, and the link opens it", async () => {
    const id = await page.locator("#al-table [data-open]").first().textContent();
    await page.locator("#al-table [data-open]").first().click();
    await page.waitForSelector("#al-drawer");
    assert(await page.evaluate(() => location.hash) === "#/alerts/" + id, "hash " + await page.evaluate(() => location.hash));
    await page.keyboard.press("Escape");
    assert(await page.evaluate(() => location.hash) === "#/alerts", "hash after close");
    await page.goto(BASE + "/app?deep=1#/alerts/" + id);
    await page.waitForSelector(`#al-drawer h2:has-text("${id}")`);
    await page.keyboard.press("Escape");
  });

  await check("alerts: drawer opens with details and closes on Escape", async () => {
    await page.locator("#al-table [data-open]").first().click();
    await page.waitForSelector("#al-drawer[role=dialog]");
    assert(await page.locator("#al-drawer >> text=Model reasons").count() === 1, "reasons section");
    await page.keyboard.press("Escape");
    assert(await page.locator("#al-drawer").count() === 0, "drawer closed");
  });

  await check("alerts: triage without a key is refused with a clear 401 message", async () => {
    await page.selectOption("#who-key", "");
    await page.waitForSelector("#al-table tbody [data-open]");
    await page.locator("#al-table [data-open]").first().click();
    await page.selectOption("#al-triage [name=priority]", "low");
    await page.click("#al-triage [type=submit]");
    await page.waitForSelector("#al-tmsg .msg.err");
    const t = await page.textContent("#al-tmsg");
    assert(t.includes("Sign-in needed"), t);
    await page.keyboard.press("Escape");
  });

  await check("alerts: a viewer gets a clear 403 message", async () => {
    const opt = await page.locator("#who-key option", {hasText: "(viewer)"}).getAttribute("value");
    await page.selectOption("#who-key", opt);
    await page.waitForSelector("#al-table tbody [data-open]");
    await page.locator("#al-table [data-open]").first().click();
    await page.selectOption("#al-triage [name=priority]", "low");
    await page.click("#al-triage [type=submit]");
    await page.waitForSelector("#al-tmsg .msg.err");
    const t = await page.textContent("#al-tmsg");
    assert(t.includes("Not allowed for this role"), t);
    await page.keyboard.press("Escape");
  });

  let triaged;
  await check("alerts: an analyst triages (assign, status, priority, comment) with a nonce", async () => {
    const opt = await page.locator("#who-key option", {hasText: "(analyst)"}).first();
    const [key, label] = [await opt.getAttribute("value"), await opt.textContent()];
    const me = label.split(" ")[0];
    await page.selectOption("#who-key", key);
    await page.waitForSelector("#al-table tbody [data-open]");
    const first = page.locator("#al-table [data-open]").first();
    triaged = await first.textContent();
    await first.click();
    await page.selectOption("#al-triage [name=assignee]", me);
    await page.selectOption("#al-triage [name=state]", "in_progress");
    await page.selectOption("#al-triage [name=priority]", "critical");
    await page.fill("#al-triage [name=comment]", "Smoke test: <b>escaped</b> comment");
    await page.click("#al-triage [type=submit]");
    await page.waitForSelector("#al-tmsg .msg.ok", {timeout: 10000});
    assert(await page.locator("#al-drawer .log b", {hasText: "escaped"}).count() === 0, "comment HTML was not escaped");
    const a = (await (await page.request.get(BASE + "/api/v2/alerts")).json()).find((x) => x.alert_id === triaged);
    assert(a.workflow.priority === "critical" && a.workflow.assignee === me && a.workflow.state === "in_progress", JSON.stringify(a.workflow));
    await page.keyboard.press("Escape");
    const row = page.locator(`#al-table tr[data-id="${triaged}"]`);
    assert((await row.textContent()).includes("critical"), "table row not updated");
  });

  await check("alerts: review decisions in the drawer: two analysts for a block, the same analyst cannot approve twice", async () => {
    const pick = async (role, n = 0) => { const o = page.locator("#who-key option", {hasText: `(${role})`}).nth(n);
      await page.selectOption("#who-key", await o.getAttribute("value")); await page.waitForSelector("#al-table tbody [data-open]"); return (await o.textContent()).split(" ")[0]; };
    const all = await (await page.request.get(BASE + "/api/v2/alerts?status=open")).json();
    const [blockId, holdId] = [all.at(-1).alert_id, all.at(-2).alert_id];
    const open = async (id) => { await page.goto(BASE + "/app#/alerts/" + id); await page.waitForSelector(`#al-drawer h2:has-text("${id}")`); };
    await pick("viewer");
    await open(blockId);
    assert(await page.locator("[data-review][disabled]").count() === 4 && /cannot decide alerts/.test(await page.textContent("#al-rwhy")), "viewer: decisions disabled with the reason");
    const a = await pick("analyst", 0);
    await open(blockId);
    await page.fill("#al-review [name=note]", "Smoke: first approval");
    await page.click('[data-review="approve_block"]');
    await page.waitForSelector("#al-rmsg .msg.ok", {timeout: 10000});
    assert(/block pending second approval/.test(await page.textContent("#al-rmsg")), await page.textContent("#al-rmsg"));
    assert(await page.locator('[data-review="approve_block"][disabled]').count() === 1, "approve disabled for the first approver");
    assert((await page.textContent("#al-drawer")).includes(a) && /different analyst/.test(await page.textContent("#al-rwhy")), "four-eyes rule shown");
    const b = await pick("analyst", 1);
    assert(b !== a, "two analysts");
    await open(blockId);
    await page.click('[data-review="approve_block"]');
    await page.waitForSelector("#al-rmsg .msg.ok", {timeout: 10000});
    assert(/Status is now blocked/.test(await page.textContent("#al-rmsg")), await page.textContent("#al-rmsg"));
    assert(await page.locator("[data-review][disabled]").count() === 4 && /final/.test(await page.textContent("#al-rwhy")), "a blocked alert is final");
    await open(holdId);
    await page.click('[data-review="hold"]');
    await page.waitForSelector("#al-rmsg .msg.ok", {timeout: 10000});
    await page.click('[data-review="verify"]');
    await page.waitForSelector("#al-rmsg >> text=customer verification", {timeout: 10000});
    const audit = await (await page.request.get(BASE + "/api/v2/audit")).json();
    const acts = audit.entries.filter((e) => e.type === "analyst_action" && (e.alert_id === blockId || e.alert_id === holdId)).map((e) => e.action);
    assert(audit.chain_valid && ["approve_block", "hold", "verify"].every((x) => acts.includes(x)), "ledger: " + acts);
    // the server refuses a replayed nonce and the same analyst twice, whatever the page shows
    const r = await page.request.post(BASE + `/api/v2/alerts/${holdId}/review`, {headers: {"X-Analyst-Key": await page.inputValue("#who-key"), "X-Nonce": "smoke-fixed-nonce", "content-type": "application/json"}, data: {action: "approve_block"}});
    const r2 = await page.request.post(BASE + `/api/v2/alerts/${holdId}/review`, {headers: {"X-Analyst-Key": await page.inputValue("#who-key"), "X-Nonce": "smoke-fixed-nonce", "content-type": "application/json"}, data: {action: "approve_block"}});
    assert(r.status() === 200 && r2.status() === 409, `nonce replay: ${r.status()} ${r2.status()}`);
    await page.keyboard.press("Escape");
  });

  await check("alerts: create an investigation from selected alerts", async () => {
    const free = await page.locator("#al-table tbody tr").evaluateAll((trs) => trs.filter((t) => !t.querySelector(".pill.plain.small")).map((t) => t.dataset.id));
    assert(free.length >= 2, "need two alerts outside an investigation");
    for (const id of free.slice(0, 2)) await page.check(`[data-pick="${id}"]`);
    await page.click("#al-case");
    await page.waitForSelector("dialog[open]");
    await page.click("dialog [type=submit]");
    await page.waitForFunction(() => /Investigation INV-\d+ opened/.test(document.querySelector("#toast").textContent), null, {timeout: 10000});
    await page.waitForFunction(() => /^#\/investigations\/INV-\d+$/.test(location.hash) && document.querySelector(".ws"), null, {timeout: 10000});
    const t = await page.textContent(".ws");
    assert(t.includes(free[0]) && t.includes(free[1]), "the new case lists both alerts");
  });

  await check("no console error during the alert flows", async () => {
    // the 401, 403 and 409 responses above are expected; the browser logs failed requests as errors, so ignore those only
    const real = errors.filter((e) => !/status of (401|403|409)/.test(e));
    assert(real.length === 0, real.join(" | "));
  });

  // ---- transaction intelligence and investigations (W5)
  const who = async (role, n = 0) => {
    const opt = page.locator("#who-key option", {hasText: `(${role})`}).nth(n);
    const [key, label] = [await opt.getAttribute("value"), await opt.textContent()];
    await page.selectOption("#who-key", key);
    return {key, name: label.split(" ")[0]};
  };
  const rowsText = () => page.locator("#tx-table tbody tr").allTextContents();

  await check("transactions: search, filters and paging over the stored payments", async () => {
    await page.goto(BASE + "/app#/transactions");
    await page.waitForSelector("#tx-table tbody tr a");
    const all = (await rowsText()).length;
    await page.fill("#tx-form [name=account]", "COLL01");
    await page.click("#tx-form [type=submit]");
    const settled = (re) => page.waitForFunction((src) => { const rs = [...document.querySelectorAll("#tx-table tbody tr")];
      return rs.length > 0 && rs.every((r) => new RegExp(src).test(r.textContent)); }, re.source);
    await settled(/COLL01/);
    let rows = await rowsText();
    assert(rows.length > 0 && rows.every((t) => t.includes("COLL01")), "account filter: " + rows.length);
    await page.selectOption("#tx-form [name=typology]", "pass_through");
    await page.selectOption("#tx-form [name=band]", "verify");
    await page.fill("#tx-form [name=min_amount]", "15000");
    await page.click("#tx-form [type=submit]");
    await page.waitForFunction(() => /min_amount=15000/.test(location.hash) && /typology=pass_through/.test(location.hash));
    await page.waitForFunction(() => [...document.querySelectorAll("#tx-table tbody td:nth-child(5)")].every((td) => Number(td.textContent.replace(/[^\d.]/g, "")) >= 15000));
    const amts = (await page.locator("#tx-table tbody td:nth-child(5)").allTextContents()).map((t) => Number(t.replace(/[^\d.]/g, "")));
    rows = await rowsText();
    assert(amts.length > 0 && amts.length < all && amts.every((a) => a >= 15000), "amount filter: " + amts);
    assert(rows.every((t) => /pass through/.test(t) && /verify/.test(t)), "typology and band filters");
    await page.fill("#tx-form [name=from]", "2030-01-01T00:00");
    await page.click("#tx-form [type=submit]");
    await page.waitForSelector("#tx-table >> text=No stored payment matches these filters.");
    assert(await page.textContent("#tx-pager") .then((t) => t.includes("0 payments")), "empty pager");
    await page.click("#tx-clear");
    await page.waitForFunction(() => location.hash === "#/transactions");
    await page.waitForSelector("#tx-table tbody tr a");
    assert(await page.locator("#tx-prev[disabled]").count() === 1, "first page: Previous disabled");
  });

  await check("transactions: detail shows risk, reason codes, model, related accounts, history, timeline and links", async () => {
    await page.goto(BASE + "/app#/transactions?account=COLL01");
    await page.waitForSelector("#tx-table tbody tr a");
    await page.locator("#tx-table tbody td:nth-child(2) a").first().click();
    await page.waitForSelector(".risk-box");
    const t = await page.textContent("#main");
    for (const s of ["Risk score", "Model thresholds", "Why it scored this way", "pass-through chain", "What scored it", "ctx-", "Related accounts",
      "Beneficiary history", "Timeline", "alert opened"]) assert(t.includes(s), "missing: " + s);
    assert(await page.locator('#main a[href^="#/alerts/"]').count() > 0, "alert link");
    assert(await page.locator('#main a[href^="#/network?account="]').count() >= 2, "network links");
    assert(await page.locator("#main [data-ev-verify]").count() === 1, "evidence");
    await page.click('#main a[href^="#/network?account="] >> nth=1');
    await page.waitForFunction(() => location.hash.startsWith("#/network?account=COLL01") && document.querySelector("#nw-graph")?._cyreg?.cy?.nodes().length > 0);
  });

  await check("transactions: links in the alert and network views open in the app, not as raw JSON", async () => {
    const pages = ctx.pages().length;
    await page.goto(BASE + "/app#/alerts");
    await page.waitForSelector('#al-table a[href^="#/transactions/"]');
    assert(await page.locator('#main a[href^="/api/"]').count() === 0, "raw /api links in alerts");
    await page.locator('#al-table a[href^="#/transactions/"]').first().click();
    await page.waitForSelector(".risk-box");
    await page.goto(BASE + "/app#/network?account=MULE01&bank=0200");
    await page.waitForFunction(() => document.querySelector("#nw-graph")?._cyreg?.cy?.edges(".pay").length > 0);
    await page.click(".nw-list summary");
    await page.locator("#nw-table [data-edge]").first().click();
    await page.waitForSelector("#nw-ev-txn a");
    assert(await page.locator('#main a[href^="/api/"]').count() === 0, "raw /api links in network");
    await page.click("#nw-ev-txn a");
    await page.waitForSelector(".risk-box");
    assert(ctx.pages().length === pages, "a new tab was opened");
  });

  await check("transactions: parallel payments get one aggregated edge label", async () => {
    await page.goto(BASE + "/app#/network?account=MULE01&bank=0200");
    await page.waitForFunction(() => document.querySelector("#nw-graph")?._cyreg?.cy?.edges(".pay").length > 0);
    const labels = await cyEval((cy) => cy.edges(".pay").filter((e) => e.target().id() === "account:0300:COLL01").map((e) => e.data("label")));
    assert(labels.length > 1 && labels.filter(Boolean).length === 1 && /^\d+ payments/.test(labels.find(Boolean)), JSON.stringify(labels));
  });

  let caseId;
  const ws = async (id) => { await page.goto(BASE + "/app#/investigations/" + id); await page.waitForSelector(".ws"); };
  await check("investigations: Open investigation from the alert drawer goes to the new case", async () => {
    await who("analyst");
    await page.goto(BASE + "/app#/alerts");
    await page.waitForSelector("#al-table tbody [data-open]");
    const free = await page.locator("#al-table tbody tr").evaluateAll((trs) => trs.filter((t) => !t.querySelector(".pill.plain.small")).map((t) => t.dataset.id));
    assert(free.length >= 1, "need an alert outside an investigation");
    await page.click(`[data-open="${free[0]}"]`);
    await page.click("#al-open-inv");
    await page.waitForSelector("dialog[open]");
    await page.fill("dialog [name=title]", "Smoke test <i>case</i>");
    await page.click("dialog [type=submit]");
    await page.waitForFunction(() => /^#\/investigations\/INV-\d+$/.test(location.hash) && document.querySelector(".ws"), null, {timeout: 10000});
    caseId = (await page.evaluate(() => location.hash)).split("/").pop();
    assert(await page.locator(".ws h2 i").count() === 0 && (await page.textContent("#ws-h")).includes("<i>case</i>"), "title escaped");
  });

  await check("investigations: the workspace renders every panel", async () => {
    await ws(caseId);
    for (const h of ["Alerts and their payments", "Network excerpt", "Timeline", "Audit history", "Actions", "Analyst comments", "Evidence and attachments"]) {
      assert(await page.locator(".ws h2", {hasText: h}).count() === 1, "panel: " + h);
    }
    await page.waitForFunction(() => document.querySelector("#ws-graph")?._cyreg?.cy?.nodes(".seed").length >= 2, null, {timeout: 10000});
    const t = await page.textContent(".ws");
    for (const s of ["due in", "case: opened", "payment", "ledger chain valid", "file upload is not supported", "four-eyes"]) assert(t.includes(s), "missing: " + s);
    assert(await page.locator('.ws a[href^="#/transactions/"]').count() > 0, "transaction links");
  });

  await check("investigations: an analyst adds a comment (escaped) and takes the case", async () => {
    const me = await who("analyst");
    await ws(caseId);
    await page.fill('[data-act="comment"] textarea', "Payee looks like a <b>collector</b>");
    await page.click('[data-act="comment"] [type=submit]');
    await page.waitForFunction(() => document.querySelector("#ws-comments")?.textContent.includes("<b>collector</b>"), null, {timeout: 10000});
    assert(await page.locator("#ws-comments b", {hasText: "collector"}).count() === 0, "comment HTML not escaped");
    await page.selectOption('[data-act="assign"] select', me.name);
    await page.click('[data-act="assign"] [type=submit]');
    await page.waitForFunction((n) => document.querySelector(".ws-kv dd")?.textContent.includes(n), me.name, {timeout: 10000});
    const other = await page.locator('[data-act="assign"] option', {hasText: "(analyst)"}).evaluateAll((os, n) => os.map((o) => o.value).find((v) => v !== n), me.name);
    await page.selectOption('[data-act="assign"] select', other);
    await page.click('[data-act="assign"] [type=submit]');
    await page.waitForSelector("#ws-msg .msg.err");
    assert((await page.textContent("#ws-msg")).includes("Not allowed for this role"), await page.textContent("#ws-msg"));
    assert(await page.locator(".tl", {hasText: "case: comment"}).count() === 1, "comment in the timeline");
  });

  await check("investigations: four-eyes close (A requests, approve blocked for A, allowed for B) and supervisor reopen", async () => {
    const a = await who("analyst", 0);
    await ws(caseId);
    await page.fill('[data-act="request_close"] textarea', "Reviewed: forwarding pattern documented");
    await page.click('[data-act="request_close"] [type=submit]');
    await page.waitForSelector('[data-do="approve_close"]', {timeout: 10000});
    assert(await page.locator('[data-do="approve_close"][disabled]').count() === 1, "approve not disabled for the requester");
    assert((await page.textContent("#ws-4eyes")).includes("cannot decide it"), "rule shown");
    const r = await page.request.post(BASE + `/api/v2/investigations/${caseId}/actions`, {headers: {"X-Analyst-Key": a.key, "X-Nonce": "smoke-" + Date.now(), "content-type": "application/json"},
      data: {action: "approve_close"}});
    assert(r.status() === 400 && /four-eyes/.test(await r.text()), "server allowed the requester: " + r.status());
    const b = await who("analyst", 1);
    assert(b.name !== a.name, "two analysts");
    await page.waitForSelector('[data-do="approve_close"]:not([disabled])');
    await page.click('[data-do="approve_close"]');
    await page.waitForSelector('[data-do="reopen"]', {timeout: 10000});
    assert((await page.textContent(".close-box")).includes(`approved by ${b.name}`), "approved by B");
    assert(await page.locator('[data-do="reopen"][disabled]').count() === 1, "reopen disabled for an analyst");
    await who("supervisor");
    await page.waitForSelector('[data-do="reopen"]:not([disabled])');
    await page.click('[data-do="reopen"]');
    await page.waitForSelector('[data-act="request_close"]', {timeout: 10000});
    const audit = await page.locator(".ws table").last().textContent();
    for (const s of ["request close", "approve close", "reopen"]) assert(audit.includes(s), "audit: " + s);
  });

  await check("investigations: evidence Verify shows the server's real result", async () => {
    await ws(caseId);
    await page.locator("[data-ev-verify]").first().click();
    await page.waitForSelector(".ev-out .msg");
    assert((await page.textContent(".ev-out")).includes("Verified"), await page.textContent(".ev-out"));
    const alertId = await page.locator("[data-ev]").first().getAttribute("data-ev");
    const pack = await (await page.request.get(BASE + `/api/v2/alerts/${alertId}/evidence`)).json();
    const bad = await (await page.request.post(BASE + "/api/v2/verify", {data: {signed_fields: {pack_hash: "0".repeat(64), alert_id: alertId}, signature: pack.signature.signature}})).json();
    assert(bad.valid === false, "tampered hash verified");
    // a failed check must not read as verified
    await page.route("**/api/v2/verify", (rt) => rt.fulfill({status: 200, contentType: "application/json", body: '{"valid":false}'}));
    await page.locator("[data-ev-verify]").first().click();
    await page.waitForSelector(".ev-out .msg.err");
    assert(/Not verified/.test(await page.textContent(".ev-out")) && !/\bVerified\b/.test(await page.textContent(".ev-out")), "false result shown as verified");
    await page.unroute("**/api/v2/verify");
  });

  await check("investigations: list tabs, SLA and progress", async () => {
    await page.goto(BASE + "/app#/investigations");
    await page.waitForSelector(".tabs");
    assert(await page.locator(`tr[data-case="${caseId}"]`).count() === 1, "open tab lists the reopened case");
    assert((await page.textContent(`tr[data-case="${caseId}"]`)).match(/due in|at risk|overdue/), "SLA state");
    assert(await page.locator(`tr[data-case="${caseId}"] .prog`).count() === 1, "progress");
    await page.click('.tabs a[href$="tab=closed"]');
    await page.waitForFunction(() => /tab=closed/.test(location.hash) && document.querySelector('.tabs a[aria-selected="true"]')?.textContent.startsWith("Closed"));
    assert(await page.locator(`tr[data-case="${caseId}"]`).count() === 0, "closed tab");
    await page.click('.tabs a[href$="tab=all"]');
    await page.waitForSelector(`tr[data-case="${caseId}"]`);
  });

  // ---- W6: AI-assisted investigation (H) and reports (I)
  const genDraft = async () => {
    await page.click("[data-nr-gen]");
    await page.waitForSelector(".nr-out .nr-st", {timeout: 15000});
  };
  await check("assistant: the workspace draft is typed, sourced and bannered as requiring review", async () => {
    await who("analyst");
    await ws(caseId);
    assert((await page.textContent(".nr-banner")).includes("Draft — requires analyst review. AI-assisted interpretations are not conclusions."), "banner");
    await genDraft();
    const items = await page.locator(".nr-out .nr-st[data-st]").evaluateAll((ls) => ls.map((l) => [l.className, l.querySelectorAll(".src-link").length, l.querySelector(".nb").textContent]));
    assert(items.length >= 10 && items.every(([, n]) => n >= 1), `${items.length} statements, an unsourced one: ${JSON.stringify(items.find(([, n]) => n < 1))}`);
    const kinds = new Set(items.map(([c]) => c.split(" ").find((x) => x.startsWith("nr-") && x !== "nr-st")));
    for (const k of ["nr-observed_fact", "nr-model_output", "nr-rule_finding", "nr-interpretation", "nr-analyst_conclusion"]) assert(kinds.has(k), "type missing: " + k);
    // the earlier escaped comment is an analyst conclusion and is still escaped here
    assert(await page.locator(".nr-analyst_conclusion b", {hasText: "collector"}).count() === 0, "conclusion not escaped");
    const styles = await page.locator(".nr-legend .nb").evaluateAll((bs) => bs.map((b) => getComputedStyle(b).borderStyle + getComputedStyle(b).color));
    assert(new Set(styles).size === 5, "badge styles not distinct: " + styles.join(","));
    assert((await page.textContent(".nr-out .src")).includes("optional LLM rephrasing: not enabled"), "LLM hook shown as not enabled");
  });

  await check("assistant: sources jump to the transaction, and to the ledger row in place", async () => {
    const ledger = page.locator('.nr-out a.src-link[data-focus-to^="ledger-"]').first();
    const to = await ledger.getAttribute("data-focus-to");
    const before = await page.evaluate(() => location.hash);
    await ledger.click();
    await page.waitForSelector(`[data-focus="${to}"].hl`, {timeout: 5000});
    assert(await page.evaluate(() => location.hash) === before, "an in-page source re-routed");
    const tx = page.locator('.nr-out a.src-link[href^="#/transactions/"]').first();
    const href = await tx.getAttribute("href");
    await tx.click();
    await page.waitForFunction((h) => location.hash === h && document.querySelector(".risk-box"), href, {timeout: 10000});
  });

  let acceptedSeq;
  await check("assistant: a viewer cannot accept; an analyst accepts an edited draft into a comment, in the audit", async () => {
    await who("viewer");
    await ws(caseId);
    await genDraft();
    await page.click("[data-nr-accept]");
    await page.waitForSelector(".nr-msg .msg.err");
    assert((await page.textContent(".nr-msg")).includes("Not allowed for this role"), await page.textContent(".nr-msg"));
    await who("analyst");
    await ws(caseId);
    await genDraft();
    const ta = page.locator(".nr-accept textarea");
    assert((await ta.inputValue()).startsWith("Narrative draft accepted after review (template v1"), "prefilled");
    await ta.fill("Accepted after review: pass-through via MULE01 <script>x</script>");
    await page.click("[data-nr-accept]");
    await page.waitForFunction(() => /focus=ledger-\d+/.test(location.hash) && document.querySelector(".ws"), null, {timeout: 10000});
    acceptedSeq = Number((await page.evaluate(() => location.hash)).match(/ledger-(\d+)/)[1]);
    await page.waitForSelector(`tr[data-focus="ledger-${acceptedSeq}"]`);
    assert((await page.textContent(`tr[data-focus="ledger-${acceptedSeq}"]`)).includes("comment"), "audit row is the comment");
    assert((await page.textContent("#ws-comments")).includes("<script>x</script>"), "comment shown escaped");
    const audit = await (await page.request.get(BASE + "/api/v2/audit")).json();
    const e = audit.entries.find((x) => x.seq === acceptedSeq);
    assert(e && e.case_id === caseId && e.action === "comment" && e.detail.comment_hash, "ledger entry: " + JSON.stringify(e));
    await genDraft();
    assert((await page.textContent(".nr-out")).includes("Accepted after review: pass-through via MULE01"), "the accepted text is now an analyst conclusion");
  });

  await check("assistant: #/assistant picks a case and generates its draft", async () => {
    await page.goto(BASE + "/app#/assistant/" + caseId);
    await page.waitForSelector("#as-pick");
    assert(await page.locator("#as-pick").inputValue() === caseId, "picker");
    await genDraft();
    assert(await page.locator(".nr-out .nr-st").count() >= 10, "statements");
  });

  const report = async () => { await page.goto(BASE + `/app?r=${Date.now()}#/reports/` + caseId); await page.waitForSelector("#rp-verify", {timeout: 20000}); };
  let reportVerified;
  await check("reports: the report renders every section and its verification is the server's real result", async () => {
    await page.goto(BASE + "/app#/reports");
    await page.waitForSelector(`a[href="#/reports/${caseId}"]`);
    await report();
    const t = await page.textContent(".rp");
    for (const s of ["1. Case details", "2. Timestamps", "3. Transaction references", "4. Entity relationships", "5. Model outputs and detection signals",
      "6. Evidence provenance", "7. Analyst notes", "8. Narrative draft", "9. Audit history", "10. Verification", "Accepted after review"]) assert(t.includes(s), "missing: " + s);
    const audit = await (await page.request.get(BASE + "/api/v2/audit")).json();
    assert(await page.getAttribute("[data-chain]", "data-chain") === String(audit.chain_valid), "chain state differs from the server");
    assert((await page.textContent("[data-chain]")).startsWith(audit.chain_valid ? "Chain valid" : "Not verified"), "chain text");
    const packs = await page.locator("[data-pack]").evaluateAll((ls) => ls.map((l) => [l.dataset.pack, l.dataset.valid, l.textContent]));
    for (const [id, valid, text] of packs) {
      const pack = await (await page.request.get(BASE + `/api/v2/alerts/${id}/evidence`)).json();
      const r = await (await page.request.post(BASE + "/api/v2/verify", {data: {signed_fields: {pack_hash: pack.pack_hash, alert_id: id}, signature: pack.signature.signature}})).json();
      assert(String(r.valid) === valid && text.trim().startsWith(r.valid ? "Verified" : "Not verified"), `${id}: page ${valid}, server ${r.valid}`);
    }
    reportVerified = {chain: audit.chain_valid, packs: packs.length};
    assert(await page.locator(`.rp tr:has-text("${acceptedSeq}")`).count() >= 1, "accepted comment in the audit history");
    assert(await page.locator('.rp [data-ev-dl="str"]').count() === packs.length, "STR draft link per alert");
  });

  await check("reports: a failed verify or audit call shows Not verified with the reason", async () => {
    await page.route("**/api/v2/verify", (rt) => rt.fulfill({status: 200, contentType: "application/json", body: '{"valid":false}'}));
    await page.route("**/api/v2/audit", (rt) => rt.fulfill({status: 503, contentType: "application/json", body: '{"detail":"audit store down"}'}));
    try {
    await report();
    assert(await page.getAttribute("[data-chain]", "data-chain") === "false" && (await page.textContent("[data-chain]")).includes("audit store down"), "chain failure hidden");
    assert(await page.locator('[data-pack][data-valid="true"]').count() === 0 && (await page.textContent("#rp-verify")).includes("valid: false"), "pack failure hidden");
    assert(!/\bVerified\b|Chain valid/.test(await page.textContent("#rp-verify")), "failure shown as verified");
    assert((await page.textContent("#rp-verdict")).startsWith("Not fully verified"), "headline");
    } finally {
      await page.unroute("**/api/v2/verify");
      await page.unroute("**/api/v2/audit");
    }
  });

  await check("reports: Download JSON and the STR draft download", async () => {
    await report();
    const [dl] = await Promise.all([page.waitForEvent("download"), page.click("#rp-json")]);
    const data = JSON.parse(require("fs").readFileSync(await dl.path(), "utf8"));
    assert(data.case_id === caseId && data.narrative.generated_by === "template v1" && data.verification.packs.length === reportVerified.packs, "report JSON");
    assert(data.verification.ledger_chain.ok === reportVerified.chain, "JSON verification differs");
    const [str] = await Promise.all([page.waitForEvent("download"), page.locator('.rp [data-ev-dl="str"]').first().click()]);
    const s = JSON.parse(require("fs").readFileSync(await str.path(), "utf8"));
    assert(/DRAFT/.test(s.status), "STR draft");
  });

  await check("reports: the print view hides the app chrome and prints with no console error", async () => {
    const n0 = errors.filter((e) => !/status of (401|403|400|503)/.test(e)).length;
    await report();
    await page.waitForFunction(() => document.querySelector("#rp-graph")?._cyreg?.cy?.nodes().length > 0, null, {timeout: 10000});
    await page.emulateMedia({media: "print"});
    const hidden = await page.evaluate(() => [".side", ".top", ".rp-bar", ".rp .no-print"].map((s) => getComputedStyle(document.querySelector(s)).display));
    assert(hidden.every((d) => d === "none"), "visible in print: " + hidden);
    assert(await page.locator("#rp-verify").isVisible(), "verification printed");
    const pdf = await page.pdf({format: "A4", printBackground: true});
    assert(pdf.length > 20000, `pdf ${pdf.length} bytes`);
    await page.emulateMedia({media: "screen"});
    const real = errors.filter((e) => !/status of (401|403|400|503)/.test(e));
    assert(real.length === n0, real.slice(n0).join(" | "));
  });

  await check("no console error in the transaction, investigation, assistant and report views", async () => {
    // the refused actions above (403, 400) are expected; the browser logs failed requests as errors, so ignore those only
    const real = errors.filter((e) => !/status of (401|403|400|503)/.test(e));
    assert(real.length === 0, real.join(" | "));
  });


  // ---- W7: scam intelligence (C) and behavioural intelligence (D)
  const scamCheck = async (text, extra = {}) => {
    await page.fill("#sc-input", text);
    for (const [k, v] of Object.entries(extra)) await page.fill(`#sc-form [name=${k}]`, v);
    const [resp] = await Promise.all([page.waitForResponse((r) => r.url().endsWith("/api/v2/message")), page.click("#sc-go")]);
    await page.waitForSelector(".sc-res");
    return resp.json();
  };
  let scamModel;
  await check("scam: a scam message scores high and the highlighted phrases are the model's own", async () => {
    await page.goto(BASE + "/app#/scam");
    await page.waitForSelector("#sc-form");
    scamModel = (await (await page.request.get(BASE + "/api/v2/messages?limit=1")).json()).model;
    await page.click('[data-sample="scam"]');
    const r = await scamCheck(await page.inputValue("#sc-input"), {bank: "0101", account: "W7V01"});
    assert(r.band === "high" && r.risk >= scamModel.thresholds.high, `band ${r.band} risk ${r.risk}`);
    assert(await page.getAttribute(".sc-res", "data-result") === "high" && (await page.textContent("#sc-risk")) === r.risk.toFixed(4), "shown band or risk differs");
    const marks = (await page.locator("#sc-text mark.kp").allTextContents()).map((m) => m.toLowerCase().replace(/\s+/g, " "));
    const kps = r.key_phrases.map((k) => k.toLowerCase());
    assert(marks.length >= 1 && marks.every((m) => kps.includes(m)), `marks ${marks} not in ${kps}`);
    assert((await page.locator("[data-kp]").count()) === r.key_phrases.length, "phrase chips");
    const t = await page.textContent(".sc-res");
    for (const x of [r.workflow.label, ...r.tactics.map((y) => y.label), ...r.hard_signals.map((y) => y.label), scamModel.meta.trained_on, "no version string", "0101:W7V01"]) assert(t.includes(x), "missing: " + x);
  });
  await check("scam: a benign message scores low with nothing highlighted", async () => {
    await page.click('[data-sample="benign"]');
    await page.fill("#sc-form [name=account]", "");
    const r = await scamCheck(await page.inputValue("#sc-input"));
    assert(r.band === "safe" && r.risk < scamModel.thresholds.caution, `band ${r.band} risk ${r.risk}`);
    assert(await page.locator("#sc-text mark").count() === 0 && await page.getAttribute(".sc-res", "data-result") === "safe", "benign highlighted");
  });
  await check("scam: empty, oversized, half-linked and malformed input give a clear error", async () => {
    await page.fill("#sc-input", "   ");
    await page.click("#sc-go");
    assert((await page.textContent("#sc-out .msg.err")).includes("the text is empty"), "empty");
    await page.evaluate(() => { document.querySelector("#sc-input").value = "x".repeat(5001); });
    await page.click("#sc-go");
    await page.waitForSelector("#sc-out .msg.err >> text=input was refused");
    assert((await page.textContent("#sc-out .msg.err")).includes("at most 5000"), await page.textContent("#sc-out"));
    await page.fill("#sc-input", "hello");
    await page.fill("#sc-form [name=account]", "X1");
    await page.fill("#sc-form [name=bank]", "");
    await page.click("#sc-go");
    assert((await page.textContent("#sc-out .msg.err")).includes("payer's bank"), "account without bank");
    await page.fill("#sc-form [name=account]", "");
    const bad = await page.request.post(BASE + "/api/v2/message", {headers: {"content-type": "application/json"}, data: Buffer.from("{bad")});
    assert(bad.status() === 422 && (await bad.json()).detail[0].type === "json_invalid", "malformed JSON");
  });
  await check("scam: the history lists the messages redacted and filters by band", async () => {
    await page.goto(BASE + "/app#/scam");
    await page.waitForSelector("#sc-htable");
    const t = await page.textContent("#sc-htable");
    assert(t.includes("[UPI]") && !t.includes("kyc.help@okaxis"), "not redacted");
    await page.selectOption("#sc-hf [name=band]", "high");
    await page.click("#sc-hf button");
    await page.waitForFunction(() => [...document.querySelectorAll("#sc-htable [data-band]")].every((b) => b.dataset.band === "high"));
    assert(await page.locator("#sc-htable tbody tr").count() >= 1, "no high row");
  });
  await check("behaviour: typed keystroke capture and the synthetic demo return the server's real result", async () => {
    await page.goto(BASE + "/app#/behaviour");
    await page.waitForSelector("#bh-type");
    assert(await page.locator("text=consent-based demonstration").count() >= 5, "consent labels");
    const phrase = "the quick brown fox jumps over the lazy dog then reads the paper";
    for (let i = 0; i < 3; i++) {
      await page.locator("#bh-type").pressSequentially(phrase, {delay: 25});
      await Promise.all([page.waitForResponse((r) => r.url().endsWith("/behaviour/enrol")), page.click("#bh-enrol")]);
    }
    assert((await page.textContent("#bh-tstat")).includes("3 enrolment session(s)"), await page.textContent("#bh-tstat"));
    await page.locator("#bh-type").pressSequentially(phrase, {delay: 25});
    const [resp] = await Promise.all([page.waitForResponse((r) => r.url().endsWith("/behaviour/check")), page.click("#bh-check")]);
    const r = await resp.json();
    await page.waitForSelector("[data-typing]");
    assert(r.ready === true && typeof r.score === "number", JSON.stringify(r));
    assert((await page.textContent("#bh-tout")).includes(r.score.toFixed(3)), "shown score differs");
    await page.click("#bh-synth");
    await page.waitForFunction(() => document.querySelectorAll("[data-typing]").length === 2, null, {timeout: 15000});
    const kinds = await page.locator("[data-typing]").evaluateAll((l) => l.map((x) => x.dataset.typing));
    assert(kinds[0] === "match" && kinds[1] === "anomaly", "synthetic owner/impostor: " + kinds);
  });
  await check("behaviour: PRAMAAN, identity and duress show the endpoints' outcomes", async () => {
    await page.click('[data-preset="swap"]');
    const [resp] = await Promise.all([page.waitForResponse((x) => x.url().endsWith("/v2/authenticate")), page.click("#bh-pf button[type=submit]")]);
    const r = await resp.json();
    await page.waitForSelector("[data-pramaan]");
    assert(r.decision !== "allow" && r.challenge_type !== "sms_otp" && await page.getAttribute("[data-pramaan]", "data-pramaan") === r.decision, JSON.stringify(r).slice(0, 200));
    assert((await page.textContent("#bh-pout")).includes(r.model_version), "model version");
    if (r.decision === "challenge") {
      await page.click('[data-ch][data-pass="0"]');
      await page.waitForSelector('[data-pramaan="block"]');
    }
    await page.click("#bh-if button");
    await page.waitForSelector('[data-identity="deny_otp_reverify"]');
    await page.click("#bh-dreg");
    await page.waitForSelector("#bh-dout .msg.ok");
    await page.click("#bh-df button[type=submit]");
    await page.waitForSelector('[data-duress="duress"]');
    assert((await page.textContent("#bh-dout")).includes("Payment successful"), "duress outcome");
  });
  await check("behaviour: unavailable signals are labelled and no console error", async () => {
    const mouse = await page.textContent('#bh-na [data-na="mouse"]');
    assert(/not available/.test(mouse) && /module exists, not served/.test(mouse) && /balabit_eval\.json/.test(mouse), mouse);
    assert(await page.locator("#bh-na li").count() === 3, "unavailable list");
    await page.waitForSelector("#bh-sess table, #bh-sess .empty");
    const real = errors.filter((e) => !/status of (401|403|400|422|503)/.test(e));
    assert(real.length === 0, real.join(" | "));
  });

  // ---- W8: whole-site audit. Every route at this viewport: no console error, no CSP violation, no horizontal page scroll,
  // every form control labelled, every button and link named, text contrast at least WCAG AA (an axe-like check written here:
  // axe-core is MPL-2.0, which this project does not vendor).
  const audit = () => page.evaluate(() => {
    const out = {unlabelled: [], unnamed: [], contrast: [], overflow: document.documentElement.scrollWidth - document.documentElement.clientWidth};
    const visible = (e) => { const r = e.getBoundingClientRect(); const cs = getComputedStyle(e); return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && cs.display !== "none"; };
    const named = (e) => (e.getAttribute("aria-label") || e.getAttribute("title") || e.textContent || "").trim() ||
      (e.getAttribute("aria-labelledby") || "").split(" ").map((id) => document.getElementById(id)?.textContent || "").join("").trim();
    const tag = (e) => e.tagName.toLowerCase() + (e.id ? "#" + e.id : "") + (e.name ? `[name=${e.name}]` : "") + (e.className && typeof e.className === "string" ? "." + e.className.split(" ")[0] : "");
    for (const e of document.querySelectorAll("input:not([type=hidden]), select, textarea")) {
      if (!visible(e) || e.closest(".__________cytoscape_container")) continue;
      const lab = e.getAttribute("aria-label") || e.getAttribute("aria-labelledby") || e.closest("label") || (e.id && document.querySelector(`label[for="${e.id}"]`)) || e.getAttribute("title");
      if (!lab) out.unlabelled.push(tag(e));
    }
    for (const e of document.querySelectorAll("button, a[href], [role=button]")) if (visible(e) && !named(e)) out.unnamed.push(tag(e));
    for (const e of document.querySelectorAll("img")) if (!e.hasAttribute("alt")) out.unnamed.push(tag(e));
    // contrast of every visible text run against the first opaque background behind it
    const rgb = (c) => (c.match(/[\d.]+/g) || []).map(Number);
    const lum = ([r, g, b]) => [r, g, b].map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; }).reduce((a, v, i) => a + v * [0.2126, 0.7152, 0.0722][i], 0);
    const bgOf = (e) => { for (let x = e; x; x = x.parentElement) { const c = rgb(getComputedStyle(x).backgroundColor); if (c.length >= 3 && (c[3] ?? 1) > 0.5) return c; } return [255, 255, 255]; };
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const seen = new Set();
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const e = n.parentElement;
      if (!n.textContent.trim() || seen.has(e) || !visible(e) || e.closest("[disabled], [aria-disabled=true], .__________cytoscape_container, noscript, script, style, option")) continue;
      seen.add(e);
      let op = 1; for (let x = e; x; x = x.parentElement) op *= +getComputedStyle(x).opacity;
      if (op < 1) continue; // faded (disabled) controls are exempt
      const cs = getComputedStyle(e), fg = rgb(cs.color);
      if ((fg[3] ?? 1) < 1) continue;
      const [l1, l2] = [lum(fg), lum(bgOf(e))].sort((a, b) => b - a);
      const ratio = (l1 + 0.05) / (l2 + 0.05), size = parseFloat(cs.fontSize), bold = +cs.fontWeight >= 700;
      const need = size >= 24 || (size >= 18.66 && bold) ? 3 : 4.5;
      if (ratio < need) out.contrast.push(`${tag(e)} "${n.textContent.trim().slice(0, 24)}" ${ratio.toFixed(2)} < ${need}`);
    }
    out.csp = window.__csp;
    return out;
  });
  const auditRoutes = () => ["dashboard", "transactions", `transactions/${triaged}`, "scam", "behaviour", "network?account=MULE01&bank=0200", "alerts", `alerts/${triaged}`,
    "investigations", `investigations/${caseId}`, `assistant/${caseId}`, "reports", `reports/${caseId}`, "pilot"];
  for (const theme of ["light", "dark"]) {
    await check(`audit (${theme} theme): every route is console-error-free, CSP-clean, labelled, named, AA contrast and fits the width`, async () => {
      await page.emulateMedia({colorScheme: theme});
      const bad = [];
      for (const r of auditRoutes()) {
        const n0 = errors.length;
        await page.goto(BASE + `/app?audit=${theme}#/` + r);
        await page.waitForSelector("#main > *:not([role=status])", {timeout: 15000});
        await page.waitForLoadState("networkidle");
        const a = await audit();
        const errs = errors.slice(n0);
        if (errs.length) bad.push(`${r}: console ${errs.join(" | ")}`);
        if (a.csp.length) bad.push(`${r}: CSP ${a.csp.join(", ")}`);
        if (a.overflow > 0) bad.push(`${r}: page scrolls sideways by ${a.overflow}px`);
        if (a.unlabelled.length) bad.push(`${r}: unlabelled ${a.unlabelled.join(", ")}`);
        if (a.unnamed.length) bad.push(`${r}: unnamed ${a.unnamed.join(", ")}`);
        if (a.contrast.length) bad.push(`${r}: contrast ${a.contrast.slice(0, 4).join("; ")}`);
      }
      await page.emulateMedia({colorScheme: null});
      assert(bad.length === 0, bad.join(" || "));
    });
  }

  await check("audit: the CSP header is set on the app and the legacy pages keep working", async () => {
    const r = await page.request.get(BASE + "/app");
    const csp = r.headers()["content-security-policy"] || "";
    assert(/script-src 'self'/.test(csp) && !/unsafe-inline'[^;]*script|script-src[^;]*unsafe/.test(csp), "CSP: " + csp);
    for (const href of ["/", "/console", "/investigate", "/report", "/docs", "/openapi.json", "/favicon.svg"]) assert((await page.request.get(BASE + href)).ok(), href);
  });

  // S1: the four older pages run under their own CSP, with their scripts and styles in /assets/legacy. Each still works:
  // its script draws the page, one action reaches the API, and no CSP violation or console error is raised.
  await check("legacy pages: CSP set, no inline script, no violation or console error, behaviour kept", async () => {
    const legacy = [
      ["/", async () => { await page.waitForSelector("#bars .bar"); await page.waitForSelector(".scroll-progress", {state: "attached"}); }],
      ["/console", async () => { await page.click("#msgSamples button.s"); await page.waitForFunction(() => /risk \d/.test(document.querySelector("#msgRes")?.textContent || ""), null, {timeout: 10000}); }],
      ["/investigate", async () => { await page.waitForFunction(() => document.querySelectorAll("#who option").length > 1); await page.click("[data-preset]");
        await page.click("#bAssess"); await page.waitForFunction(() => /^\d+$/.test(document.querySelector("#rScore")?.textContent.trim() || ""), null, {timeout: 10000}); }],
      ["/report", async () => { await page.waitForSelector("#amlTable tbody tr"); }],
    ];
    const bad = [];
    for (const [href, works] of legacy) {
      errors.length = 0;
      const r = await page.goto(BASE + href);
      const csp = r.headers()["content-security-policy"] || "";
      if (!/script-src 'self';/.test(csp) || /script-src[^;]*unsafe/.test(csp)) bad.push(`${href}: CSP ${csp || "missing"}`);
      if (await page.locator("script:not([src])").count()) bad.push(`${href}: inline script`);
      if (await page.locator("style").count()) bad.push(`${href}: inline <style>`);
      await works().catch((e) => bad.push(`${href}: ${e.message.split("\n")[0]}`));
      await page.waitForTimeout(300);
      const v = await page.evaluate(() => window.__csp);
      if (v.length) bad.push(`${href}: CSP violation ${v.join(", ")}`);
      if (errors.length) bad.push(`${href}: console ${errors.join(" | ")}`);
    }
    errors.length = 0;
    assert(bad.length === 0, bad.join(" || "));
  });

  await check("audit: keyboard: skip link first, visible focus on every stop, drawer traps focus, dialog closes on Escape", async () => {
    await page.goto(BASE + "/app?kb=1#/alerts");
    await page.waitForSelector("#al-table tbody [data-open]");
    await page.keyboard.press("Tab");
    assert(await page.evaluate(() => document.activeElement.classList.contains("skip")), "first Tab stop is the skip link");
    assert(await page.evaluate(() => document.activeElement.getBoundingClientRect().left >= 0), "skip link visible on focus");
    await page.keyboard.press("Enter");
    assert(await page.evaluate(() => document.activeElement.id === "main" || location.hash.startsWith("#main") || document.activeElement.closest("#main")), "skip link moves focus to the content");
    await page.goto(BASE + "/app?kb=2#/alerts");
    await page.waitForSelector("#al-table tbody [data-open]");
    const noRing = [];
    for (let i = 0; i < 40; i++) {
      await page.keyboard.press("Tab");
      const f = await page.evaluate(() => { const e = document.activeElement, cs = getComputedStyle(e);
        return {t: e.tagName + (e.id ? "#" + e.id : ""), ring: (cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) > 0) || cs.boxShadow !== "none"}; });
      if (!f.ring && f.t !== "BODY") noRing.push(f.t);
    }
    assert(noRing.length === 0, "no visible focus: " + [...new Set(noRing)].join(", "));
    await page.locator("#al-table [data-open]").first().focus();
    await page.keyboard.press("Enter");
    await page.waitForSelector("#al-drawer");
    assert(await page.evaluate(() => document.activeElement.id === "al-close"), "focus moves into the drawer");
    for (let i = 0; i < 60; i++) await page.keyboard.press("Tab");
    assert(await page.evaluate(() => !!document.activeElement.closest("#al-drawer")), "focus stays in the drawer");
    await page.keyboard.press("Escape");
    assert(await page.locator("#al-drawer").count() === 0 && await page.evaluate(() => !!document.activeElement.closest("#al-table")), "Escape closes and returns focus");
    await page.check("[data-pick]");
    await page.click("#al-case");
    await page.waitForSelector("dialog[open]");
    await page.keyboard.press("Escape");
    assert(await page.locator("dialog[open]").count() === 0, "dialog closes on Escape");
  });

  await check("audit: malformed input on every form gives a clear message, never raw JSON", async () => {
    const said = async (sel, re) => { await page.waitForFunction(([s, src]) => new RegExp(src).test(document.querySelector(s)?.textContent || ""), [sel, re.source], {timeout: 8000})
      .catch(async () => { throw new Error(`${sel}: ${(await page.textContent(sel).catch(() => "missing")).trim().slice(0, 160)}`); });
      assert(!/\[\{"type"/.test(await page.textContent(sel)), `${sel} shows raw JSON`); };
    await who("analyst");
    // transactions: min above max is refused by the server and said so
    await page.goto(BASE + "/app#/transactions");
    await page.waitForSelector("#tx-form");
    await page.fill("#tx-form [name=min_amount]", "500");
    await page.fill("#tx-form [name=max_amount]", "100");
    await page.click("#tx-form [type=submit]");
    await said("#main", /min_amount is above max_amount/);
    await page.click("#tx-clear");
    // network: an unknown account, and markup typed as an account, are shown as text
    await page.goto(BASE + "/app#/network?account=MULE01&bank=0200");
    await page.waitForSelector("#nw-search");
    await page.fill("#nw-search", "<img src=x onerror=alert(1)>");
    await page.press("#nw-search", "Enter");
    await said("#nw-msg", /No stored payment names account|Request failed/);
    assert(await page.locator("#nw-msg img").count() === 0, "markup rendered");
    // alert triage: SLA hours that are not a positive number; a case with a blank title
    await page.goto(BASE + "/app#/alerts");
    await page.waitForSelector("#al-table tbody [data-open]");
    await page.locator("#al-table [data-open]").first().click();
    for (const v of ["abc", "-5", "99999"]) {
      await page.fill("#al-triage [name=sla_hours]", v);
      await page.click("#al-triage [type=submit]");
      await said("#al-tmsg", /SLA hours must be a number above 0/);
    }
    await page.fill("#al-triage [name=sla_hours]", "");
    await page.click("#al-triage [type=submit]");
    await said("#al-tmsg", /Nothing changed/);
    await page.keyboard.press("Escape");
    await page.check("[data-pick]");
    await page.click("#al-case");
    await page.fill("dialog [name=title]", "   ");
    await page.click("dialog [type=submit]");
    await said("#al-cmsg", /A title is needed/);
    await page.keyboard.press("Escape");
    // review: a malformed body and an unknown action are refused by the server
    const key = await page.inputValue("#who-key");
    const bad = await page.request.post(BASE + `/api/v2/alerts/${triaged}/review`, {headers: {"X-Analyst-Key": key, "X-Nonce": "smoke-" + Date.now(), "content-type": "application/json"}, data: Buffer.from("{bad")});
    const unk = await page.request.post(BASE + `/api/v2/alerts/${triaged}/review`, {headers: {"X-Analyst-Key": key, "X-Nonce": "smoke2-" + Date.now(), "content-type": "application/json"}, data: {action: "delete"}});
    assert(bad.status() === 422 && [400, 403].includes(unk.status()), `review: ${bad.status()} ${unk.status()}`);
    for (const q of ["sort=nope", "limit=x", "demo=maybe"]) assert((await page.request.get(BASE + "/api/v2/alerts?" + q)).status() === 422, q);
    // investigation: a blank comment
    await ws(caseId);
    await page.fill('[data-act="comment"] textarea', "   ");
    await page.click('[data-act="comment"] [type=submit]');
    await said("#ws-msg", /\S/);
    // behaviour: a negative amount, a short PIN, a check before enrolment
    await page.goto(BASE + "/app#/behaviour");
    await page.waitForSelector("#bh-pf");
    await page.fill("#bh-pf [name=amount]", "-1");
    await page.click("#bh-pf button[type=submit]");
    await said("#bh-pout", /Amount must be a number of 0 or more/);
    await page.fill("#bh-df [name=normal]", "12");
    await page.click("#bh-dreg");
    await said("#bh-dout", /PINs are 4 to 6 digits/);
    await page.click("#bh-reset");
    await page.click("#bh-check");
    await said("#bh-tout", /Type the sentence first/);
  });

  await check("audit: every remaining control is exercised once (dashboard buckets, theme, filters, zoom, tabs, presets, pickers)", async () => {
    await page.goto(BASE + "/app#/dashboard");
    await page.waitForSelector("[data-bucket]");
    for (const b of ["day", "hour"]) {
      await page.click(`[data-bucket="${b}"]`);
      await page.waitForSelector(`[data-bucket="${b}"][aria-pressed="true"]`);
      assert(await page.locator("#main svg rect, #main svg path").count() > 0, "chart drawn for " + b);
    }
    await page.locator('#main a[href^="#/alerts/"]').first().click();
    await page.waitForSelector("#al-drawer");
    await page.keyboard.press("Escape");
    const t0 = await page.evaluate(() => document.documentElement.getAttribute("data-theme"));
    await page.click("#theme");
    assert(await page.evaluate(() => document.documentElement.getAttribute("data-theme")) !== t0, "theme toggles");
    await page.click("#theme");
    await page.goto(BASE + "/app#/transactions");
    await page.waitForSelector("#tx-form");
    await page.selectOption("#tx-form [name=source]", "assess");
    await page.fill("#tx-form [name=max_amount]", "1000000");
    await page.fill("#tx-form [name=to]", "2030-01-01T00:00");
    await page.click("#tx-form [type=submit]");
    await page.waitForFunction(() => /source=assess/.test(location.hash));
    if (await page.locator("#tx-next:not([disabled])").count()) { await page.click("#tx-next"); await page.waitForSelector("#tx-prev:not([disabled])"); await page.click("#tx-prev"); }
    await page.goto(BASE + "/app#/scam");
    await page.waitForSelector("#sc-hf");
    await page.selectOption("#sc-form [name=channel]", {index: 1});
    await page.fill("#sc-hf [name=q]", "UPI");
    await Promise.all([page.waitForResponse((r) => /\/api\/v2\/messages\?.*q=UPI/.test(r.url())), page.click("#sc-hf button")]);
    await page.waitForSelector("#sc-hist, #sc-htable, #main .empty");
    await page.goto(BASE + "/app#/behaviour");
    await page.waitForSelector("#bh-pf");
    for (const p of ["owner", "coached"]) {
      await page.click(`[data-preset="${p}"]`);
      const [r] = await Promise.all([page.waitForResponse((x) => x.url().endsWith("/v2/authenticate")), page.click("#bh-pf button[type=submit]")]);
      assert(r.ok(), "PRAMAAN " + p);
    }
    await page.click("#bh-if button");
    await page.waitForSelector("[data-identity]");
    await page.goto(BASE + "/app#/network?account=MULE01&bank=0200");
    await page.waitForFunction(() => document.querySelector("#nw-graph")?._cyreg?.cy?.nodes().length > 0);
    const z = () => page.evaluate(() => document.querySelector("#nw-graph")._cyreg.cy.zoom());
    const z0 = await z();
    await page.click("#nw-in");
    assert(await z() > z0, "zoom in");
    await page.click("#nw-out");
    await page.click("#nw-fit");
    await page.fill("#nw-from", "2022-09-01T00:00");
    await page.dispatchEvent("#nw-from", "change");
    await page.waitForFunction(() => /from=/.test(location.hash));
    await page.fill("#nw-to", "2022-09-30T00:00");
    await page.dispatchEvent("#nw-to", "change");
    await page.waitForFunction(() => /to=/.test(location.hash));
    await page.click("#nw-form button >> nth=0");
    await page.goto(BASE + "/app#/investigations");
    await page.waitForSelector(".tabs");
    for (const tab of ["pending", "critical", "sla", "open"]) {
      await page.click(`.tabs a[href$="tab=${tab}"]`);
      await page.waitForFunction((t) => location.hash.endsWith("tab=" + t) && document.querySelector('.tabs a[aria-selected="true"]'), tab);
    }
    await page.goto(BASE + "/app#/assistant/" + caseId);
    await page.waitForSelector("#as-pick");
    const other = await page.locator("#as-pick option").evaluateAll((os, c) => os.map((o) => o.value).find((v) => v !== c), caseId);
    if (other) { await page.selectOption("#as-pick", other); await page.waitForFunction((c) => location.hash === "#/assistant/" + c, other); }
    const real = errors.filter((e) => !/status of (401|403|400|404|409|422|503)/.test(e));
    assert(real.length === 0, real.join(" | "));
  });

  if (SHOTS) {
    await page.selectOption("#who-key", "");
    for (const [w, h, tag] of SIZES) {
      const c = await browser.newContext({viewport: {width: w, height: h}, deviceScaleFactor: 2});
      const p = await c.newPage();
      await p.goto(BASE + "/app#/dashboard");
      await p.waitForSelector(".kpi");
      await p.waitForTimeout(400);
      await p.screenshot({path: path.join(OUT, `w3-dashboard-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app#/alerts");
      await p.waitForSelector("#al-table tbody [data-open]");
      await p.screenshot({path: path.join(OUT, `w3-alerts-${tag}.png`), fullPage: true});
      await p.locator("#al-table [data-open]").first().click();
      await p.waitForSelector("#al-drawer");
      await p.screenshot({path: path.join(OUT, `w3-alert-drawer-${tag}.png`)});
      await p.goto(BASE + "/app#/network?account=MULE01&bank=0200&depth=2");
      await p.waitForFunction(() => document.querySelector("#nw-graph")?._cyreg?.cy?.edges().length > 0);
      await p.waitForTimeout(400);
      await p.screenshot({path: path.join(OUT, `w4-network-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app#/investigations?tab=all");
      await p.waitForSelector(".tabs");
      await p.screenshot({path: path.join(OUT, `w5-investigations-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app#/investigations/" + caseId);
      await p.waitForFunction(() => document.querySelector("#ws-graph")?._cyreg?.cy?.nodes().length > 0);
      await p.locator("[data-ev-verify]").first().click();
      await p.waitForSelector(".ev-out .msg");
      await p.waitForTimeout(300);
      await p.screenshot({path: path.join(OUT, `w5-case-${tag}.png`), fullPage: true});
      await p.click("[data-nr-gen]");
      await p.waitForSelector(".nr-out .nr-st");
      await p.locator("#ws-ai").scrollIntoViewIfNeeded();
      await p.screenshot({path: path.join(OUT, `w6-case-draft-${tag}.png`)});
      await p.goto(BASE + "/app#/assistant/" + caseId);
      await p.waitForSelector("[data-nr-gen]");
      await p.click("[data-nr-gen]");
      await p.waitForSelector(".nr-out .nr-st");
      await p.screenshot({path: path.join(OUT, `w6-assistant-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app#/reports");
      await p.waitForSelector(`a[href="#/reports/${caseId}"]`);
      await p.screenshot({path: path.join(OUT, `w6-reports-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app#/reports/" + caseId);
      await p.waitForSelector("#rp-verify", {timeout: 20000});
      await p.waitForFunction(() => document.querySelector("#rp-graph")?._cyreg?.cy?.nodes().length > 0, null, {timeout: 10000});
      await p.waitForTimeout(300);
      await p.screenshot({path: path.join(OUT, `w6-report-${tag}.png`), fullPage: true});
      await p.emulateMedia({media: "print"});
      await p.screenshot({path: path.join(OUT, `w6-report-print-${tag}.png`), fullPage: true});
      await p.emulateMedia({media: "screen"});
      await p.goto(BASE + "/app#/scam");
      await p.waitForSelector("#sc-htable");
      await p.click('[data-sample="scam"]');
      await p.click("#sc-go");
      await p.waitForSelector(".sc-res");
      await p.screenshot({path: path.join(OUT, `w7-scam-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app#/behaviour");
      await p.waitForSelector("#bh-sess table, #bh-sess .empty");
      await p.click("#bh-synth");
      await p.waitForFunction(() => document.querySelectorAll("[data-typing]").length === 2);
      await p.click('[data-preset="swap"]');
      await p.click("#bh-pf button[type=submit]");
      await p.waitForSelector("[data-pramaan]");
      await p.screenshot({path: path.join(OUT, `w7-behaviour-${tag}.png`), fullPage: true});
      await c.close();
    }
  }

  // last, because loading the replay's account history resets the review desk (its alerts and their cases)
  let demoTxn;
  await check("transactions: a replayed payment shows its SHAP feature contributions", async () => {
    for (const [u, n] of [["/api/v2/demo/reset", 1], ["/api/v2/demo/next", 3]]) for (let i = 0; i < n; i++) assert((await page.request.post(BASE + u, {data: {}})).ok(), u);
    await page.goto(BASE + "/app#/transactions?source=demo_stream");
    await page.waitForSelector("#tx-table tbody tr a");
    assert(await page.locator("#tx-table .p-demo").count() > 0, "demo rows labelled");
    demoTxn = await page.locator("#tx-table tbody td:nth-child(2) a").first().getAttribute("href");
    await page.click(`#tx-table a[href="${demoTxn}"]`);
    await page.waitForSelector(".risk-box");
    assert(await page.locator(".bars .bar-row").count() > 0, "SHAP rows");
    assert(await page.locator(".banner.demo").count() === 1, "demo banner");
    // 503 only from the report check's simulated audit outage, 422 from the refused malformed inputs, 404 from the unknown account
    // the audit searches for, 409 from the replayed nonce
    const real = errors.filter((e) => !/status of (401|403|400|404|409|422|503)/.test(e));
    assert(real.length === 0, real.join(" | "));
  });

  if (SHOTS && demoTxn) {
    for (const [w, h, tag] of SIZES) {
      const c = await browser.newContext({viewport: {width: w, height: h}, deviceScaleFactor: 2});
      const p = await c.newPage();
      await p.goto(BASE + "/app#/transactions");
      await p.waitForSelector("#tx-table tbody tr a");
      await p.screenshot({path: path.join(OUT, `w5-transactions-${tag}.png`), fullPage: true});
      await p.goto(BASE + "/app" + demoTxn);
      await p.waitForSelector(".risk-box");
      await p.screenshot({path: path.join(OUT, `w5-transaction-detail-${tag}.png`), fullPage: true});
      await c.close();
    }
  }

  await browser.close();
  for (const r of results) console.log(r.join("  "));
  const failed = results.filter((r) => r[0] === "FAIL").length;
  console.log(`${results.length - failed} passed, ${failed} failed at ${VIEWPORT} (${vw}x${vh}) in ${((Date.now() - started) / 1000).toFixed(1)} s`);
  process.exit(failed ? 1 : 0);
})();
