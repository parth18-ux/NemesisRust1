// G. Investigation and case management: the investigation list (GET /api/v2/investigations) with tabs and SLA state,
// and the one-screen case workspace (GET /api/v2/investigations/{id}): header, linked alerts and their payments, a
// network excerpt, a merged timeline, comments, evidence packs, the ledger history and every case action
// (POST /api/v2/investigations/{id}/actions, a fresh X-Nonce each). Closing takes two people (four-eyes): the server
// enforces it; the page shows the rule and disables Approve for the requester when it knows who is acting (demo keys).
import {api, demoKeys, currentAnalyst, getKey} from "./api.js";
import {$, html, num, money, fixed, words, bandPill, prioPill, toast, applyWidths} from "./util.js";
import {evidenceRow, wireEvidence} from "./evidence.js";
import {miniNetwork} from "./network.js";
import {narrativePanel, wireNarrative, focusEl} from "./assistant.js";

const enc = encodeURIComponent;
const PRIOS = ["critical", "high", "medium", "low"];
const STAGES = ["open", "in_progress", "pending", "pending_closure", "closed"];
const TABS = [
  ["open", "Open", (c) => !["pending_closure", "closed"].includes(c.status)],
  ["pending", "Pending close", (c) => c.status === "pending_closure"],
  ["closed", "Closed", (c) => c.status === "closed"],
  ["critical", "Critical", (c) => c.priority === "critical" && c.status !== "closed"],
  ["sla", "SLA at risk or overdue", (c) => ["at_risk", "overdue"].includes(slaOf(c).state)],
  ["all", "All", () => true],
];
const caseHref = (id) => "#/investigations/" + enc(id);
const iso = (t) => (/^\d{4}-\d\d-\d\d \d\d:\d\d$/.test(t || "") ? t.replace(" ", "T") + ":00Z" : t || "");
const shortTime = (t) => String(t || "").replace("T", " ").replace(/:\d\dZ$/, " UTC");

function span(ms) {
  const m = Math.round(Math.abs(ms) / 60000), d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60);
  return d ? `${d}d ${h}h` : h ? `${h}h ${m % 60}m` : `${m}m`;
}

/** SLA state from the backend's sla_due: overdue past it, at risk in the last quarter of the window since opening. */
export function slaOf(c, now = Date.now()) {
  if (c.status === "closed") return {state: "closed", label: "closed"};
  const due = Date.parse(c.sla_due || "");
  if (Number.isNaN(due)) return {state: "none", label: "no SLA"};
  const left = due - now, total = due - Date.parse(c.created_at || "");
  if (left < 0 || c.sla_breached) return {state: "overdue", label: `overdue by ${span(left)}`};
  if (total > 0 && left < total / 4) return {state: "at_risk", label: `at risk: due in ${span(left)}`};
  return {state: "due", label: `due in ${span(left)}`};
}
const SLA_CLS = {overdue: "p-bad", at_risk: "p-warn", due: "p-ok", closed: "", none: ""};
const slaPill = (c) => { const s = slaOf(c); return html`<span class="pill ${SLA_CLS[s.state]}" title="SLA due ${c.sla_due ?? "not set"}">${s.label}</span>`; };
const STATUS_CLS = {open: "p-info", in_progress: "p-info", pending: "p-warn", pending_closure: "p-warn", closed: "p-ok"};
const statusPill = (s) => html`<span class="pill ${STATUS_CLS[s] ?? ""}">${words(s)}</span>`;
function progress(c) {
  const i = Math.max(0, STAGES.indexOf(c.status));
  return html`<span class="prog" title="Stage ${i + 1} of ${STAGES.length}: ${words(c.status)}"><span class="track"><span class="fill" data-w="${((i + 1) / STAGES.length) * 100}"></span></span>
    <span class="small muted">${i + 1}/${STAGES.length}</span></span>`;
}

// ---- list

async function renderList(main, ctx) {
  const tab = TABS.find((t) => t[0] === ctx.query.get("tab")) || TABS[0];
  let r;
  try { r = await api("/v2/investigations?limit=500"); } catch (e) { main.innerHTML = html`<div class="banner err" role="alert">${e.explain || e.message}</div>`; return; }
  if (!ctx.isCurrent()) return;
  const all = r.investigations || [];
  const rows = all.filter(tab[2]);
  main.innerHTML = html`<section class="card" aria-labelledby="iv-h">
    <div class="card-h"><h2 id="iv-h">Investigations</h2><a class="btn" href="#/alerts">Open one from Alert management</a></div>
    <div class="tabs" role="tablist" aria-label="Investigation lists">${TABS.map(([k, label, f]) => html`<a role="tab" href="#/investigations?tab=${k}"
      aria-selected="${k === tab[0]}" ${k === tab[0] ? html`aria-current="page"` : ""}>${label} <span class="cnt">${all.filter(f).length}</span></a>`)}</div>
    <div class="table-wrap"><table><caption class="sr">${tab[1]} investigations, ${rows.length}</caption>
      <thead><tr><th scope="col">Case</th><th scope="col">Title</th><th scope="col">Status</th><th scope="col">Priority</th><th scope="col">Assignee</th>
        <th scope="col" class="r">Alerts</th><th scope="col">SLA</th><th scope="col">Progress</th><th scope="col">Updated (UTC)</th></tr></thead>
      <tbody>${rows.length ? rows.map((c) => html`<tr data-case="${c.case_id}"><td><a class="mono" href="${caseHref(c.case_id)}">${c.case_id}</a></td>
        <td class="wrap">${c.title}</td><td>${statusPill(c.status)}</td><td>${prioPill(c.priority)}</td><td>${c.assignee || html`<span class="muted">unassigned</span>`}</td>
        <td class="r num">${(c.alert_ids || []).length}</td><td>${slaPill(c)}</td><td>${progress(c)}</td><td class="mono small">${shortTime(c.updated_at)}</td></tr>`)
      : html`<tr><td colspan="9" class="empty">${all.length ? "No investigation in this list." : "No investigation yet: select alerts in Alert management and create one."}</td></tr>`}</tbody></table></div>
    <p class="src">Source: GET /api/v2/investigations (${num(r.total)} in total). SLA: overdue past <span class="mono">sla_due</span>, at risk in the last quarter of
      the time between opening and <span class="mono">sla_due</span>. The due times are this desk's defaults (critical 4 h, high 24 h, medium 72 h, low 168 h) unless set,
      not regulatory deadlines. Progress is the case's stage: open, in progress, pending, pending closure, closed.</p>
  </section>`;
  applyWidths(main);
}

// ---- workspace

const payLine = (p) => (p ? `${money(p.amount_paid, p.payment_currency)} from ${p.from_bank}:${p.from_account} to ${p.to_bank}:${p.to_account}` : "");
const detailText = (d) => Object.entries(d || {}).filter(([, v]) => v != null && v !== "").map(([k, v]) => `${words(k)}: ${Array.isArray(v) ? v.join(", ") : typeof v === "object" ? JSON.stringify(v) : v}`).join(" · ");

function timeline(inv, alerts, txns) {
  const ev = [];
  for (const a of alerts) {
    if (a.payment) ev.push({t: iso(a.payment.timestamp), kind: "payment", who: "", text: html`${payLine(a.payment)} · <a class="mono" href="#/transactions/${enc(a.event_id || a.alert_id)}">${a.event_id || a.alert_id}</a>`});
  }
  for (const d of txns.values()) for (const e of d.timeline || []) {
    ev.push({t: e.at, kind: words(e.type), who: e.analyst || "", text: html`${e.alert_id ? html`<a class="mono" href="#/alerts/${enc(e.alert_id)}">${e.alert_id}</a> ` : ""}${e.decision || e.action || ""}`});
  }
  for (const h of inv.history || []) ev.push({t: h.at, kind: "case: " + words(h.action), who: h.analyst, text: detailText(h.detail)});
  for (const c of inv.comments || []) ev.push({t: c.at, kind: "comment", who: c.analyst, text: c.text});
  return ev.sort((a, b) => (a.t < b.t ? 1 : a.t > b.t ? -1 : 0));
}

function actionsPanel(inv, me, people, free) {
  const st = inv.status, closed = st === "closed", pending = st === "pending_closure", cl = inv.closure || {};
  const isRequester = !!me && cl.requested_by === me.analyst;
  const dis = closed ? "disabled" : "";
  return html`<p class="small muted">${me ? `Acting as ${me.analyst} (${me.role}).` : getKey() ? "Acting with your analyst key." : "Read only: choose an analyst or supervisor key in the top bar to act."}
      Every action is recorded in the hash-chained ledger.</p>
    <div id="ws-msg" role="status" aria-live="polite"></div>
    <form class="act" data-act="assign"><label class="field">Assignee
      ${people.length ? html`<select name="assignee">${people.map((p) => html`<option value="${p.analyst}" ${p.analyst === inv.assignee ? "selected" : ""}>${p.analyst} (${p.role})</option>`)}</select>`
        : html`<input name="assignee" type="text" maxlength="128" value="${inv.assignee ?? ""}" placeholder="analyst id">`}</label>
      <button class="btn" type="submit" ${dis}>Assign</button></form>
    <p class="small muted">An analyst can take work themselves; assigning someone else needs a supervisor.</p>
    <form class="act" data-act="priority"><label class="field">Priority<select name="priority">${PRIOS.map((p) => html`<option value="${p}" ${p === inv.priority ? "selected" : ""}>${p}</option>`)}</select></label>
      <button class="btn" type="submit" ${dis}>Set priority</button></form>
    <form class="act" data-act="status"><label class="field">Status<select name="status">${STAGES.slice(0, 3).map((s) => html`<option value="${s}" ${s === st ? "selected" : ""}>${words(s)}</option>`)}</select></label>
      <button class="btn" type="submit" ${closed || pending ? "disabled" : ""}>Set status</button></form>
    <form class="act" data-act="sla"><label class="field">SLA (hours from now)<input name="sla_hours" type="number" min="0.1" max="2160" step="any" inputmode="decimal" placeholder="e.g. 24"></label>
      <button class="btn" type="submit" ${dis}>Set SLA</button></form>
    <form class="act" data-act="add_alert"><label class="field">Add alert${free.length ? html`<select name="alert_id">${free.map((a) => html`<option value="${a}">${a}</option>`)}</select>`
        : html`<input name="alert_id" type="text" maxlength="64" placeholder="alert id">`}</label>
      <button class="btn" type="submit" ${dis}>Add alert</button></form>
    <div class="close-box">
      <h3>Closure (four-eyes)</h3>
      <p class="small">Rule: one person requests the closure with a reason; a <b>different</b> analyst or supervisor approves or rejects it. Closing decides no payment.</p>
      ${closed ? html`<p class="small">Closed: requested by <b>${cl.requested_by ?? "?"}</b>, approved by <b>${cl.approved_by ?? "?"}</b> at <span class="mono">${shortTime(cl.decided_at)}</span>.</p>
          <p class="small muted">Reason: ${cl.reason ?? ""}</p>
          <button class="btn" type="button" data-do="reopen" ${me && me.role !== "supervisor" ? "disabled" : ""}>Reopen</button>
          <span class="small muted">Reopening needs a supervisor key.</span>`
        : pending ? html`<p class="small">Requested by <b>${cl.requested_by}</b> at <span class="mono">${shortTime(cl.requested_at)}</span>.</p>
          <p class="small muted">Reason: ${cl.reason ?? ""}</p>
          <div class="row-btns"><button class="btn primary" type="button" data-do="approve_close" ${isRequester ? "disabled" : ""}>Approve closure</button>
            <button class="btn" type="button" data-do="reject_close" ${isRequester ? "disabled" : ""}>Reject closure</button></div>
          ${isRequester ? html`<p class="small msg err" id="ws-4eyes">You requested this closure, so you cannot decide it: a different analyst or supervisor must approve or reject it.</p>` : ""}`
        : html`<form class="act" data-act="request_close"><label class="field">Reason for closing<textarea name="text" rows="2" maxlength="1000" required></textarea></label>
          <button class="btn" type="submit">Request closure</button></form>`}
    </div>`;
}

async function renderCase(main, ctx, id) {
  let d;
  try { d = await api("/v2/investigations/" + enc(id)); } catch (e) {
    main.innerHTML = e.status === 404 ? html`<div class="banner" role="alert">No investigation <span class="mono">${id}</span>. <a href="#/investigations">All investigations</a></div>`
      : html`<div class="banner err" role="alert">${e.explain || e.message}</div>`;
    return;
  }
  const inv = d.investigation, alerts = d.alerts || [];
  const [me, keys, alertList, audit, txList] = await Promise.all([
    currentAnalyst(), demoKeys(), api("/v2/alerts").catch(() => []), api("/v2/audit").catch(() => null),
    Promise.allSettled(alerts.map((a) => api("/v2/transactions/" + enc(a.event_id || a.alert_id)))),
  ]);
  if (!ctx.isCurrent()) return;
  const txns = new Map();
  txList.forEach((r, i) => { if (r.status === "fulfilled") txns.set(alerts[i].alert_id, r.value); });
  const people = keys.filter((k) => k.role === "analyst" || k.role === "supervisor");
  const free = (Array.isArray(alertList) ? alertList : []).filter((a) => !a.case_id).map((a) => a.alert_id);
  const seeds = new Map();
  for (const a of alerts) if (a.payment) for (const s of ["from", "to"]) seeds.set(`${a.payment[s + "_bank"]}:${a.payment[s + "_account"]}`, {bank: a.payment[s + "_bank"], account: a.payment[s + "_account"]});
  const s = slaOf(inv), ev = timeline(inv, alerts, txns);
  // the ledger entries of the case and of its alerts (the chain check needs GET /api/v2/audit; without it, the case's own)
  const mine = new Set(alerts.map((a) => a.alert_id));
  const ledger = audit ? audit.entries.filter((e) => e.case_id === inv.case_id || mine.has(e.alert_id)) : d.audit_entries || [];

  // listeners go on this wrapper, which the next render replaces (#main itself outlives every view)
  main.innerHTML = html`<div class="ws">
    <p class="small"><a href="#/investigations">&larr; All investigations</a></p>
    <section class="card ws-head" aria-labelledby="ws-h">
      <div class="card-h"><h2 id="ws-h"><span class="mono">${inv.case_id}</span> · ${inv.title}</h2>${statusPill(inv.status)} ${prioPill(inv.priority)}
        <a class="btn" href="#/reports/${enc(inv.case_id)}">Investigation report</a></div>
      <dl class="kv ws-kv">
        <dt>Assignee</dt><dd>${inv.assignee || html`<span class="muted">unassigned</span>`}</dd>
        <dt>SLA</dt><dd id="ws-sla">${slaPill(inv)} <span class="small muted mono">${inv.status === "closed" ? "" : `due ${shortTime(inv.sla_due)}`}</span></dd>
        <dt>Progress</dt><dd>${progress(inv)} ${words(inv.status)}</dd>
        <dt>Opened</dt><dd>by ${inv.created_by} · <span class="mono small">${shortTime(inv.created_at)}</span></dd>
        <dt>Alerts</dt><dd>${alerts.length}</dd>
        <dt>Summary</dt><dd>${inv.summary || html`<span class="muted">none</span>`}</dd>
      </dl>
      ${s.state === "overdue" ? html`<p class="msg err">The SLA is past due.</p>` : ""}
    </section>
    <section class="card" aria-labelledby="ws-ai"><div class="card-h"><h2 id="ws-ai">AI-assisted investigation</h2>
        <a class="btn" href="#/assistant/${enc(inv.case_id)}">Open on its own page</a></div>${narrativePanel(inv.case_id)}</section>
    <div class="ws-grid">
      <div class="ws-col">
        <section class="card" aria-labelledby="ws-a"><div class="card-h"><h2 id="ws-a">Alerts and their payments</h2></div>
          <div class="table-wrap"><table><thead><tr><th scope="col">Alert</th><th scope="col">Status</th><th scope="col">Transaction</th><th scope="col">Payment</th>
            <th scope="col" class="r">Risk</th><th scope="col">Typology</th><th scope="col">Triage</th></tr></thead>
            <tbody>${alerts.map((a) => { const t = txns.get(a.alert_id)?.transaction, p = a.payment || {}, wf = a.workflow || {};
              return a.missing ? html`<tr><td class="mono">${a.alert_id}</td><td colspan="6" class="muted">This alert is no longer held by the desk.</td></tr>`
              : html`<tr><td><a class="mono" href="#/alerts/${enc(a.alert_id)}">${a.alert_id}</a></td><td title="${a.shown_decision}">${a.status}</td>
                <td><a class="mono" href="#/transactions/${enc(a.event_id || a.alert_id)}">${a.event_id || a.alert_id}</a></td>
                <td class="small"><span class="mono">${p.from_account}</span> &rarr; <a class="mono" href="#/network?${new URLSearchParams({account: p.to_account ?? "", bank: p.to_bank ?? ""})}">${p.to_account}</a>
                  · <span class="num">${money(p.amount_paid, p.payment_currency)}</span> · <span class="mono">${p.timestamp}</span></td>
                <td class="r">${t ? html`<span class="num">${fixed(t.risk, 4)}</span> ${bandPill(t.band)}` : html`<span class="muted">n/a</span>`}</td>
                <td>${(t?.typologies || []).map(words).join(", ") || html`<span class="muted">none</span>`}</td>
                <td class="small">${wf.assignee ?? "unassigned"} · ${wf.priority ?? "no priority"}</td></tr>`; })}</tbody></table></div></section>
        <section class="card" aria-labelledby="ws-n"><div class="card-h"><h2 id="ws-n">Network excerpt</h2>
            <span class="caption" role="note">A connection is not evidence of wrongdoing.</span></div>
          <div id="ws-graph" class="ws-graph" role="img" aria-label="Accounts one payment away from the accounts of this investigation"></div>
          <p class="small" id="ws-gmsg">Seeded with the ${seeds.size} accounts of this investigation's payments; one edge per account pair. Select an account to open it in Network intelligence.</p></section>
        <section class="card" aria-labelledby="ws-t"><div class="card-h"><h2 id="ws-t">Timeline</h2><span class="small muted">${ev.length} events, newest first</span></div>
          <ul class="log tl">${ev.map((e) => html`<li><span class="pill plain">${e.kind}</span> <span class="mono small">${shortTime(e.t)}</span>${e.who ? html` · <b>${e.who}</b>` : ""}<div>${e.text}</div></li>`)}</ul>
          <p class="src">Payments (payment time), ledger entries of the alerts, case actions and comments, merged by time.</p></section>
        <section class="card" aria-labelledby="ws-l"><div class="card-h"><h2 id="ws-l">Audit history</h2>
            ${audit ? html`<span class="pill ${audit.chain_valid ? "p-ok" : "p-bad"}">ledger chain ${audit.chain_valid ? "valid" : "NOT valid"}</span>` : ""}</div>
          <div class="table-wrap"><table><thead><tr><th scope="col" class="r">Seq</th><th scope="col">Time (UTC)</th><th scope="col">Analyst</th><th scope="col">Action</th>
            <th scope="col">Status</th><th scope="col">Hash</th></tr></thead>
            <tbody>${ledger.map((e) => html`<tr data-focus="ledger-${e.seq}"><td class="r num">${e.seq}</td><td class="mono small">${shortTime(e.at)}</td><td>${e.analyst ?? html`<span class="muted">system</span>`}</td>
              <td>${words(e.action || e.type)}${e.case_id ? "" : html` <span class="small muted mono">${e.alert_id}</span>`}</td>
              <td>${words(e.status ?? e.decision ?? "")}</td><td class="mono small" title="${e.hash}">${String(e.hash).slice(0, 16)}…</td></tr>`)}</tbody></table></div>
          <p class="src">${d.audit_source ?? ""}${audit ? "; the case's entries and its alerts' entries, chain check from GET /api/v2/audit" : ""}. Comment and reason texts enter the ledger as SHA-256 hashes.</p></section>
      </div>
      <div class="ws-col">
        <section class="card" aria-labelledby="ws-x"><div class="card-h"><h2 id="ws-x">Actions</h2></div>${actionsPanel(inv, me, people, free)}</section>
        <section class="card" aria-labelledby="ws-c"><div class="card-h"><h2 id="ws-c">Analyst comments</h2><span class="small muted">${(inv.comments || []).length}</span></div>
          <ul class="log" id="ws-comments">${(inv.comments || []).length ? [...inv.comments].map((c, i) => [c, i]).reverse().map(([c, i]) => html`<li data-focus="comment-${i}"><b>${c.analyst}</b> · <span class="mono small">${shortTime(c.at)}</span><div>${c.text}</div></li>`)
            : html`<li class="muted">No comment yet.</li>`}</ul>
          <form class="act col" data-act="comment"><label class="field">New comment<textarea name="text" rows="3" maxlength="1000" required></textarea></label>
            <button class="btn primary" type="submit">Add comment</button></form></section>
        <section class="card" aria-labelledby="ws-e"><div class="card-h"><h2 id="ws-e">Evidence and attachments</h2></div>
          <p class="small muted">The backend stores no files, so file upload is not supported. The attachments of an investigation are the signed evidence packs of its alerts.</p>
          <ul class="ev-list">${alerts.filter((a) => !a.missing).map((a) => evidenceRow(a.alert_id))}</ul></section>
      </div>
    </div></div>`;
  const root = $(".ws", main);
  applyWidths(root);
  wireEvidence(root);
  wireActions(root, main, ctx, inv.case_id);
  wireNarrative(root, (saved) => { location.hash = `#/investigations/${enc(id)}?focus=ledger-${(saved.history || []).at(-1)?.ledger_seq}`; });
  const focus = ctx.query.get("focus");
  const target = focus && root.querySelector(`[data-focus="${CSS.escape(focus)}"]`);
  if (target) focusEl(target);
  try {
    const g = await miniNetwork($("#ws-graph", main), [...seeds.values()]);
    if (g.failed) $("#ws-gmsg", main).append(` ${g.failed} account(s) could not be loaded.`);
  } catch (e) {
    $("#ws-graph", main).innerHTML = html`<p class="small muted">${e.explain || e.message}</p>`;
  }
}

function wireActions(root, main, ctx, id) {
  const send = async (action, fields, btn) => {
    const msg = $("#ws-msg", root);
    btn.disabled = true;
    try {
      await api(`/v2/investigations/${enc(id)}/actions`, {method: "POST", body: {action, ...fields}});
      toast(`${id}: ${words(action)} saved`);
      await renderCase(main, ctx, id);
    } catch (e) {
      msg.innerHTML = html`<p class="msg err" role="alert">${words(action)}: ${e.explain || e.message}</p>`;
      msg.scrollIntoView({block: "nearest"});
      btn.disabled = false;
    }
  };
  root.addEventListener("submit", (e) => {
    const f = e.target.closest("form.act");
    if (!f) return;
    e.preventDefault();
    const fields = {};
    for (const [k, v] of new FormData(f)) { const t = String(v).trim(); if (t) fields[k] = k === "sla_hours" ? Number(t) : t; }
    send(f.dataset.act, fields, f.querySelector("[type=submit]"));
  });
  root.addEventListener("click", (e) => {
    const b = e.target.closest("[data-do]");
    if (b) send(b.dataset.do, {}, b);
  });
}

export async function render(main, ctx) {
  return ctx.params[0] ? renderCase(main, ctx, ctx.params[0]) : renderList(main, ctx);
}
