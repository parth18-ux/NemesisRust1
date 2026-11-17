// B. Transaction intelligence: the stored payment decisions (GET /api/v2/transactions), searched and filtered on the
// server and paged; and one payment's detail (GET /api/v2/transactions/{id}): risk and band, the SHAP and motif reason
// codes, what scored it, related accounts, the payee's history, the ledger timeline and links to the alert, the network
// and the evidence. Nothing is scored here: every value is read from the engine's stored result.
import {api} from "./api.js";
import {$, html, num, money, fixed, words, bandPill, demoPill, applyWidths} from "./util.js";
import {evidenceRow, wireEvidence} from "./evidence.js";

const PAGE = 50;
const FILTERS = ["account", "bank", "band", "source", "typology", "min_amount", "max_amount", "from", "to"];
const BANDS = ["allow", "verify", "hold", "block"];
const SOURCES = ["live", "assess", "demo_stream"];
const MOTIFS = ["cycle", "scatter_gather", "pass_through", "fan_in", "fan_out"];
const enc = encodeURIComponent;
const toInput = (t) => (t ? t.replace(" ", "T") : "");
const fromInput = (v) => (v ? v.replace("T", " ") : "");

export const txnHref = (id) => "#/transactions/" + enc(id);
const netHref = (bank, account) => "#/network?" + new URLSearchParams({account, bank: bank ?? ""});
/** "bank:account" as the backend writes it. */
const split = (k) => { const i = String(k).indexOf(":"); return i < 0 ? ["", k] : [k.slice(0, i), k.slice(i + 1)]; };
const opts = (vals, any, cur) => html`<option value="">${any}</option>${vals.map((v) => html`<option value="${v}" ${v === cur ? "selected" : ""}>${words(v)}</option>`)}`;

// ---- list

function readQuery(q) {
  const f = {};
  for (const k of FILTERS) if (q.get(k)) f[k] = q.get(k);
  return {f, offset: Math.max(0, +q.get("offset") || 0)};
}

function listView(main, f) {
  main.innerHTML = html`<section class="card" aria-labelledby="tx-h">
    <div class="card-h"><h2 id="tx-h">Stored payments</h2><span id="tx-data"></span></div>
    <form class="toolbar" id="tx-form" role="search">
      <label class="field">Account<input name="account" type="search" value="${f.account ?? ""}" placeholder="any" size="12" autocomplete="off"></label>
      <label class="field">Bank<input name="bank" type="search" value="${f.bank ?? ""}" placeholder="any" size="6" autocomplete="off"></label>
      <label class="field">Band<select name="band">${opts(BANDS, "Any band", f.band)}</select></label>
      <label class="field">Source<select name="source">${opts(SOURCES, "Any source", f.source)}</select></label>
      <label class="field">Typology<select name="typology">${opts(MOTIFS, "Any typology", f.typology)}</select></label>
      <label class="field">Min amount<input name="min_amount" type="number" min="0" step="any" inputmode="decimal" value="${f.min_amount ?? ""}"></label>
      <label class="field">Max amount<input name="max_amount" type="number" min="0" step="any" inputmode="decimal" value="${f.max_amount ?? ""}"></label>
      <label class="field">From (UTC)<input name="from" type="datetime-local" value="${toInput(f.from)}"></label>
      <label class="field">To (UTC)<input name="to" type="datetime-local" value="${toInput(f.to)}"></label>
      <button class="btn primary" type="submit">Search</button><button class="btn" type="button" id="tx-clear">Clear</button>
    </form>
    <div id="tx-msg"></div>
    <div class="table-wrap" id="tx-table"></div>
    <div class="pager" id="tx-pager"></div>
    <p class="src">Source: GET /api/v2/transactions (stored decisions: live events, /assess alerts and the demo replay; newest payment first). Risk is the
      cascade's served model risk; the band is the final decision. Account and bank match either side of the payment.</p>
  </section>`;
}

function go(f, offset = 0) {
  const q = new URLSearchParams();
  for (const k of FILTERS) if (f[k]) q.set(k, f[k]);
  if (offset) q.set("offset", String(offset));
  location.hash = "#/transactions" + (q.size ? "?" + q : "");
}

function drawRows(main, r, f, offset) {
  const rows = r.transactions || [];
  $("#tx-data", main).innerHTML = html`<span class="small muted">data: <span class="mono">${r.data}</span></span>`;
  $("#tx-msg", main).innerHTML = r.window?.note ? html`<p class="small muted">${r.window.note}</p>` : "";
  $("#tx-table", main).innerHTML = html`<table><caption class="sr">Stored payments, ${r.total} matching</caption>
    <thead><tr><th scope="col">Time (UTC)</th><th scope="col">Transaction</th><th scope="col">From</th><th scope="col">To</th><th scope="col" class="r">Amount</th>
      <th scope="col" class="r">Risk</th><th scope="col">Band</th><th scope="col">Typology</th><th scope="col">Source</th><th scope="col">Alert</th></tr></thead>
    <tbody>${rows.length ? rows.map((t) => html`<tr>
      <td class="mono">${t.timestamp}</td>
      <td><a class="mono" href="${txnHref(t.id)}">${t.id}</a></td>
      <td class="mono">${t.from_bank}:${t.from_account}</td><td class="mono">${t.to_bank}:${t.to_account}</td>
      <td class="r num">${money(t.amount, t.currency)}</td><td class="r num">${fixed(t.risk, 4)}</td><td>${bandPill(t.band)}</td>
      <td>${(t.typologies || []).map(words).join(", ") || html`<span class="muted">none</span>`}</td>
      <td>${words(t.source)} ${t.demo ? demoPill() : ""}</td>
      <td>${t.alert_id ? html`<a class="mono" href="#/alerts/${enc(t.alert_id)}">${t.alert_id}</a>` : html`<span class="muted">none</span>`}</td></tr>`)
    : html`<tr><td colspan="10" class="empty">${Object.keys(f).length ? "No stored payment matches these filters." : "No payment decision is stored yet."}</td></tr>`}</tbody></table>`;
  const last = offset + rows.length;
  $("#tx-pager", main).innerHTML = html`<span>${r.total ? `${offset + 1} to ${last} of ${num(r.total)}` : "0 payments"}</span><span class="sp"></span>
    <button class="btn" type="button" id="tx-prev" ${offset === 0 ? "disabled" : ""}>Previous</button>
    <span>Page ${Math.floor(offset / PAGE) + 1} of ${Math.max(1, Math.ceil(r.total / PAGE))}</span>
    <button class="btn" type="button" id="tx-next" ${last >= r.total ? "disabled" : ""}>Next</button>`;
  $("#tx-prev", main).addEventListener("click", () => go(f, Math.max(0, offset - PAGE)));
  $("#tx-next", main).addEventListener("click", () => go(f, offset + PAGE));
}

async function renderList(main, ctx) {
  const {f, offset} = readQuery(ctx.query);
  listView(main, f);
  $("#tx-form", main).addEventListener("submit", (e) => {
    e.preventDefault();
    const d = new FormData(e.target), nf = {};
    for (const k of FILTERS) { const v = String(d.get(k) ?? "").trim(); if (v) nf[k] = k === "from" || k === "to" ? fromInput(v) : v; }
    go(nf);
  });
  $("#tx-clear", main).addEventListener("click", () => go({}));
  const q = new URLSearchParams({...f, limit: String(PAGE), offset: String(offset)});
  try {
    const r = await api("/v2/transactions?" + q);
    if (ctx.isCurrent()) drawRows(main, r, f, offset);
  } catch (e) {
    $("#tx-table", main).innerHTML = html`<div class="banner err" role="alert">${e.explain || e.message}</div>`;
  }
}

// ---- detail

function reasonBars(rows) {
  const max = Math.max(...rows.map((r) => Math.abs(r.impact || 0)), 1e-9);
  return html`<ul class="bars">${rows.map((r) => html`<li class="bar-row">
    <span><b>${r.feature}</b> <span class="small muted">${r.display ?? r.value}</span></span>
    <span class="n" title="SHAP contribution">${r.impact > 0 ? "+" : ""}${fixed(r.impact, 3)}</span>
    <span class="track"><span class="fill ${r.impact < 0 ? "f-allow" : "f-hold"}" data-w="${(Math.abs(r.impact || 0) / max) * 100}"></span></span></li>`)}</ul>
  <p class="small muted">Bar: size of the contribution; amber raises the risk, green lowers it.</p>`;
}

function detailView(main, d) {
  const t = d.transaction, rk = d.risk || {}, ex = d.explanations || {}, m = d.model || {}, al = d.alert, bh = d.beneficiary_history || {};
  const th = rk.thresholds || {};
  const evId = al?.alert_id;
  main.innerHTML = html`<div class="txd">
    <p class="small"><a href="#/transactions">&larr; All stored payments</a></p>
    ${t.demo ? html`<div class="banner demo" role="note"><b>Demonstration data</b><span>This payment comes from the replay of recorded IBM AMLworld (synthetic)
      payments, not from a bank.</span></div>` : ""}
    <section class="card" aria-labelledby="td-h">
      <div class="card-h"><h2 id="td-h">Payment <span class="mono">${t.id}</span></h2>${bandPill(t.band)} ${t.demo ? demoPill() : ""}</div>
      <div class="grid cols-2">
        <dl class="kv">
          <dt>From</dt><dd><span class="mono">${t.from_bank}:${t.from_account}</span> <a class="small" href="${netHref(t.from_bank, t.from_account)}">network</a>
            · <a class="small" href="#/transactions?${new URLSearchParams({account: t.from_account, bank: t.from_bank})}">payments</a></dd>
          <dt>To</dt><dd><span class="mono">${t.to_bank}:${t.to_account}</span> <a class="small" href="${netHref(t.to_bank, t.to_account)}">network</a>
            · <a class="small" href="#/transactions?${new URLSearchParams({account: t.to_account, bank: t.to_bank})}">payments</a></dd>
          <dt>Amount</dt><dd class="num">${money(t.amount, t.currency)} · ${t.format ?? ""}</dd>
          <dt>Payment time</dt><dd class="mono">${t.timestamp} UTC</dd>
          <dt>Source</dt><dd>${words(t.source)}${t.event_id ? html` · event <span class="mono">${t.event_id}</span>` : ""}${t.synthetic_label != null ? html` · synthetic label: ${String(t.synthetic_label)}` : ""}</dd>
          <dt>Decision</dt><dd>${t.decision ?? "n/a"}</dd>
          <dt>Typology</dt><dd>${(t.typologies || []).map(words).join(", ") || html`<span class="muted">none</span>`}</dd>
        </dl>
        <div class="risk-box">
          <h3>Risk score</h3>
          <p><span class="big num">${fixed(rk.score, 4)}</span> ${bandPill(rk.band)}</p>
          <div class="track"><span class="fill f-${rk.band || "allow"}" data-w="${(rk.score || 0) * 100}"></span></div>
          <p class="small muted">Model thresholds: verify ${fixed(th.verify, 3)} · hold ${fixed(th.hold, 3)} · block ${fixed(th.block, 3)}. The band is the final
            decision (model plus context and motif rules), so it can sit above the band of the model risk alone.</p>
          <p class="small muted">${rk.source ?? ""}</p>
        </div>
      </div>
    </section>
    <div class="grid cols-2">
      <section class="card" aria-labelledby="td-x">
        <div class="card-h"><h2 id="td-x">Why it scored this way</h2></div>
        <h3>Model reasons (tree-SHAP, L2 XGBoost)</h3>
        ${ex.model_reasons?.length ? reasonBars(ex.model_reasons) : html`<p class="small muted">No model reason code on this payment.</p>`}
        <h3 class="mt">Context: temporal motifs</h3>
        ${ex.motif_reasons?.length ? html`<ul>${ex.motif_reasons.map((r) => html`<li><b>${r.display ?? r.feature}</b>
          <span class="small muted">(${(r.motifs || []).map(words).join(", ")})</span></li>`)}</ul>` : html`<p class="small muted">No motif on this payment.</p>`}
        <p class="src">${ex.note ?? ""}</p>
      </section>
      <section class="card" aria-labelledby="td-m">
        <div class="card-h"><h2 id="td-m">What scored it</h2></div>
        <dl class="kv"><dt>Service</dt><dd class="mono">v${m.service_version ?? "?"}</dd><dt>Ruleset</dt><dd class="mono">${m.ruleset ?? "n/a"}</dd>
          <dt>Trained on</dt><dd>${m.trained_on ?? "n/a"}</dd><dt>Hardened</dt><dd>${m.hardened == null ? "n/a" : m.hardened ? "yes" : "no"}</dd></dl>
        <p class="src">${m.note ?? ""}</p>
        <h3 class="mt">Alert and investigation</h3>
        ${al ? html`<dl class="kv"><dt>Alert</dt><dd><a class="mono" href="#/alerts/${enc(al.alert_id)}">${al.alert_id}</a> · ${al.status} · ${al.shown_decision}</dd>
            <dt>Investigation</dt><dd>${al.case_id ? html`<a class="mono" href="#/investigations/${enc(al.case_id)}">${al.case_id}</a>` : html`<span class="muted">none</span>`}</dd>
            <dt>Triage</dt><dd>${al.workflow ? `${al.workflow.assignee ?? "unassigned"} · ${al.workflow.priority ?? "no priority"} · ${words(al.workflow.state ?? "new")}` : html`<span class="muted">none</span>`}</dd></dl>`
          : html`<p class="small muted">No alert was opened for this payment.</p>`}
        <h3 class="mt">Evidence</h3>
        ${evId ? html`<ul class="ev-list">${evidenceRow(evId)}</ul>` : html`<p class="small muted">No evidence pack: packs belong to alerts.</p>`}
      </section>
    </div>
    <div class="grid cols-eq">
      <section class="card" aria-labelledby="td-r">
        <div class="card-h"><h2 id="td-r">Related accounts</h2><span class="small muted">one stored payment from the payer or payee</span></div>
        ${d.related_accounts?.length ? html`<div class="table-wrap"><table><thead><tr><th scope="col">Account</th><th scope="col">Relation</th>
          <th scope="col" class="r">Payments</th><th scope="col" class="r">Total</th><th scope="col" class="r">Flagged</th></tr></thead>
          <tbody>${d.related_accounts.map((r) => { const [b, a] = split(r.account); return html`<tr><td><a class="mono" href="${netHref(b, a)}">${r.account}</a></td>
            <td>${r.relation}</td><td class="r num">${r.payments}</td><td class="r num">${money(r.total_amount)}</td><td class="r num">${r.flagged_payments}</td></tr>`; })}</tbody></table></div>`
          : html`<p class="small muted">No other stored payment touches the payer or the payee.</p>`}
      </section>
      <section class="card" aria-labelledby="td-b">
        <div class="card-h"><h2 id="td-b">Beneficiary history</h2><span class="mono small">${bh.payee}</span></div>
        <dl class="kv"><dt>Payments</dt><dd class="num">${num(bh.payments)} from ${num(bh.distinct_payers)} payer(s), ${num(bh.flagged_payments)} flagged</dd>
          <dt>Total</dt><dd class="num">${money(bh.total_amount)}</dd><dt>Seen</dt><dd class="mono small">${bh.first_seen ?? "n/a"} to ${bh.last_seen ?? "n/a"}</dd></dl>
        ${bh.recent?.length ? html`<div class="table-wrap mt"><table><thead><tr><th scope="col">Time</th><th scope="col">Payment</th><th scope="col">From</th>
          <th scope="col" class="r">Amount</th><th scope="col">Band</th></tr></thead>
          <tbody>${bh.recent.map((r) => html`<tr class="${r.id === t.id ? "sel" : ""}"><td class="mono">${r.timestamp}</td><td><a class="mono" href="${txnHref(r.id)}">${r.id}</a></td>
            <td class="mono">${r.from}</td><td class="r num">${money(r.amount, r.currency)}</td><td>${bandPill(r.band)}</td></tr>`)}</tbody></table></div>` : ""}
      </section>
    </div>
    <section class="card" aria-labelledby="td-t">
      <div class="card-h"><h2 id="td-t">Timeline</h2></div>
      <ul class="log tl">
        <li><b>Payment</b> · <span class="mono small">${t.timestamp} UTC</span> · ${money(t.amount, t.currency)} to <span class="mono">${t.to_account}</span></li>
        ${(d.timeline || []).map((e) => html`<li><b>${words(e.type)}</b> · <span class="mono small">${e.at}</span>${e.analyst ? ` · ${e.analyst}` : ""}${e.action ? ` · ${words(e.action)}` : ""}
          ${e.decision ? ` · ${e.decision}` : ""}<div class="small muted mono">ledger #${e.seq} · ${String(e.hash ?? "").slice(0, 16)}…</div></li>`)}
      </ul>
      <p class="src">${d.timeline_source ?? ""}</p>
    </section></div>`;
  applyWidths(main);
  wireEvidence($(".txd", main));
}

async function renderDetail(main, ctx, id) {
  let d;
  try {
    d = await api("/v2/transactions/" + enc(id));
  } catch (e) {
    main.innerHTML = e.status === 404
      ? html`<div class="banner" role="alert">No stored payment has the id <span class="mono">${id}</span>. <a href="#/transactions">Search stored payments</a></div>`
      : html`<div class="banner err" role="alert">${e.explain || e.message}</div>`;
    return;
  }
  if (ctx.isCurrent()) detailView(main, d);
}

export async function render(main, ctx) {
  if (ctx.params[0]) return renderDetail(main, ctx, ctx.params[0]);
  return renderList(main, ctx);
}
