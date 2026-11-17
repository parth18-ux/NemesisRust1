// E. Network intelligence: the accounts and banks around one account, from GET /api/v2/graph. One directed edge per
// stored payment (amount, time, band, evidence ids) and one held_at edge from an account to its bank. Highlights come
// from the backend's indicators (motif reason codes, ring cases) only; nothing here infers a structure of its own.
// Drawn with Cytoscape.js (MIT), vendored under /assets/vendor/.
import cytoscape from "../vendor/cytoscape.esm.min.js";
import {api, isDemo, replayMule} from "./api.js";
import {$, html, num, money, words, bandPill, demoPill, toast} from "./util.js";

const st = {account: "", bank: "", depth: 1, from: "", to: "", min: 0, dir: "all", nodes: new Map(), edges: new Map(), meta: null, demo: false};
let mainEl, cy;

// ---- data

const acctOf = (id) => id.replace(/^account:/, "");
const isPay = (e) => e.type === "payment";
const toInput = (t) => (t ? t.replace(" ", "T") : "");
const fromInput = (v) => (v ? v.replace("T", " ") : "");

function graphPath(account, bank, depth) {
  const q = new URLSearchParams({account, depth: String(depth)});
  if (bank) q.set("bank", bank);
  if (st.from) q.set("from", st.from);
  if (st.to) q.set("to", st.to);
  return "/v2/graph?" + q;
}

/** Adds a /v2/graph response to what is loaded; an account seen twice keeps the union of its alerts and indicators.
 *  `hops` is the depth of the response's own seed in the loaded graph (0 for a fresh load, more for an expansion). */
function merge(g, hops = 0) {
  for (const n of g.nodes) {
    const old = st.nodes.get(n.id);
    if (!old) { st.nodes.set(n.id, {...structuredClone(n), seed: hops === 0 && n.seed, depth: (n.depth ?? 0) + hops}); continue; }
    if (n.type !== "account") continue;
    old.alerts = [...new Set([...old.alerts, ...n.alerts])];
    for (const i of n.indicators) {
      const k = old.indicators.findIndex((x) => x.kind === i.kind && x.name === i.name);
      if (k < 0) old.indicators.push(i); else if ((i.payments ?? 0) > (old.indicators[k].payments ?? 0)) old.indicators[k] = i;
    }
    old.depth = Math.min(old.depth, n.depth + hops);
  }
  for (const e of g.edges) st.edges.set(e.id, e);
}

/** The payments and accounts the filters keep. Direction follows money from (out) or back to (in) the seed account. */
function visible() {
  const pays = [...st.edges.values()].filter((e) => isPay(e) && (e.amount ?? 0) >= st.min);
  let kept = pays;
  if (st.dir !== "all") {
    const [a, b] = st.dir === "out" ? ["from", "to"] : ["to", "from"];
    const reach = new Set([...st.nodes.values()].filter((n) => n.seed).map((n) => n.id));
    for (let grew = true; grew;) {
      grew = false;
      for (const e of pays) if (reach.has(e[a]) && !reach.has(e[b])) { reach.add(e[b]); grew = true; }
    }
    kept = pays.filter((e) => reach.has(e[a]));
  }
  const accts = new Set([...st.nodes.values()].filter((n) => n.seed).map((n) => n.id));
  kept.forEach((e) => { accts.add(e.from); accts.add(e.to); });
  return {pays: kept, accts: [...accts].map((id) => st.nodes.get(id)).filter(Boolean), allPays: pays.length};
}

/** In and out totals, flagged count and counterparties of one account over the loaded payments. */
function stats(id) {
  const s = {in: {n: 0, amt: {}}, out: {n: 0, amt: {}}, flagged: 0, cps: new Map(), first: "", last: ""};
  for (const e of st.edges.values()) {
    if (!isPay(e) || (e.from !== id && e.to !== id)) continue;
    const side = e.from === id ? s.out : s.in, other = e.from === id ? e.to : e.from, cur = e.currency || "";
    side.n++;
    side.amt[cur] = (side.amt[cur] || 0) + (e.amount || 0);
    if (e.band && e.band !== "allow") s.flagged++;
    const c = s.cps.get(other) || {sent: 0, received: 0, amt: 0, cur};
    c[e.from === id ? "sent" : "received"]++;
    c.amt += e.amount || 0;
    s.cps.set(other, c);
    if (!s.first || e.timestamp < s.first) s.first = e.timestamp;
    if (!s.last || e.timestamp > s.last) s.last = e.timestamp;
  }
  return s;
}
const totals = (side) => (side.n ? Object.entries(side.amt).map(([c, v]) => money(v, c)).join(", ") + ` (${side.n} payment${side.n > 1 ? "s" : ""})` : "none");

// ---- drawing

function colours() {
  const cs = getComputedStyle(document.documentElement), v = (n) => cs.getPropertyValue(n).trim();
  return {ink: v("--ink"), ink3: v("--ink-3"), surface: v("--surface"), rule: v("--rule"), accent: v("--accent"), soft: v("--accent-soft"),
    mute: v("--mute-soft"), focus: v("--focus"), allow: v("--ok"), verify: v("--info"), hold: v("--warn"), block: v("--bad"), mono: v("--mono")};
}

function style() {
  const c = colours();
  return [
    {selector: "node", style: {label: "data(label)", "font-family": c.mono, "font-size": 10, color: c.ink, "text-valign": "bottom", "text-margin-y": 4,
      "text-wrap": "wrap", "text-background-color": c.surface, "text-background-opacity": 0.8, "text-background-padding": 1, "min-zoomed-font-size": 6}},
    {selector: "node.account", style: {width: 22, height: 22, "background-color": c.soft, "border-width": 1.5, "border-color": c.accent}},
    {selector: "node.seed", style: {width: 32, height: 32, "background-color": c.accent}},
    {selector: "node.flagged", style: {"border-color": c.hold, "border-width": 2.5}},
    {selector: "node.indicator", style: {"underlay-color": c.block, "underlay-opacity": 0.28, "underlay-padding": 7, "underlay-shape": "ellipse", "border-color": c.block, "border-width": 3}},
    {selector: "node.bank", style: {shape: "round-rectangle", width: 30, height: 18, "background-color": c.mute, "border-width": 1, "border-color": c.ink3, color: c.ink3}},
    {selector: "edge", style: {width: 1.5, "curve-style": "bezier", "control-point-step-size": 24, "target-arrow-shape": "triangle", "arrow-scale": 0.9,
      "line-color": c.ink3, "target-arrow-color": c.ink3, label: "data(label)", "font-family": c.mono, "font-size": 8, color: c.ink, "text-wrap": "wrap",
"text-background-color": c.surface, "text-background-opacity": 0.85, "text-background-padding": 1, "min-zoomed-font-size": 7}},
    ...["allow", "verify", "hold", "block"].map((b) => ({selector: `edge.b-${b}`, style: {"line-color": c[b], "target-arrow-color": c[b]}})),
    {selector: "edge.motif", style: {width: 3}},
    {selector: "edge.held", style: {width: 1, "line-style": "dashed", "line-color": c.rule, "target-arrow-shape": "none", "curve-style": "straight", label: ""}},
    {selector: "node:selected", style: {"border-color": c.focus, "border-width": 4}},
    {selector: "edge:selected", style: {"line-color": c.focus, "target-arrow-color": c.focus, width: 4}},
  ];
}

function elements() {
  const v = visible();
  const banks = new Set(v.accts.map((n) => "bank:" + n.bank));
  const els = [];
  for (const n of v.accts) {
    const s = stats(n.id), ind = n.indicators.length > 0;
    els.push({group: "nodes", data: {id: n.id, label: `${n.account}\n${n.bank}`},
      classes: ["account", n.seed && "seed", s.flagged && "flagged", ind && "indicator"].filter(Boolean).join(" ")});
  }
  for (const b of banks) els.push({group: "nodes", data: {id: b, label: "bank " + b.slice(5)}, classes: "bank"});
  for (const n of v.accts) els.push({group: "edges", data: {id: "held:" + n.id, source: n.id, target: "bank:" + n.bank}, classes: "held"});
  for (const e of v.pays) {
    els.push({group: "edges", data: {id: e.id, source: e.from, target: e.to, label: `${money(e.amount)}\n${(e.timestamp || "").slice(5)}`},
      classes: ["pay", `b-${e.band}`, e.typologies?.length && "motif"].filter(Boolean).join(" ")});
  }
  return {els, v, banks: banks.size};
}

function draw(relayout = true) {
  const {els, v, banks} = elements();
  const pos = new Map(cy.nodes().map((n) => [n.id(), {...n.position()}]));
  cy.elements().remove();
  cy.add(els);
  cy.nodes().forEach((n) => { if (pos.has(n.id())) n.position(pos.get(n.id())); });
  if (relayout) {
    // lay out with one edge per account pair: parallel payments would otherwise pull their two accounts together
    const seen = new Set(), springs = cy.edges().filter((e) => {
      const k = [e.source().id(), e.target().id()].sort().join(" ");
      return !seen.has(k) && seen.add(k);
    });
    cy.nodes().union(springs).layout({name: "cose", animate: false, randomize: pos.size === 0, padding: 40, nodeRepulsion: () => 3e6}).run();
    if (cy.zoom() > 1.4) { cy.zoom(1.4); cy.center(); }
  }
  const allAccts = [...st.nodes.values()].filter((n) => n.type === "account").length;
  $("#nw-counts", mainEl).innerHTML = html`<b>${num(v.accts.length)}</b> of ${num(allAccts)} accounts · <b>${num(v.pays.length)}</b> of
    ${num([...st.edges.values()].filter(isPay).length)} payments · ${num(banks)} banks`;
  $("#nw-accts", mainEl).innerHTML = html`${v.accts.map((n) => html`<option value="${n.account}"></option>`)}`;
  drawTable(v.pays);
}

function drawTable(pays) {
  const rows = [...pays].sort((a, b) => (a.timestamp < b.timestamp ? 1 : -1));
  $("#nw-table", mainEl).innerHTML = html`<table><caption class="sr">Payments in view, ${rows.length}</caption>
    <thead><tr><th scope="col">Time (UTC)</th><th scope="col">From</th><th scope="col">To</th><th scope="col" class="r">Amount</th><th scope="col">Band</th>
      <th scope="col">Motifs</th><th scope="col">Evidence</th></tr></thead>
    <tbody>${rows.length ? rows.map((e) => html`<tr><td class="mono">${e.timestamp}</td><td class="mono">${acctOf(e.from)}</td><td class="mono">${acctOf(e.to)}</td>
      <td class="r num">${money(e.amount, e.currency)}</td><td>${bandPill(e.band)}</td><td>${(e.typologies || []).map(words).join(", ") || html`<span class="muted">none</span>`}</td>
      <td><button class="link-btn" type="button" data-edge="${e.id}">${e.id}</button></td></tr>`)
      : html`<tr><td colspan="7" class="empty">No payment passes these filters.</td></tr>`}</tbody></table>`;
}

function focusNode(id, move = true) {
  if (!id) return false;
  const n = cy.getElementById(id);
  if (!n.length) return false;
  cy.elements().unselect();
  n.select();
  if (move) cy.animate({center: {eles: n}, zoom: Math.max(cy.zoom(), 1.2)}, {duration: 250});
  showNode(id);
  return true;
}

// ---- side panel

function panel(body) {
  const p = $("#nw-panel", mainEl);
  p.innerHTML = body;
}

const txnLink = (id) => html`<a class="mono" href="/api/v2/transactions/${encodeURIComponent(id)}" target="_blank" rel="noopener">${id}</a>`;
const centreHref = (n) => "#/network?" + new URLSearchParams({account: n.account, bank: n.bank, depth: String(st.depth)});

function showNode(id) {
  const n = st.nodes.get(id);
  if (!n) return;
  if (n.type === "bank") {
    const held = [...st.nodes.values()].filter((x) => x.type === "account" && x.bank === n.bank);
    panel(html`<h3>Bank <span class="mono">${n.bank}</span></h3>
      <p class="small muted">A bank node groups the loaded accounts by the bank code their stored payments carry.</p>
      <ul class="cps">${held.map((x) => html`<li><button class="link-btn" type="button" data-node="${x.id}">${x.account}</button></li>`)}</ul>`);
    return;
  }
  const s = stats(id);
  const cps = [...s.cps.entries()].sort((a, b) => b[1].amt - a[1].amt);
  const q = new URLSearchParams({account: n.account, bank: n.bank});
  panel(html`<h3>Account <span class="mono">${n.account}</span></h3>
    <dl class="kv">
      <dt>Bank</dt><dd class="mono">${n.bank}</dd>
      <dt>Position</dt><dd>${n.seed ? "seed account" : `${n.depth} hop${n.depth > 1 ? "s" : ""} from the seed`}</dd>
      <dt>Money in</dt><dd class="num">${totals(s.in)}</dd>
      <dt>Money out</dt><dd class="num">${totals(s.out)}</dd>
      <dt>Flagged</dt><dd>${s.flagged} of ${s.in.n + s.out.n} loaded payments not in the allow band</dd>
      <dt>Seen</dt><dd class="mono small">${s.first || "n/a"} to ${s.last || "n/a"}</dd>
    </dl>
    <div class="row-btns"><button class="btn" type="button" data-expand="${id}">Expand one hop</button><a class="btn" href="${centreHref(n)}">Centre here</a></div>
    <section class="sect"><h4>Indicators from the backend</h4>${n.indicators.length ? html`<ul>${n.indicators.map((i) => html`<li>
      <span class="pill p-bad plain">${words(i.kind)}</span> <b>${words(i.name)}</b>${i.payments ? html` · ${i.payments} payment${i.payments > 1 ? "s" : ""}` : ""}
      ${i.evidence?.length ? html`<div class="small">Evidence: ${i.evidence.map((x, k) => html`${k ? ", " : ""}${txnLink(x)}`)}</div>` : ""}
      ${i.kind === "ring_case" ? html`<div class="small"><a href="/api/v2/cases" target="_blank" rel="noopener">Ring cases (JSON)</a></div>` : ""}
      <div class="small muted">${i.source}</div></li>`)}</ul><p class="small muted">Leads for analyst review, not findings.</p>`
      : html`<p class="small muted">No motif or ring indicator on this account.</p>`}</section>
    <section class="sect"><h4>Counterparties (${cps.length})</h4>${cps.length ? html`<ul class="cps">${cps.slice(0, 30).map(([o, c]) => html`<li>
      <button class="link-btn" type="button" data-node="${o}">${acctOf(o)}</button>
      <span class="small muted">${c.sent ? `sent ${c.sent}` : ""}${c.sent && c.received ? ", " : ""}${c.received ? `received ${c.received}` : ""} · ${money(c.amt, c.cur)}</span></li>`)}</ul>`
      : html`<p class="small muted">None loaded.</p>`}</section>
    <section class="sect"><h4>Alerts and transactions</h4>
      ${n.alerts.length ? html`<p class="small">${n.alerts.map((a, k) => html`${k ? ", " : ""}<a class="mono" href="#/alerts/${encodeURIComponent(a)}">${a}</a>`)}</p>`
        : html`<p class="small muted">No alert names this account in the loaded payments.</p>`}
      <p class="small"><a href="/api/v2/transactions?${q}" target="_blank" rel="noopener">Stored payments of this account (JSON)</a></p></section>`);
}

function showEdge(id) {
  const e = st.edges.get(id);
  if (!e) return;
  const ev = e.evidence || {};
  const same = [...st.edges.values()].filter((x) => isPay(x) && x.id !== id && x.from === e.from && x.to === e.to);
  panel(html`<h3>Payment <span class="mono">${e.id}</span> ${ev.demo ? demoPill() : ""}</h3>
    <dl class="kv">
      <dt>From</dt><dd><button class="link-btn" type="button" data-node="${e.from}">${acctOf(e.from)}</button></dd>
      <dt>To</dt><dd><button class="link-btn" type="button" data-node="${e.to}">${acctOf(e.to)}</button></dd>
      <dt>Amount</dt><dd class="num">${money(e.amount, e.currency)} · ${e.format ?? ""}</dd>
      <dt>Time</dt><dd class="mono">${e.timestamp} UTC</dd>
      <dt>Band</dt><dd>${bandPill(e.band)} <span class="small muted">model risk ${e.risk ?? "n/a"}</span></dd>
      <dt>Motifs</dt><dd>${(e.typologies || []).map(words).join(", ") || html`<span class="muted">none</span>`}</dd>
    </dl>
    <section class="sect"><h4>Evidence</h4><dl class="kv">
      <dt>Transaction</dt><dd id="nw-ev-txn">${txnLink(ev.transaction ?? e.id)}</dd>
      <dt>Event id</dt><dd>${ev.event_id ? txnLink(ev.event_id) : html`<span class="muted">none (source: ${ev.source})</span>`}</dd>
      <dt>Alert</dt><dd>${ev.alert_id ? html`<a class="mono" href="#/alerts/${encodeURIComponent(ev.alert_id)}">${ev.alert_id}</a>` : html`<span class="muted">none</span>`}</dd>
      <dt>Ledger</dt><dd class="mono small">${ev.ledger == null ? "none" : typeof ev.ledger === "object" ? JSON.stringify(ev.ledger) : ev.ledger}</dd>
      <dt>Source</dt><dd>${ev.source}</dd>
    </dl></section>
    ${same.length ? html`<section class="sect"><h4>Other payments on this pair (${same.length})</h4><ul class="cps">${same.slice(0, 30).map((x) => html`<li>
      <button class="link-btn" type="button" data-edge="${x.id}">${x.id}</button> <span class="small muted">${money(x.amount, x.currency)} · ${x.timestamp}</span></li>`)}</ul></section>` : ""}`);
}

function selectEdge(id) {
  cy.elements().unselect();
  cy.getElementById(id).select();
  showEdge(id);
}

// ---- loading

function syncHash() {
  const q = new URLSearchParams({account: st.account});
  if (st.bank) q.set("bank", st.bank);
  if (st.depth !== 1) q.set("depth", String(st.depth));
  if (st.from) q.set("from", st.from);
  if (st.to) q.set("to", st.to);
  history.replaceState(null, "", "#/network?" + q);
}

async function load() {
  const msg = $("#nw-msg", mainEl);
  msg.innerHTML = html`<p class="muted" role="status">Loading the network of ${st.account}…</p>`;
  let g;
  try {
    g = await api(graphPath(st.account, st.bank, st.depth));
  } catch (e) {
    msg.innerHTML = e.status === 404
      ? html`<div class="banner" role="alert">No stored payment names account <span class="mono">${st.account}</span>${st.bank ? ` at bank ${st.bank}` : ""}${st.from || st.to ? " in this time range" : ""}.</div>`
      : html`<div class="banner err" role="alert">${e.explain || e.message}</div>`;
    return;
  }
  st.nodes = new Map();
  st.edges = new Map();
  st.meta = g;
  merge(g);
  syncHash();
  const na = Object.entries(g.not_available || {});
  msg.innerHTML = html`${g.truncated ? html`<div class="banner" role="note">The graph reached a limit (${g.limits?.accounts} accounts, ${num(g.limits?.payments)} payments):
      narrow the time range or the depth to see everything.</div>` : ""}
    <p class="small">Data: <span class="mono">${g.data}</span>${g.window?.note ? html` · <span class="muted">${g.window.note}</span>` : ""}</p>
    ${na.length ? html`<p class="small na">Not available in this data: ${na.map(([k, why], i) => html`${i ? "; " : ""}<b>${words(k)}</b> <span class="muted">(${why})</span>`)}</p>` : ""}`;
  $("#nw-search", mainEl).value = st.account;
  cy.elements().remove();
  draw();
  focusNode([...st.nodes.values()].find((n) => n.seed)?.id, false);
}

async function expand(id) {
  const n = st.nodes.get(id);
  if (!n) return;
  const before = [st.nodes.size, st.edges.size];
  try {
    merge(await api(graphPath(n.account, n.bank, 1)), n.depth);
  } catch (e) {
    toast(e.explain || e.message);
    return;
  }
  draw();
  focusNode(id);
  toast(`Added ${st.nodes.size - before[0]} nodes and ${st.edges.size - before[1]} edges around ${n.account}`);
}

function search(q) {
  q = q.trim();
  if (!q) return;
  const [b, a] = q.includes(":") ? q.split(":", 2) : ["", q];
  const hit = [...st.nodes.values()].find((n) => n.type === "account" && n.account.toLowerCase() === a.toLowerCase() && (!b || n.bank === b));
  if (hit && cy.getElementById(hit.id).length) { focusNode(hit.id); return; }
  st.account = a;
  st.bank = b;
  load();
}

/** No account named: start from the payee of the newest flagged stored payment, else of the newest payment. */
async function pickStart() {
  const t = await api("/v2/transactions?limit=200");
  const rows = t.transactions || [];
  const r = rows.find((x) => x.band && x.band !== "allow") || rows[0];
  return r ? {account: r.to_account, bank: r.to_bank, why: r.band !== "allow" ? "the payee of the newest flagged payment" : "the payee of the newest payment"} : null;
}

async function seed(e) {
  const btn = e.currentTarget;
  btn.disabled = true;
  try {
    const F = await replayMule((i, n) => { btn.textContent = `Replaying ${i} of ${n}…`; });
    location.hash = "#/network?" + new URLSearchParams({account: F.mule.account, bank: F.mule.bank});
  } catch (err) {
    toast(err.explain || err.message);
    btn.disabled = false;
  }
}

// ---- render

function wire() {
  const m = mainEl;
  $("#nw-form", m).addEventListener("submit", (e) => { e.preventDefault(); search($("#nw-search", m).value); });
  $("#nw-depth", m).addEventListener("change", (e) => { st.depth = +e.target.value; load(); });
  for (const k of ["from", "to"]) $(`#nw-${k}`, m).addEventListener("change", (e) => { st[k] = fromInput(e.target.value); load(); });
  $("#nw-min", m).addEventListener("change", (e) => { st.min = Math.max(0, +e.target.value || 0); draw(); });
  $("#nw-dir", m).addEventListener("change", (e) => { st.dir = e.target.value; draw(); });
  $("#nw-fit", m).addEventListener("click", () => cy.fit(undefined, 30));
  $("#nw-in", m).addEventListener("click", () => cy.zoom({level: cy.zoom() * 1.25, renderedPosition: {x: cy.width() / 2, y: cy.height() / 2}}));
  $("#nw-out", m).addEventListener("click", () => cy.zoom({level: cy.zoom() / 1.25, renderedPosition: {x: cy.width() / 2, y: cy.height() / 2}}));
  m.addEventListener("click", (e) => {
    const n = e.target.closest("[data-node]"), x = e.target.closest("[data-edge]"), ex = e.target.closest("[data-expand]");
    if (n) { if (!focusNode(n.dataset.node)) showNode(n.dataset.node); } else if (x) selectEdge(x.dataset.edge); else if (ex) expand(ex.dataset.expand);
  });
  $("#nw-seed", m)?.addEventListener("click", seed);
  cy.on("tap", "node", (e) => showNode(e.target.id()));
  cy.on("tap", "edge", (e) => (e.target.hasClass("held") ? null : showEdge(e.target.id())));
  cy.on("dbltap", "node.account", (e) => expand(e.target.id()));
  // the colours come from the theme tokens: restyle when the theme changes, stop once this view is gone
  const c = cy, obs = new MutationObserver(() => (document.contains(c.container()) ? c.style(style()) : obs.disconnect()));
  obs.observe(document.documentElement, {attributes: true, attributeFilter: ["data-theme"]});
}

export async function render(main, ctx) {
  mainEl = main;
  st.demo = await isDemo();
  const q = ctx.query;
  Object.assign(st, {account: q.get("account") || "", bank: q.get("bank") || "", depth: Math.min(3, Math.max(1, +q.get("depth") || 1)),
    from: q.get("from") || "", to: q.get("to") || "", min: 0, dir: "all"});
  let start = null;
  if (!st.account) {
    try { start = await pickStart(); } catch (e) { main.innerHTML = html`<div class="banner err" role="alert">${e.explain || e.message}</div>`; return; }
    if (start) Object.assign(st, {account: start.account, bank: start.bank});
  }
  main.innerHTML = html`
    ${st.demo ? html`<div class="banner demo" role="note"><b>Demonstration data</b><span>This instance runs in demo mode: its payments come from demo replays and
      fictional fixtures, not from a bank's payments.</span></div>` : ""}
    <section class="card" aria-labelledby="nw-h">
      <div class="card-h"><h2 id="nw-h">Account network</h2>
        <span class="caption" role="note">A connection is not evidence of wrongdoing.</span></div>
      ${st.account ? html`
      <form class="toolbar" id="nw-form" role="search">
        <label class="field grow">Search account<input id="nw-search" type="search" list="nw-accts" placeholder="account or bank:account" value="${st.account}" autocomplete="off"></label>
        <datalist id="nw-accts"></datalist>
        <button class="btn primary" type="submit">Find</button>
        <label class="field">Depth<select id="nw-depth">${[1, 2, 3].map((d) => html`<option value="${d}" ${d === st.depth ? "selected" : ""}>${d} hop${d > 1 ? "s" : ""}</option>`)}</select></label>
        <label class="field">From (UTC)<input id="nw-from" type="datetime-local" value="${toInput(st.from)}"></label>
        <label class="field">To (UTC)<input id="nw-to" type="datetime-local" value="${toInput(st.to)}"></label>
        <label class="field">Min amount<input id="nw-min" type="number" min="0" step="any" inputmode="decimal" value="0"></label>
        <label class="field">Direction<select id="nw-dir"><option value="all">All payments</option><option value="out">Money out of the seed</option>
          <option value="in">Money into the seed</option></select></label>
      </form>
      ${start ? html`<p class="small muted">No account named: starting from <span class="mono">${start.bank}:${start.account}</span>, ${start.why}.</p>` : ""}
      <div id="nw-msg"></div>
      <div class="nw-bar"><span id="nw-counts" class="small" role="status" aria-live="polite"></span>
        <span class="sp"></span><button class="btn icon" type="button" id="nw-in" aria-label="Zoom in">+</button>
        <button class="btn icon" type="button" id="nw-out" aria-label="Zoom out">&minus;</button><button class="btn" type="button" id="nw-fit">Fit</button></div>
      <div class="nw-grid">
        <div id="nw-graph" class="nw-graph" role="img" aria-label="Account network graph. The payments table below lists the same edges."></div>
        <aside id="nw-panel" class="nw-panel" aria-live="polite" aria-label="Selected entity"><p class="small muted">Select an account, a bank or a payment.
          Double-click an account (or use Expand) to load its neighbours.</p></aside>
      </div>
      <div class="legend nw-legend" aria-label="Legend">
        <span><i class="lg-seed"></i>seed account</span><span><i class="lg-acct"></i>account</span><span><i class="lg-flag"></i>has flagged payments</span>
        <span><i class="lg-ind"></i>backend motif or ring indicator</span><span><i class="lg-bank"></i>bank</span>
        <span><i class="sw-allow"></i>allow</span><span><i class="sw-verify"></i>verify</span><span><i class="sw-hold"></i>hold</span><span><i class="sw-block"></i>block</span>
        <span><i class="lg-held"></i>held at (dashed)</span><span class="muted">Arrows run payer to payee; a thick edge carries a motif reason code. Labels: amount and time.</span>
      </div>
      <details class="nw-list"><summary>Payments in view (table)</summary><div class="table-wrap" id="nw-table"></div></details>`
      : html`<div class="empty"><p>No stored payment yet, so there is no network to draw.</p>
        ${st.demo ? html`<p class="small">Replay the fictional mule fixture (12 victims pay one mule, who forwards to a collector) through
          <span class="mono">/api/v2/assess</span>:</p><p><button class="btn primary" type="button" id="nw-seed">Load demonstration network</button></p>` : ""}</div>`}
      <p class="src">Source: GET /api/v2/graph (stored payments only; one edge per payment, evidence ids on each). Highlights show the backend's motif
        and ring-case indicators, nothing inferred here. Graph drawn with <a href="https://js.cytoscape.org" rel="noopener">Cytoscape.js</a>
        (MIT, <a href="/assets/vendor/cytoscape.LICENSE.txt">licence</a>).</p>
    </section>`;
  if (!st.account) { $("#nw-seed", main)?.addEventListener("click", seed); return; }
  // Cytoscape adds a <style> element unless one with this id exists; the CSP blocks inline styles, so app.css carries
  // the rule (.__________cytoscape_container) and this marker stops the injection.
  if (!document.getElementById("__________cytoscape_stylesheet")) {
    const m = document.createElement("meta");
    m.id = "__________cytoscape_stylesheet";
    document.head.append(m);
  }
  cy?.destroy();
  cy = cytoscape({container: $("#nw-graph", main), style: style(), minZoom: 0.2, maxZoom: 4, boxSelectionEnabled: false});
  wire();
  if (ctx.isCurrent()) await load();
}
