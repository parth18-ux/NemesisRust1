// Browser smoke test of the app shell (W2), the dashboard and alert views (W3), the network view (W4) and the transaction and
// investigation views (W5), the assistant and report views (W6), against a running rusetect-server
// in demo mode:  RUSETECT_DEMO=1 RUSETECT_RATE_LIMIT=100000 rusetect-server --port 8011 ; BASE=http://localhost:8011 node tests/e2e/smoke.js
// (the report and screenshot steps send more than the default 240 requests per 10 s from one client, so raise the limit)
// Uses the preinstalled Playwright (Apache-2.0) and Chromium. SHOTS=1 also writes screenshots to docs/screenshots/.
const {chromium} = require("playwright");
const path = require("path");

const BASE = process.env.BASE || "http://localhost:8011";
const SHOTS = process.env.SHOTS === "1";
const OUT = path.join(__dirname, "..", "..", "docs", "screenshots");
const results = [];

async function check(name, fn) {
  try { await fn(); results.push(["PASS", name]); } catch (e) { results.push(["FAIL", name, e.message.split("\n")[0]]); }
}
function assert(c, msg) { if (!c) throw new Error(msg); }

(async () => {
  const browser = await chromium.launch();
  const ctx = await browser.newContext({viewport: {width: 1440, height: 900}, deviceScaleFactor: 2});
  const page = await ctx.newPage();
  const errors = [];
  page.on("console", (m) => { if (m.type() === "error") errors.push(m.text()); });
  page.on("pageerror", (e) => errors.push(e.message));

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
    assert(links.length === 9, `${links.length} section links`);
    for (const [href] of links) {
      await page.click(`#nav a[href="${href}"]`);
      await page.waitForFunction((h) => location.hash === h && document.querySelector('#nav a[aria-current="page"]')?.getAttribute("href") === h, href);
      await page.waitForSelector("#main > *:not([role=status])", {timeout: 10000});
      const built = ["#/dashboard", "#/transactions", "#/network", "#/alerts", "#/investigations", "#/assistant", "#/reports"].includes(href);
      if (!built) assert(await page.locator("#main >> text=coming in a later stage").count() === 1, `${href} has no later-stage note`);
    }
    for (const href of ["/", "/console", "/investigate", "/report", "/docs"]) {
      const r = await page.request.get(BASE + href);
      assert(r.status() === 200, `${href} returned ${r.status()}`);
    }
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

  await check("alerts: search and band filter", async () => {
    await page.fill("#al-q", "COLL01");
    const ents = await page.locator("#al-table tbody tr td:nth-child(6)").allTextContents();
    assert(ents.length > 0 && ents.every((t) => t.includes("COLL01")), "search: " + ents.join(","));
    await page.fill("#al-q", "");
    await page.selectOption("#al-band", "verify");
    const bands = await page.locator("#al-table tbody tr td:nth-child(3) .pill").allTextContents();
    assert(bands.length > 0 && bands.every((t) => t === "verify"), "band filter: " + bands.join(","));
    await page.selectOption("#al-band", "");
  });

  await check("alerts: sort by risk both ways", async () => {
    const scores = async () => (await page.locator("#al-table tbody tr td:nth-child(3) .num").allTextContents()).map(Number);
    await page.click('[data-sort="score"]');
    let s = await scores();
    assert(await page.getAttribute('th:has([data-sort="score"])', "aria-sort") === "descending", "aria-sort");
    assert(s.every((v, i) => i === 0 || s[i - 1] >= v), "descending " + s);
    await page.click('[data-sort="score"]');
    s = await scores();
    assert(s.every((v, i) => i === 0 || s[i - 1] <= v), "ascending " + s);
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
    // the 401 and 403 responses above are expected; the browser logs failed requests as errors, so ignore those only
    const real = errors.filter((e) => !/status of (401|403)/.test(e));
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

  if (SHOTS) {
    await page.selectOption("#who-key", "");
    for (const [w, h, tag] of [[1440, 900, "desktop"], [390, 844, "phone"]]) {
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
    // 503 only from the report check's simulated audit outage
    const real = errors.filter((e) => !/status of (401|403|400|503)/.test(e));
    assert(real.length === 0, real.join(" | "));
  });

  if (SHOTS && demoTxn) {
    for (const [w, h, tag] of [[1440, 900, "desktop"], [390, 844, "phone"]]) {
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
  console.log(`${results.length - failed} passed, ${failed} failed`);
  process.exit(failed ? 1 : 0);
})();
