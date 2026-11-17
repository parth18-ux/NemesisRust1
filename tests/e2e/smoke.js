// Browser smoke test of the app shell (W2) and the dashboard and alert views (W3), against a running rusetect-server
// in demo mode:  RUSETECT_DEMO=1 rusetect-server --port 8011 ; BASE=http://localhost:8011 node tests/e2e/smoke.js
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
      const built = href === "#/dashboard" || href === "#/alerts";
      if (!built) assert(await page.locator("#main >> text=coming in a later stage").count() === 1, `${href} has no later-stage note`);
    }
    for (const href of ["/", "/console", "/investigate", "/report", "/docs"]) {
      const r = await page.request.get(BASE + href);
      assert(r.status() === 200, `${href} returned ${r.status()}`);
    }
    assert(errors.length === 0, "console errors: " + errors.join(" | "));
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
    await page.waitForSelector(`#al-table tr[data-id="${free[0]}"] .pill.plain.small`);
  });

  await check("no console error during the alert flows", async () => {
    // the 401 and 403 responses above are expected; the browser logs failed requests as errors, so ignore those only
    const real = errors.filter((e) => !/status of (401|403)/.test(e));
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
      await c.close();
    }
  }

  await browser.close();
  for (const r of results) console.log(r.join("  "));
  const failed = results.filter((r) => r[0] === "FAIL").length;
  console.log(`${results.length - failed} passed, ${failed} failed`);
  process.exit(failed ? 1 : 0);
})();
