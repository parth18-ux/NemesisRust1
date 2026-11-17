// F. Alert management: every alert the review desk holds (GET /api/v2/alerts), searched, filtered, sorted and paged
// in the browser; a detail drawer; triage through POST /api/v2/alerts/{id}/triage; investigations from a selection.
import {api, isDemo, demoKeys, currentAnalyst, getKey, replayMule} from "./api.js";
import {$, $$, html, num, money, words, bandOf, bandPill, prioPill, demoPill, toast} from "./util.js";
import {evidenceRow, wireEvidence} from "./evidence.js";

const txnLink = (id) => html`<a class="mono" href="#/transactions/${encodeURIComponent(id)}">${id}</a>`;

const PAGE = 25;
const PRIOS = ["critical", "high", "medium", "low"];
const STATES = ["new", "in_progress", "escalated", "done"];
const COLS = [
  ["id", "Alert"], ["score", "Risk"], ["source", "Source"], ["typology", "Typology"], ["entity", "Entity (payee)"], ["txn", "Transaction"],
  ["time", "Time (UTC)"], ["assignee", "Assignee"], ["status", "Status"], ["prio", "Priority"],
];
const st = {rows: [], q: "", band: "", status: "", prio: "", assignee: "", sort: "time", dir: -1, page: 0, sel: new Set(), demo: false};
let mainEl, lastFocus;

function toRow(a, caseOf) {
  const as = a.assessed || {}, p = a.payment || {}, wf = a.workflow || {};
  const motifs = [...new Set((as.payment?.reasons || []).flatMap((r) => r.motifs || []))];
  return {
    a, id: a.alert_id, score: as.risk_score?.score ?? null, band: as.risk_score?.band || bandOf(a.shown_decision),
    source: a.event_id ? "live" : "assess", typology: motifs.map(words).join(", "),
    entity: `${p.to_bank ?? "?"}:${p.to_account ?? "?"}`, payer: `${p.from_bank ?? "?"}:${p.from_account ?? "?"}`,
    txn: a.event_id || a.alert_id, time: p.timestamp || "", assignee: wf.assignee || "", status: a.status || "",
    state: wf.state || "new", prio: wf.priority || "", case: caseOf[a.alert_id] || "",
  };
}

function view() {
  const q = st.q.toLowerCase();
  let rows = st.rows.filter((r) => (!st.band || r.band === st.band) && (!st.status || r.status === st.status || r.state === st.status)
    && (!st.prio || (st.prio === "none" ? !r.prio : r.prio === st.prio))
    && (!st.assignee || (st.assignee === "unassigned" ? !r.assignee : r.assignee === st.assignee))
    && (!q || [r.id, r.entity, r.payer, r.txn, r.typology, r.assignee, r.case, r.a.shown_decision].join(" ").toLowerCase().includes(q)));
  const key = st.sort, rank = (r) => (key === "prio" ? (r.prio ? 4 - PRIOS.indexOf(r.prio) : 0) : r[key] ?? "");
  rows = rows.sort((x, y) => { const a = rank(x), b = rank(y); return (a > b ? 1 : a < b ? -1 : 0) * st.dir || (x.id < y.id ? 1 : -1); });
  return rows;
}

/** "View in network" link for one account (the Network intelligence view, section E). */
const netLink = (bank, account, short) => (account ? html`<a class="small" href="#/network?${new URLSearchParams({account, bank: bank ?? ""})}"
  title="View ${bank ?? ""}:${account} in the network">${short ? "network" : "View in network"}</a>` : "");

function statusCell(r) {
  const cls = r.status === "blocked" ? "p-bad" : r.status === "released" ? "p-ok" : "p-info";
  return html`<span class="pill ${cls}">${r.status}</span> <span class="small muted">${words(r.state)}</span>`;
}

function drawTable() {
  const rows = view();
  const pages = Math.max(1, Math.ceil(rows.length / PAGE));
  st.page = Math.min(st.page, pages - 1);
  const shown = rows.slice(st.page * PAGE, st.page * PAGE + PAGE);
  const allSel = shown.length > 0 && shown.every((r) => st.sel.has(r.id));
  $("#al-table", mainEl).innerHTML = html`<table>
    <caption class="sr">Alerts, ${rows.length} matching</caption>
    <thead><tr><th scope="col"><input type="checkbox" id="al-all" aria-label="Select all on this page" ${allSel ? "checked" : ""}></th>
      ${COLS.map(([k, label]) => html`<th scope="col" ${k === "score" ? html`class="r"` : ""} aria-sort="${st.sort === k ? (st.dir > 0 ? "ascending" : "descending") : "none"}">
        <button type="button" data-sort="${k}">${label}</button></th>`)}</tr></thead>
    <tbody>${shown.length ? shown.map((r) => html`<tr class="${st.sel.has(r.id) ? "sel" : ""}" data-id="${r.id}">
      <td><input type="checkbox" data-pick="${r.id}" aria-label="Select ${r.id}" ${st.sel.has(r.id) ? "checked" : ""}></td>
      <td><button class="link-btn" type="button" data-open="${r.id}">${r.id}</button>${r.case ? html` <span class="pill plain small" title="In investigation">${r.case}</span>` : ""}</td>
      <td class="r"><span class="num">${r.score ?? "n/a"}</span> ${bandPill(r.band)}</td>
      <td>${r.source}</td>
      <td>${r.typology || html`<span class="muted">none</span>`}</td>
      <td class="mono">${r.entity} ${netLink(r.a.payment?.to_bank, r.a.payment?.to_account, true)}</td><td>${txnLink(r.txn)}</td><td class="mono">${r.time}</td>
      <td>${r.assignee || html`<span class="muted">unassigned</span>`}</td><td>${statusCell(r)}</td><td>${prioPill(r.prio)}</td></tr>`)
    : html`<tr><td colspan="${COLS.length + 1}" class="empty">${st.rows.length ? "No alert matches these filters." : "The review desk holds no alert yet."}</td></tr>`}</tbody>
  </table>`;
  $("#al-pager", mainEl).innerHTML = html`<span>${rows.length ? `${st.page * PAGE + 1} to ${st.page * PAGE + shown.length} of ${num(rows.length)}` : "0 alerts"}
      ${rows.length !== st.rows.length ? `(filtered from ${num(st.rows.length)})` : ""}</span>
    <span class="sp">${st.sel.size} selected</span>
    <button class="btn" type="button" id="al-prev" ${st.page === 0 ? "disabled" : ""}>Previous</button>
    <span>Page ${st.page + 1} of ${pages}</span>
    <button class="btn" type="button" id="al-next" ${st.page >= pages - 1 ? "disabled" : ""}>Next</button>`;
  $("#al-case", mainEl).disabled = st.sel.size === 0;
  $("#al-case", mainEl).textContent = st.sel.size ? `Create investigation (${st.sel.size})` : "Create investigation";
}

function wire() {
  const m = mainEl;
  $("#al-q", m).addEventListener("input", (e) => { st.q = e.target.value; st.page = 0; drawTable(); });
  for (const k of ["band", "status", "prio", "assignee"]) $(`#al-${k}`, m).addEventListener("change", (e) => { st[k] = e.target.value; st.page = 0; drawTable(); });
  $("#al-table", m).addEventListener("click", (e) => {
    const s = e.target.closest("[data-sort]");
    if (s) { const k = s.dataset.sort; st.dir = st.sort === k ? -st.dir : (k === "time" || k === "score" || k === "prio" ? -1 : 1); st.sort = k; drawTable(); $(`[data-sort="${k}"]`, m).focus(); return; }
    const o = e.target.closest("[data-open]");
    if (o) { openDrawer(o.dataset.open, o); return; }
  });
  $("#al-table", m).addEventListener("change", (e) => {
    if (e.target.id === "al-all") {
      $$("[data-pick]", m).forEach((c) => (e.target.checked ? st.sel.add(c.dataset.pick) : st.sel.delete(c.dataset.pick)));
    } else if (e.target.dataset.pick) {
      e.target.checked ? st.sel.add(e.target.dataset.pick) : st.sel.delete(e.target.dataset.pick);
    }
    drawTable();
  });
  $("#al-pager", m).addEventListener("click", (e) => {
    if (e.target.id === "al-prev") st.page--; else if (e.target.id === "al-next") st.page++; else return;
    drawTable();
  });
  $("#al-case", m).addEventListener("click", openCaseDialog);
  $("#al-refresh", m).addEventListener("click", () => load(true));
  $("#al-seed", m)?.addEventListener("click", seedDemo);
}

// ---- drawer

function closeDrawer() {
  $("#al-drawer")?.remove();
  $("#scrim").hidden = true;
  lastFocus?.focus?.();
}

async function openDrawer(id, from) {
  const r = st.rows.find((x) => x.id === id);
  if (!r) { toast(`Alert ${id} is not in the list`); return; }
  lastFocus = from || document.activeElement;
  $("#al-drawer")?.remove();
  const a = r.a, as = a.assessed || {}, p = a.payment || {}, wf = a.workflow || {};
  const reasons = as.payment?.reasons || [], ctxReasons = as.context?.reasons || [];
  const me = await currentAnalyst();
  const people = (await demoKeys()).filter((x) => x.role === "analyst" || x.role === "supervisor");
  const d = document.createElement("aside");
  d.id = "al-drawer";
  d.className = "drawer";
  d.setAttribute("role", "dialog");
  d.setAttribute("aria-modal", "true");
  d.setAttribute("aria-labelledby", "al-dh");
  d.innerHTML = html`<div class="drawer-h"><h2 id="al-dh">Alert <span class="mono">${a.alert_id}</span></h2>${st.demo ? demoPill() : ""}
      <button class="btn icon" type="button" id="al-close" aria-label="Close details">&#10005;</button></div>
    <div class="drawer-b">
      <dl class="kv">
        <dt>Risk score</dt><dd><span class="num">${r.score ?? "n/a"}</span> ${bandPill(r.band)} <span class="small muted">${as.risk_score?.note ?? ""}</span></dd>
        <dt>Shown decision</dt><dd>${a.shown_decision}</dd>
        <dt>Engine decision</dt><dd>${as.payment_decision ?? as.payment?.decision ?? "n/a"} <span class="small muted">model risk ${as.payment?.risk ?? "n/a"}, exit ${as.payment?.layer_exit ?? "n/a"}</span></dd>
        <dt>Context band</dt><dd>${as.context_band ?? "n/a"} ${as.raised_by_context ? html`<span class="small muted">(raised by context)</span>` : ""}</dd>
        <dt>Status</dt><dd>${statusCell(r)}</dd>
        <dt>Payer</dt><dd><span class="mono">${r.payer}</span> ${netLink(p.from_bank, p.from_account)}</dd>
        <dt>Payee</dt><dd><span class="mono">${r.entity}</span> ${netLink(p.to_bank, p.to_account)}</dd>
        <dt>Amount</dt><dd class="num">${money(p.amount_paid, p.payment_currency)} · ${p.payment_format ?? ""}</dd>
        <dt>Payment time</dt><dd class="mono">${p.timestamp} UTC</dd>
        <dt>Source</dt><dd>${r.source}${a.event_id ? html` · event <span class="mono">${a.event_id}</span>` : ""}</dd>
        <dt>Transaction</dt><dd>${txnLink(r.txn)} <span class="small muted">risk, reasons, related accounts, history</span></dd>
        <dt>Investigation</dt><dd>${r.case ? html`<a class="mono" href="#/investigations/${encodeURIComponent(r.case)}">${r.case}</a>`
          : html`<button class="btn" type="button" id="al-open-inv">Open investigation</button>`}</dd>
        <dt>SLA due</dt><dd class="mono">${wf.sla_due ?? "not set"}</dd>
      </dl>
      <section class="sect"><h3>Model reasons</h3>${reasons.length ? html`<ul>${reasons.map((x) => html`<li>${x.display ?? x.feature}${x.feature && x.display ? html` <span class="small muted">(${x.feature}, impact ${x.impact})</span>` : ""}</li>`)}</ul>`
        : html`<p class="small muted">No model reason codes on this alert.</p>`}</section>
      <section class="sect"><h3>Context reasons</h3>${ctxReasons.length ? html`<ul>${ctxReasons.map((x) => html`<li>${x}</li>`)}</ul>` : html`<p class="small muted">None.</p>`}</section>
      ${as.intervention ? html`<section class="sect"><h3>Customer intervention</h3><p class="small">${as.intervention.screen?.en?.title ?? as.intervention.show}: ${as.intervention.screen?.en?.question ?? ""}</p>
        <p class="small muted">Funds: ${as.intervention.funds ?? "n/a"}${as.intervention.step_up?.length ? ` · step-up: ${as.intervention.step_up.join(", ")}` : ""}</p></section>` : ""}
      <section class="sect"><h3>Evidence</h3><ul class="ev-list">${evidenceRow(a.alert_id)}</ul>
        ${as.signature ? html`<p class="small muted">Assessment signed ${as.signature.alg}, key <span class="mono">${as.signature.key_id}</span>.</p>` : ""}</section>
      <section class="sect" aria-labelledby="al-th"><h3 id="al-th">Triage</h3>
        <form id="al-triage" class="form" novalidate>
          <label class="field">Assignee
            ${people.length ? html`<select name="assignee"><option value="">(unchanged)</option>${people.map((x) => html`<option value="${x.analyst}" ${x.analyst === wf.assignee ? "selected" : ""}>${x.analyst} (${x.role})</option>`)}</select>`
              : html`<input name="assignee" type="text" maxlength="64" value="${wf.assignee ?? ""}" placeholder="analyst id">`}</label>
          <label class="field">Status<select name="state">${STATES.map((s) => html`<option value="${s}" ${s === (wf.state || "new") ? "selected" : ""}>${words(s)}</option>`)}</select></label>
          <label class="field">Priority<select name="priority"><option value="">${wf.priority ? "(unchanged)" : "none"}</option>${PRIOS.map((s) => html`<option value="${s}" ${s === wf.priority ? "selected" : ""}>${s}</option>`)}</select></label>
          <label class="field">SLA hours (optional)<input name="sla_hours" type="text" inputmode="decimal" placeholder="desk default"></label>
          <label class="field full">Comment<textarea name="comment" maxlength="1000" rows="2"></textarea></label>
          <div class="full"><button class="btn primary" type="submit">Save triage</button>
            <span class="small muted">${me ? `as ${me.analyst} (${me.role})` : getKey() ? "with your analyst key" : "needs an analyst or supervisor key"}</span></div>
          <div class="full" id="al-tmsg" role="status" aria-live="polite"></div>
        </form></section>
      <section class="sect"><h3>History</h3>${a.actions?.length ? html`<ul class="log">${[...a.actions].reverse().map((x) => html`<li><b>${x.analyst}</b> · ${words(x.action)} · <span class="mono small">${x.at}</span>
          ${x.changes ? html`<div class="small muted">${Object.entries(x.changes).map(([k, v]) => `${k}: ${v}`).join(" · ")}</div>` : ""}${x.note ? html`<div>${x.note}</div>` : ""}</li>`)}</ul>`
        : html`<p class="small muted">No action yet.</p>`}</section>
    </div>`;
  document.body.append(d);
  $("#scrim").hidden = false;
  $("#scrim").addEventListener("click", closeDrawer, {once: true});
  $("#al-close", d).addEventListener("click", closeDrawer);
  $("#al-triage", d).addEventListener("submit", (e) => { e.preventDefault(); triage(r, e.target); });
  $("#al-open-inv", d)?.addEventListener("click", () => { closeDrawer(); st.sel = new Set([r.id]); openCaseDialog(); });
  wireEvidence(d);
  d.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeDrawer();
    if (e.key === "Tab") {
      const f = $$("a[href],button:not([disabled]),input,select,textarea", d);
      if (e.shiftKey && document.activeElement === f[0]) { e.preventDefault(); f.at(-1).focus(); }
      else if (!e.shiftKey && document.activeElement === f.at(-1)) { e.preventDefault(); f[0].focus(); }
    }
  });
  $("#al-close", d).focus();
}

async function triage(r, form) {
  const wf = r.a.workflow || {}, f = new FormData(form), body = {};
  const assignee = String(f.get("assignee") || "").trim(), state = f.get("state"), prio = f.get("priority"), sla = String(f.get("sla_hours") || "").trim();
  const comment = String(f.get("comment") || "").trim();
  if (assignee && assignee !== wf.assignee) body.assignee = assignee;
  if (state && state !== (wf.state || "new")) body.state = state;
  if (prio && prio !== wf.priority) body.priority = prio;
  if (sla) body.sla_hours = Number(sla);
  if (comment) body.comment = comment;
  const msg = $("#al-tmsg");
  if (!Object.keys(body).length) { msg.innerHTML = html`<p class="msg err">Nothing changed: pick a new value or write a comment.</p>`; return; }
  const btn = form.querySelector("[type=submit]");
  btn.disabled = true;
  try {
    const a = await api(`/v2/alerts/${encodeURIComponent(r.id)}/triage`, {method: "POST", body});
    const i = st.rows.findIndex((x) => x.id === r.id);
    st.rows[i] = toRow(a, {[r.id]: r.case});
    drawTable();
    toast(`Alert ${r.id} updated`);
    await openDrawer(r.id, lastFocus);
    $("#al-tmsg").innerHTML = html`<p class="msg ok">Saved: ${Object.keys(body).join(", ")}.</p>`;
  } catch (e) {
    msg.innerHTML = html`<p class="msg err" role="alert">${e.explain || e.message}</p>`;
    btn.disabled = false;
  }
}

// ---- create investigation

function openCaseDialog() {
  const ids = [...st.sel];
  if (!ids.length) return;
  const dlg = document.createElement("dialog");
  dlg.setAttribute("aria-labelledby", "al-ch");
  dlg.innerHTML = html`<form method="dialog" id="al-cform">
    <h2 id="al-ch">Create investigation</h2>
    <p class="small muted">Alerts: <span class="mono">${ids.join(", ")}</span>. Each alert can be in one open investigation at a time.</p>
    <label class="field">Title<input name="title" type="text" required maxlength="200" value="${`Review of ${ids.length} alert${ids.length > 1 ? "s" : ""}`}"></label>
    <label class="field">Priority<select name="priority">${PRIOS.map((s) => html`<option value="${s}" ${s === "medium" ? "selected" : ""}>${s}</option>`)}</select></label>
    <label class="field">Summary (optional)<textarea name="summary" maxlength="2000" rows="3"></textarea></label>
    <div id="al-cmsg" role="status" aria-live="polite"></div>
    <div class="row"><button class="btn" type="button" value="cancel" id="al-ccancel">Cancel</button><button class="btn primary" type="submit">Create</button></div>
  </form>`.toString();
  document.body.append(dlg);
  dlg.addEventListener("close", () => dlg.remove());
  $("#al-ccancel", dlg).addEventListener("click", () => dlg.close());
  $("#al-cform", dlg).addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = new FormData(e.target);
    const body = {title: String(f.get("title") || "").trim(), alert_ids: ids, priority: f.get("priority")};
    const summary = String(f.get("summary") || "").trim();
    if (summary) body.summary = summary;
    if (!body.title) { $("#al-cmsg", dlg).innerHTML = html`<p class="msg err">A title is needed.</p>`; return; }
    try {
      const inv = await api("/v2/investigations", {method: "POST", body});
      dlg.close();
      st.sel.clear();
      toast(`Investigation ${inv.case_id} opened with ${inv.alert_ids.length} alert(s)`);
      location.hash = "#/investigations/" + encodeURIComponent(inv.case_id);
    } catch (err) {
      $("#al-cmsg", dlg).innerHTML = html`<p class="msg err" role="alert">${err.explain || err.message}</p>`;
    }
  });
  dlg.showModal();
}

// ---- demo seeding (demo mode only): replays the fictional mule fixture through /api/v2/assess

async function seedDemo(e) {
  const btn = e.currentTarget;
  btn.disabled = true;
  try {
    await replayMule((i, n) => { btn.textContent = `Replaying ${i} of ${n}…`; });
    await load(true);
  } catch (err) {
    toast(err.explain || err.message);
    btn.disabled = false;
  }
}

// ---- load and render

async function load(keepFilters) {
  const [alerts, invs] = await Promise.all([api("/v2/alerts"), api("/v2/investigations").catch(() => [])]);
  const caseOf = {};
  for (const inv of Array.isArray(invs) ? invs : invs?.investigations || invs?.data || []) {
    if (inv.status !== "closed") for (const id of inv.alert_ids || []) caseOf[id] = inv.case_id;
  }
  st.rows = (alerts || []).map((a) => toRow(a, caseOf));
  if (!keepFilters) st.sel.clear();
  const known = new Set(st.rows.map((r) => r.id));
  for (const id of st.sel) if (!known.has(id)) st.sel.delete(id);
  const assignees = [...new Set(st.rows.map((r) => r.assignee).filter(Boolean))].sort();
  $("#al-assignee", mainEl).innerHTML = html`<option value="">Any assignee</option><option value="unassigned">Unassigned</option>
    ${assignees.map((x) => html`<option value="${x}" ${x === st.assignee ? "selected" : ""}>${x}</option>`)}`;
  if (st.assignee === "unassigned") $("#al-assignee", mainEl).value = "unassigned";
  $("#al-seedbox", mainEl).hidden = !(st.demo && st.rows.length === 0);
  drawTable();
}

const opts = (vals, any, cur, label = (v) => v) => html`<option value="">${any}</option>${vals.map((v) => html`<option value="${v}" ${v === cur ? "selected" : ""}>${label(v)}</option>`)}`;

export async function render(main, ctx) {
  mainEl = main;
  st.demo = await isDemo();
  main.innerHTML = html`
    ${st.demo ? html`<div class="banner demo" role="note"><b>Demonstration data</b><span>This instance runs in demo mode: its alerts come from demo replays and
      fictional fixtures, not from a bank's payments. Demo analyst keys are offered in the top bar.</span></div>` : ""}
    <section class="card" aria-labelledby="al-h">
      <div class="card-h"><h2 id="al-h">Alerts</h2>
        <button class="btn" type="button" id="al-refresh">Refresh</button>
        <button class="btn primary" type="button" id="al-case" disabled>Create investigation</button></div>
      <div class="toolbar" role="search">
        <label class="field grow">Search<input id="al-q" type="search" placeholder="Alert, account, transaction, typology, assignee" value="${st.q}"></label>
        <label class="field">Risk band<select id="al-band">${opts(["verify", "hold", "block", "allow"], "Any band", st.band)}</select></label>
        <label class="field">Status<select id="al-status">${opts(["open", "released", "blocked", ...STATES], "Any status", st.status, words)}</select></label>
        <label class="field">Priority<select id="al-prio">${opts([...PRIOS, "none"], "Any priority", st.prio)}</select></label>
        <label class="field">Assignee<select id="al-assignee"><option value="">Any assignee</option></select></label>
      </div>
      <div id="al-seedbox" class="banner" hidden><span>No alert yet. Replay the fictional mule fixture (12 victims pay one mule, who forwards to a collector)
        through <span class="mono">/api/v2/assess</span> to open demonstration alerts.</span><button class="btn primary" type="button" id="al-seed">Load demonstration alerts</button></div>
      <div class="table-wrap" id="al-table"></div>
      <div class="pager" id="al-pager"></div>
      <p class="src">Source: GET /api/v2/alerts (the review desk; with Redis also live:alerts) and GET /api/v2/investigations. Risk is the display score
        derived from the decision, not a probability. Triage changes the workflow fields only; an alert's decision status changes only through review.</p>
    </section>`;
  wire();
  try {
    await load(false);
  } catch (e) {
    $("#al-table", main).innerHTML = html`<div class="banner err" role="alert">${e.explain || e.message}</div>`;
    return;
  }
  if (ctx.isCurrent() && ctx.params[0]) openDrawer(ctx.params[0]);
}
