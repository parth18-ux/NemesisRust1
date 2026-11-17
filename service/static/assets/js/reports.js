// I. Reports and evidence: a printable investigation report (browser print to PDF; no PDF library) built at render time
// from GET /api/v2/investigations/{id}, the transaction detail and evidence pack of each alert, the narrative draft and
// GET /api/v2/audit. The verification section asks the server: POST /api/v2/verify for each pack's signature and GET
// /api/v2/audit for the ledger chain. "Verified" and "Chain valid" are shown only when those calls succeed and say so;
// otherwise "Not verified" with the reason. "Download JSON" saves exactly the data the report shows.
import {api} from "./api.js";
import {$, html, money, fixed, words, bandPill, prioPill} from "./util.js";
import {evidenceRow, wireEvidence, save} from "./evidence.js";
import {narrativeBody, legend, NOTICE} from "./assistant.js";
import {miniNetwork} from "./network.js";

const enc = encodeURIComponent;
const t = (x) => (x ? String(x).replace("T", " ").replace(/Z$/, " UTC") : "");
const reason = (e) => e?.explain || e?.message || String(e);

async function renderList(main, ctx) {
  let r;
  try { r = await api("/v2/investigations?limit=500"); } catch (e) { main.innerHTML = html`<div class="banner err" role="alert">${reason(e)}</div>`; return; }
  if (!ctx.isCurrent()) return;
  const all = r.investigations || [];
  main.innerHTML = html`<section class="card" aria-labelledby="rp-h">
    <div class="card-h"><h2 id="rp-h">Investigation reports</h2></div>
    <p class="small">A printable report per investigation: case details, payments, relationships, model outputs and signals, evidence provenance, analyst
      notes, the audit history and the narrative draft, with a verification section checked against the server when the report is opened.
      STR drafts and evidence packs are in each report and in the case workspace.</p>
    <div class="table-wrap"><table><thead><tr><th scope="col">Case</th><th scope="col">Title</th><th scope="col">Status</th><th scope="col">Priority</th>
      <th scope="col" class="r">Alerts</th><th scope="col">Updated</th><th scope="col">Report</th></tr></thead>
      <tbody>${all.length ? all.map((c) => html`<tr><td class="mono">${c.case_id}</td><td class="wrap">${c.title}</td><td>${words(c.status)}</td><td>${prioPill(c.priority)}</td>
        <td class="r num">${(c.alert_ids || []).length}</td><td class="mono small">${t(c.updated_at)}</td>
        <td><a class="btn" href="#/reports/${enc(c.case_id)}">Open report</a></td></tr>`)
      : html`<tr><td colspan="7" class="empty">No investigation yet: open one from Alert management.</td></tr>`}</tbody></table></div>
    <p class="src">Source: GET /api/v2/investigations. Other reports: <a href="/report">benchmark report</a>, <a href="/docs">API docs</a>.</p></section>`;
}

/** Everything the report shows, fetched now. Failures are kept as reasons, never replaced by a guess. */
async function gather(id) {
  const d = await api("/v2/investigations/" + enc(id));
  const alerts = (d.alerts || []).filter((a) => !a.missing);
  const settle = (ps) => Promise.allSettled(ps).then((rs) => rs.map((r) => (r.status === "fulfilled" ? {ok: r.value} : {err: reason(r.reason)})));
  const [txns, packs, [narr], [audit]] = await Promise.all([
    settle(alerts.map((a) => api("/v2/transactions/" + enc(a.event_id || a.alert_id)))),
    settle(alerts.map((a) => api(`/v2/alerts/${enc(a.alert_id)}/evidence`))),
    settle([api(`/v2/investigations/${enc(id)}/narrative`)]),
    settle([api("/v2/audit")]),
  ]);
  // verification: the server checks each pack's Ed25519 signature over {pack_hash, alert_id}
  const verify = await settle(packs.map((p, i) => (p.ok
    ? api("/v2/verify", {method: "POST", body: {signed_fields: {pack_hash: p.ok.pack_hash, alert_id: alerts[i].alert_id}, signature: p.ok.signature?.signature || ""}})
    : Promise.reject(new Error(`evidence pack not loaded: ${p.err}`)))));
  return {d, alerts, txns, packs, narr, audit, verify, rendered_at: new Date().toISOString()};
}

function packVerdict(p, v) {
  if (v.ok?.valid === true) return {ok: true, text: `Verified: POST /api/v2/verify accepted the ${p.ok.signature?.alg} signature (key ${p.ok.signature?.key_id}).`};
  if (v.ok) return {ok: false, text: "Not verified: POST /api/v2/verify answered valid: false."};
  return {ok: false, text: `Not verified: ${v.err}`};
}
function chainVerdict(audit) {
  if (audit.ok?.chain_valid === true) return {ok: true, text: `Chain valid: GET /api/v2/audit re-hashed ${audit.ok.entries.length} ledger entries and found the chain intact.`};
  if (audit.ok) return {ok: false, text: "Not verified: GET /api/v2/audit answered chain_valid: false (an entry was changed or removed)."};
  return {ok: false, text: `Not verified: ${audit.err}`};
}

async function renderReport(main, ctx, id) {
  main.innerHTML = html`<p class="muted" role="status">Building the report of ${id}: reading the case, payments, evidence packs, narrative and ledger, and verifying…</p>`;
  let R;
  try { R = await gather(id); } catch (e) {
    main.innerHTML = e.status === 404 ? html`<div class="banner" role="alert">No investigation <span class="mono">${id}</span>. <a href="#/reports">All reports</a></div>`
      : html`<div class="banner err" role="alert">${reason(e)}</div>`;
    return;
  }
  if (!ctx.isCurrent()) return;
  const {d, alerts, txns, packs, narr, audit, verify} = R, inv = d.investigation;
  const mine = new Set(alerts.map((a) => a.alert_id));
  const ledger = audit.ok ? audit.ok.entries.filter((e) => e.case_id === inv.case_id || mine.has(e.alert_id)) : d.audit_entries || [];
  const chain = chainVerdict(audit);
  const verdicts = alerts.map((a, i) => packVerdict(packs[i], verify[i]));
  const allOk = chain.ok && verdicts.length > 0 && verdicts.every((v) => v.ok);
  // relationships: each account's role across the case's payments, plus the accounts one stored payment away
  const roles = new Map();
  alerts.forEach((a) => { const p = a.payment || {};
    for (const [side, rel] of [["from", "paid"], ["to", "received"]]) {
      const k = `${p[side + "_bank"]}/${p[side + "_account"]}`, r = roles.get(k) || {paid: 0, received: 0, amt_out: 0, amt_in: 0, alerts: new Set()};
      r[rel]++; r[side === "from" ? "amt_out" : "amt_in"] += p.amount_paid || 0; r.alerts.add(a.alert_id); roles.set(k, r);
    } });
  const near = new Map();
  txns.forEach((x) => { for (const r of x.ok?.related_accounts || []) if (!roles.has(r.account.replace(":", "/"))) near.set(r.account + "|" + r.relation, r); });
  const notes = [];
  for (const c of inv.comments || []) notes.push({at: c.at, who: c.analyst, text: c.text, where: "case comment"});
  if (inv.closure?.reason) notes.push({at: inv.closure.requested_at, who: inv.closure.requested_by, text: inv.closure.reason, where: "closure reason"});
  packs.forEach((p, i) => { for (const a of p.ok?.alert?.actions || []) if (a.note) notes.push({at: a.at || "", who: a.analyst, text: a.note, where: `alert ${alerts[i].alert_id}: ${words(a.action)}`}); });
  const data = {report: "RuseTect investigation report", case_id: inv.case_id, rendered_at: R.rendered_at, investigation: inv, alerts,
    transactions: txns.map((x) => x.ok ?? {error: x.err}), evidence_packs: packs.map((x) => x.ok ?? {error: x.err}),
    narrative: narr.ok ?? {error: narr.err}, audit_entries: ledger,
    verification: {ledger_chain: chain, packs: alerts.map((a, i) => ({alert_id: a.alert_id, pack_hash: packs[i].ok?.pack_hash ?? null, ...verdicts[i]}))}};

  main.innerHTML = html`<article class="rp" aria-labelledby="rp-title">
    <div class="rp-bar no-print"><a href="#/reports">&larr; All reports</a> <a href="#/investigations/${enc(id)}">Case workspace</a>
      <span class="sp"></span><button class="btn" type="button" id="rp-json">Download JSON</button>
      <button class="btn primary" type="button" id="rp-print">Print or save as PDF</button></div>
    <header class="rp-head">
      <p class="rp-kicker">RuseTect investigation report · for analyst and compliance review</p>
      <h2 id="rp-title"><span class="mono">${inv.case_id}</span> · ${inv.title}</h2>
      <p class="small">Rendered ${t(R.rendered_at)} from the stored case data. Nothing in this report blocked or changed a decision.</p>
      <p class="rp-verdict ${allOk ? "ok" : "bad"}" id="rp-verdict">${allOk ? "Evidence verified and ledger chain valid at render time." : "Not fully verified at render time: see Verification."}</p>
    </header>

    <section class="rp-s" aria-labelledby="rp1"><h3 id="rp1">1. Case details</h3>
      <dl class="kv">
        <dt>Status</dt><dd>${words(inv.status)}</dd><dt>Priority</dt><dd>${inv.priority}</dd>
        <dt>Assignee</dt><dd>${inv.assignee || "unassigned"}</dd><dt>Opened by</dt><dd>${inv.created_by}</dd>
        <dt>Summary</dt><dd>${inv.summary || "none"}</dd><dt>Alerts</dt><dd>${(inv.alert_ids || []).join(", ")}</dd>
      </dl></section>

    <section class="rp-s" aria-labelledby="rp2"><h3 id="rp2">2. Timestamps</h3>
      <dl class="kv"><dt>Opened</dt><dd class="mono">${t(inv.created_at)}</dd><dt>Last updated</dt><dd class="mono">${t(inv.updated_at)}</dd>
        <dt>SLA due</dt><dd class="mono">${t(inv.sla_due)}${inv.sla_breached ? " (breached)" : ""}</dd>
        <dt>Closure</dt><dd>${inv.closure ? html`requested ${t(inv.closure.requested_at)} by ${inv.closure.requested_by}${inv.closure.decided_at ? html`; ${inv.closure.approved_by ? "approved" : "rejected"} ${t(inv.closure.decided_at)} by ${inv.closure.approved_by || inv.closure.rejected_by}` : "; awaiting a second person"}` : "not requested"}</dd>
        <dt>Report rendered</dt><dd class="mono">${t(R.rendered_at)}</dd></dl></section>

    <section class="rp-s" aria-labelledby="rp3"><h3 id="rp3">3. Transaction references</h3>
      <div class="table-wrap"><table><thead><tr><th scope="col">Alert</th><th scope="col">Transaction</th><th scope="col">Payment time</th><th scope="col">From</th>
        <th scope="col">To</th><th scope="col" class="r">Amount</th><th scope="col">Decision shown</th><th scope="col">Status</th></tr></thead>
        <tbody>${alerts.map((a) => { const p = a.payment || {}; return html`<tr><td class="mono">${a.alert_id}</td>
          <td><a class="mono" href="#/transactions/${enc(a.event_id || a.alert_id)}">${a.event_id || a.alert_id}</a></td><td class="mono small">${p.timestamp}</td>
          <td class="mono">${p.from_bank}/${p.from_account}</td><td class="mono">${p.to_bank}/${p.to_account}</td>
          <td class="r num">${money(p.amount_paid, p.payment_currency)}</td><td>${a.shown_decision}</td><td>${words(a.status)}</td></tr>`; })}</tbody></table></div></section>

    <section class="rp-s" aria-labelledby="rp4"><h3 id="rp4">4. Entity relationships</h3>
      <p class="small">Accounts in this investigation's payments. A connection is not evidence of wrongdoing.</p>
      <ul class="rp-list">${[...roles].map(([k, r]) => html`<li><span class="mono">${k}</span>: ${r.paid ? `paid ${r.paid} (${money(r.amt_out)})` : ""}${r.paid && r.received ? "; " : ""}${r.received ? `received ${r.received} (${money(r.amt_in)})` : ""}
        <span class="small muted">· alerts ${[...r.alerts].join(", ")}</span></li>`)}</ul>
      ${near.size ? html`<p class="small">Accounts one stored payment away (from the transaction detail):</p>
        <ul class="rp-list small">${[...near.values()].slice(0, 30).map((r) => html`<li><span class="mono">${r.account}</span> ${r.relation}: ${r.payments} payment(s), ${money(r.total_amount)}, ${r.flagged_payments} flagged</li>`)}</ul>` : ""}
      <div id="rp-graph" class="ws-graph rp-graph" role="img" aria-label="Network snapshot of the accounts in this investigation"></div>
      <p class="small muted" id="rp-gmsg">Network snapshot from GET /api/v2/graph (depth 1 around each account above).</p></section>

    <section class="rp-s" aria-labelledby="rp5"><h3 id="rp5">5. Model outputs and detection signals</h3>
      <div class="table-wrap"><table><thead><tr><th scope="col">Alert</th><th scope="col" class="r">Model risk</th><th scope="col">Band</th><th scope="col">Model reasons</th>
        <th scope="col">Motifs</th><th scope="col">Rules fired (ruleset)</th></tr></thead>
        <tbody>${alerts.map((a, i) => { const x = txns[i].ok, ctxr = packs[i].ok?.alert?.assessed?.context || {};
          return html`<tr><td class="mono">${a.alert_id}</td>
            <td class="r">${x ? html`<span class="num">${fixed(x.risk?.score, 5)}</span>` : html`<span class="muted">${txns[i].err}</span>`}</td><td>${x ? bandPill(x.risk?.band) : ""}</td>
            <td class="small">${(x?.explanations?.model_reasons || []).map((r) => `${r.feature} = ${r.display}`).join("; ") || "none stored"}</td>
            <td class="small">${(x?.explanations?.motif_reasons || []).map((r) => r.display).join("; ") || "none"}</td>
            <td class="small">${(ctxr.reasons || []).join("; ") || "none"}${ctxr.ruleset ? ` (${ctxr.ruleset})` : ""}</td></tr>`; })}</tbody></table></div>
      <p class="src">Risk is the cascade's served score, not a fraud probability; rules come from the evidence pack.</p></section>

    <section class="rp-s" aria-labelledby="rp6"><h3 id="rp6">6. Evidence provenance</h3>
      <div class="table-wrap"><table><thead><tr><th scope="col">Alert</th><th scope="col">Pack hash (SHA-256)</th><th scope="col">Signing key</th><th scope="col">Ledger seqs</th></tr></thead>
        <tbody>${alerts.map((a, i) => { const p = packs[i].ok; return html`<tr><td class="mono">${a.alert_id}</td>
          <td class="mono small hash">${p ? p.pack_hash : html`<span class="muted">${packs[i].err}</span>`}</td><td class="mono small">${p?.signature?.alg ?? ""} ${p?.signature?.key_id ?? ""}</td>
          <td class="mono small">${(p?.audit_entries || []).map((e) => e.seq).join(", ")}</td></tr>`; })}</tbody></table></div>
      <div class="no-print"><p class="small">Evidence packs and STR drafts (template, for analyst review; never filed automatically):</p>
        <ul class="ev-list">${alerts.map((a) => evidenceRow(a.alert_id))}</ul></div></section>

    <section class="rp-s" aria-labelledby="rp7"><h3 id="rp7">7. Analyst notes</h3>
      ${notes.length ? html`<ul class="rp-list">${notes.map((n) => html`<li><b>${n.who}</b> <span class="small muted">${n.where}${n.at ? html` · <span class="mono">${t(n.at)}</span>` : ""}</span><div>${n.text}</div></li>`)}</ul>`
        : html`<p class="small muted">No analyst note recorded.</p>`}</section>

    <section class="rp-s" aria-labelledby="rp8"><h3 id="rp8">8. Narrative draft</h3>
      <p class="rp-notice"><b>${NOTICE}</b></p>
      ${narr.ok ? html`${legend()}${narrativeBody(narr.ok)}<p class="src">${narr.ok.generated_by} · digest <span class="mono">${narr.ok.digest}</span></p>`
        : html`<p class="msg err">The narrative could not be generated: ${narr.err}</p>`}</section>

    <section class="rp-s" aria-labelledby="rp9"><h3 id="rp9">9. Audit history</h3>
      <div class="table-wrap"><table><thead><tr><th scope="col" class="r">Seq</th><th scope="col">Time</th><th scope="col">Entry</th><th scope="col">By</th><th scope="col">Hash</th></tr></thead>
        <tbody>${ledger.map((e) => html`<tr><td class="r num">${e.seq}</td><td class="mono small">${t(e.at)}</td>
          <td>${words(e.action || e.type)} <span class="small muted mono">${e.case_id || e.alert_id || ""}</span></td><td>${e.analyst || "system"}</td>
          <td class="mono small hash">${e.hash}</td></tr>`)}</tbody></table></div>
      <p class="src">${audit.ok ? "Entries of this case and its alerts from GET /api/v2/audit." : `GET /api/v2/audit failed (${audit.err}); showing the case's own entries.`} Comment texts enter the ledger as SHA-256 hashes.</p></section>

    <section class="rp-s rp-verify" aria-labelledby="rp10" id="rp-verify"><h3 id="rp10">10. Verification (checked at render time)</h3>
      <p class="rp-v ${chain.ok ? "ok" : "bad"}" data-chain="${String(chain.ok)}"><b>${chain.ok ? "Chain valid" : "Not verified"}</b> · ${chain.text}</p>
      <ul class="rp-list">${alerts.map((a, i) => html`<li class="rp-v ${verdicts[i].ok ? "ok" : "bad"}" data-pack="${a.alert_id}" data-valid="${String(verdicts[i].ok)}"><b>${verdicts[i].ok ? "Verified" : "Not verified"}</b>
        · <span class="mono">${a.alert_id}</span> · ${verdicts[i].text}</li>`)}</ul>
      <p class="src">The signature check covers the pack hash and alert id the server signed. The server signs a pack when it is read, so this proves the pack
        came from this instance's key; it does not recompute the pack hash independently.</p></section>
  </article>`;
  const root = $(".rp", main);
  wireEvidence(root);
  $("#rp-print", root).addEventListener("click", () => window.print());
  $("#rp-json", root).addEventListener("click", () => save(`report-${inv.case_id}.json`, data));
  const seeds = [...roles.keys()].map((k) => { const [bank, account] = k.split("/"); return {bank, account}; });
  try {
    const g = await miniNetwork($("#rp-graph", root), seeds);
    if (g.failed) $("#rp-gmsg", root).append(` ${g.failed} account(s) could not be loaded.`);
  } catch (e) {
    $("#rp-graph", root).innerHTML = html`<p class="small muted">Network snapshot unavailable: ${reason(e)}</p>`;
  }
}

export async function render(main, ctx) {
  return ctx.params[0] ? renderReport(main, ctx, ctx.params[0]) : renderList(main, ctx);
}
