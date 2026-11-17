// C. Scam intelligence: check a message with the served scam-message model (POST /api/v2/message), show its band,
// risk, workflow, tactics, rule signals, entities and the key phrases the model itself returned (highlighted where they
// occur in the text; nothing here keeps its own phrase list), the lure-fusion outcome, the redacted message history
// (GET /api/v2/messages), payments the lure memory raised (stored alerts whose reasons carry its two lure features),
// scam campaigns (GET /api/v2/campaigns) and the model card (GET /api/v2/message-model).
import {api} from "./api.js";
import {$, html, raw, esc, fixed, num, pct, words} from "./util.js";

const enc = encodeURIComponent;
const BAND = {high: "p-bad", caution: "p-warn", safe: "p-ok"};
const bandPill = (b) => html`<span class="pill ${BAND[b] ?? ""}" data-band="${b}">${b ?? "n/a"}</span>`;
// the two reasons lure_fusion.rs adds to a payment it raised (crates/rusetect/src/lure_fusion.rs)
const LURE_FEATURES = ["Payer received a scam-like message", "Payee named in a scam message"];
// example inputs for the demonstration buttons (inputs only: the model decides what is risky in them)
const SAMPLES = {
  scam: "This is CBI cyber cell. You are under digital arrest for money laundering. Stay on the video call and transfer Rs 50,000 to the safe account kyc.help@okaxis immediately. Do not tell anyone.",
  benign: "Hi, the plumber is coming at 4 tomorrow. Can you leave the side gate open? See you at dinner.",
};

/** The text with each model key phrase marked where it occurs literally (case-insensitive, whole words). */
export function highlight(text, phrases) {
  const spans = [], found = new Set();
  for (const p of phrases || []) {
    const rx = new RegExp(`(?<![\\p{L}\\p{N}])${p.trim().split(/\s+/).map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("\\s+")}(?![\\p{L}\\p{N}])`, "giu");
    for (const m of text.matchAll(rx)) { spans.push([m.index, m.index + m[0].length]); found.add(p); }
  }
  spans.sort((a, b) => a[0] - b[0]);
  let out = "", at = 0;
  for (const [s, e] of spans) {
    if (s < at) continue; // overlapping phrase: the first one wins
    out += esc(text.slice(at, s)) + `<mark class="kp">${esc(text.slice(s, e))}</mark>`;
    at = e;
  }
  return {html: raw(out + esc(text.slice(at))), found};
}

function resultView(r, text, sent, model) {
  const hl = highlight(text, r.key_phrases);
  const ents = Object.entries(r.entities || {}).filter(([, v]) => Array.isArray(v) && v.length);
  const watch = r.band !== "safe" && ["upi_ids", "phones", "accounts"].some((k) => (r.entities?.[k] || []).length);
  return html`<div class="sc-res" data-result="${r.band}">
    <div class="sc-head">${bandPill(r.band)}
      <span class="kpi-mini"><b class="mono" id="sc-risk">${fixed(r.risk, 4)}</b> risk</span>
      <span class="kpi-mini"><b class="mono">${fixed(r.model_risk, 4)}</b> model probability</span>
      <span class="small muted">message <span class="mono">${r.message_id ?? "n/a"}</span> · channel ${r.channel}</span></div>
    <p class="small"><b>Recommended action (backend):</b> ${r.action}</p>
    <div class="sect"><h3>Text with the model's key phrases</h3>
      <p class="sc-text" id="sc-text">${hl.html}</p>
      ${(r.key_phrases || []).length ? html`<p class="small">Key phrases returned by the model (largest positive contributions):
        ${r.key_phrases.map((p) => html`<span class="pill plain ${hl.found.has(p) ? "p-warn" : ""}" data-kp="${p}">${p}</span> `)}</p>
        ${r.key_phrases.some((p) => !hl.found.has(p)) ? html`<p class="small muted">Unmarked phrases are the model's normalised n-grams (for example amounts become "₹ amount") and do not occur literally in the text.</p>` : ""}`
        : html`<p class="small muted">The model returned no key phrases (it returns them only for caution and high bands).</p>`}</div>
    <div class="sect"><h3>Evidence</h3><dl class="kv">
      <dt>Workflow</dt><dd>${r.workflow ? html`<b>${r.workflow.label}</b> (${r.workflow.matches} pattern matches): ${r.workflow.description}` : html`<span class="muted">none matched${r.band === "safe" ? " (not reported for the safe band)" : ""}</span>`}
        ${(r.other_workflows || []).length ? html`<br><span class="small">Also resembles: ${r.other_workflows.map((w) => w.label).join(", ")}</span>` : ""}</dd>
      <dt>Tactics</dt><dd>${(r.tactics || []).length ? r.tactics.map((t) => html`<span class="pill p-info plain">${t.label}</span> `) : html`<span class="muted">none</span>`}</dd>
      <dt>Rule signals</dt><dd>${(r.hard_signals || []).length ? html`<ul class="sc-ul">${r.hard_signals.map((h) => html`<li>${h.label}</li>`)}</ul>` : html`<span class="muted">none</span>`}
        <span class="small muted">A rule signal lifts the risk to at least the caution threshold.</span></dd>
      <dt>Entities</dt><dd>${ents.length ? ents.map(([k, v]) => html`<span class="small">${words(k)}: <span class="mono">${v.join(", ")}</span></span><br>`) : html`<span class="muted">none</span>`}</dd>
      <dt>Model</dt><dd>${model ? html`${model.meta?.model ?? "n/a"}; trained on ${model.meta?.trained_on ?? "n/a"}. Thresholds: caution ${model.thresholds?.caution}, high ${model.thresholds?.high}.
        <span class="small muted">The backend publishes no version string for this model.</span>` : html`<span class="muted">model description not available (GET /api/v2/messages was refused)</span>`}</dd>
    </dl></div>
    <div class="sect"><h3>Lure fusion</h3>
      <p class="small"><b>Customer warning:</b> ${r.alert?.notify ? "pushed" : "not pushed"} — ${r.alert?.reason ?? "n/a"}.</p>
      ${r.alert?.message ? html`<p class="msg ok small">${r.alert.message}</p>` : ""}
      ${r.band === "safe" ? html`<p class="small muted">A safe message is not remembered for later payments.</p>`
        : html`<ul class="sc-ul small">
          ${sent.account ? html`<li>Linked to payer <span class="mono">${sent.bank}:${sent.account}</span>: a later payment by this payer to a new payee, inside the lure memory's look-back window, is raised to at least a step-up and carries the reason "${LURE_FEATURES[0]}".</li>`
            : html`<li>No payer account given, so no payment of a customer is linked to this message.</li>`}
          ${watch ? html`<li>Its UPI ids, phone numbers and account numbers joined the payee watchlist: a payment to one of them inside the look-back window is raised to at least an analyst hold and carries the reason "${LURE_FEATURES[1]}".</li>` : html`<li>No payee identifier in the text, so nothing joined the payee watchlist.</li>`}</ul>`}
    </div></div>`;
}

const historyRows = (h) => (h.messages || []).map((m) => html`<tr>
  <td class="mono">${m.message_id}</td><td class="mono small">${m.at}</td><td>${bandPill(m.band)}</td><td class="r mono">${fixed(m.risk, 3)}</td>
  <td>${m.workflow ?? html`<span class="muted">none</span>`}</td><td class="sc-wrap">${m.text}</td><td>${m.notified ? "pushed" : html`<span class="muted">no</span>`}</td></tr>`);

async function drawHistory(box) {
  const f = new FormData($("#sc-hf"));
  const qs = new URLSearchParams({limit: "50"});
  if (f.get("q")) qs.set("q", f.get("q"));
  if (f.get("band")) qs.set("band", f.get("band"));
  const out = $("#sc-hist", box);
  try {
    const h = await api("/v2/messages?" + qs);
    out.innerHTML = html`<p class="small muted">${num(h.total)} matching of ${num(h.stored)} stored (at most ${h.capacity}); ${h.scope}. Text is redacted: identifiers are replaced by [PHONE], [UPI], [ACCOUNT] and the like.</p>
      ${(h.messages || []).length ? html`<div class="table-wrap"><table id="sc-htable"><thead><tr><th>Id</th><th>Scored at (UTC)</th><th>Band</th><th class="r">Risk</th><th>Workflow</th><th>Redacted text</th><th>Warning</th></tr></thead>
        <tbody>${historyRows(h)}</tbody></table></div>` : html`<p class="empty">No message matches. Check a message above to start the history.</p>`}`;
    return h;
  } catch (e) {
    out.innerHTML = html`<p class="msg err" role="alert">${e.explain || e.message}</p>`;
    return null;
  }
}

async function drawLinks(box) {
  const out = $("#sc-links", box);
  try {
    const [alerts, camps] = await Promise.all([api("/v2/alerts"), api("/v2/campaigns")]);
    const lured = alerts.map((a) => [a, (a.assessed?.payment?.reasons || []).filter((r) => LURE_FEATURES.includes(r.feature))]).filter(([, r]) => r.length);
    const msgCamps = camps.filter((c) => (c.linked_by || []).length);
    out.innerHTML = html`<div class="sect"><h3>Payments raised by the lure memory</h3>
      ${lured.length ? html`<div class="table-wrap"><table id="sc-lured"><thead><tr><th>Alert</th><th>Payer</th><th>Payee</th><th>Decision</th><th>Lure reason</th></tr></thead><tbody>
        ${lured.map(([a, rs]) => html`<tr><td><a class="mono" href="#/alerts/${enc(a.alert_id)}">${a.alert_id}</a></td>
          <td class="mono">${a.payment.from_bank}:${a.payment.from_account}</td><td class="mono">${a.payment.to_bank}:${a.payment.to_account}</td>
          <td>${a.assessed?.final_decision ?? a.shown_decision ?? "n/a"}</td><td>${rs.map((r) => html`${r.feature} (${r.display})<br>`)}</td></tr>`)}</tbody></table></div>`
        : html`<p class="small muted">No stored alert carries a lure reason yet. A payment is linked only when the backend's lure memory raised it (payer warned by a risky message, or payee named in one).</p>`}</div>
      <div class="sect"><h3>Scam campaigns</h3>
      ${camps.length ? html`<ul class="log">${camps.map((c) => html`<li><b class="mono">${c.campaign_id}</b> · ${c.size} alerts (${c.alerts.map((id) => html`<a class="mono" href="#/alerts/${enc(id)}">${id}</a> `)})
          · linked by ${(c.linked_by || []).map(words).join(", ") || "n/a"} · ${c.victims.length} payers, ${c.payees.length} payees
          ${c.sample_script ? html`<br><span class="small muted">Script (redacted): ${c.sample_script}</span>` : ""}</li>`)}</ul>`
        : html`<p class="small muted">No campaign: the backend links alerts into a campaign only by a shared payee, a payee identifier in a message, or near-identical scripts.</p>`}
      <p class="src">GET /api/v2/alerts, GET /api/v2/campaigns. ${msgCamps.length} campaign(s) with a stated link. A message in the history is not itself joined to an alert: the backend links messages to payments through the lure memory only.</p></div>`;
  } catch (e) {
    out.innerHTML = html`<p class="msg err" role="alert">${e.explain || e.message}</p>`;
  }
}

async function drawCard(box) {
  const out = $("#sc-card", box);
  try {
    const c = await api("/v2/message-model");
    const sd = c.served_decision || {};
    const row = (k, lab) => sd[k] ? html`<tr><td>${lab}</td>${["caution_or_high", "high"].map((b) => html`<td class="r mono">${pct(sd[k][b]?.precision)}</td><td class="r mono">${pct(sd[k][b]?.recall)}</td>`)}</tr>` : "";
    out.innerHTML = html`<p class="small">${c.task ?? ""}</p>
      <p class="banner demo small" role="note">${c.data?.note ?? "No data note in the evaluation file."}</p>
      <dl class="kv"><dt>Messages</dt><dd>${num(c.data?.total)} (${num(c.data?.scam)} scam; ${num(c.data?.india)} Indian, ${num(c.data?.uci)} UCI)</dd>
        <dt>Protocol</dt><dd>${c.protocol ?? "n/a"}</dd></dl>
      <div class="table-wrap mt"><table><thead><tr><th>Corpus</th><th class="r">Precision caution+</th><th class="r">Recall caution+</th><th class="r">Precision high</th><th class="r">Recall high</th></tr></thead>
        <tbody>${row("india", "Indian (synthetic)")}${row("uci", "UCI SMS (real)")}</tbody></table></div>
      <p class="src">GET /api/v2/message-model (results/message_model_eval.json). ${sd.note ?? ""}</p>`;
  } catch (e) {
    out.innerHTML = html`<p class="msg err" role="alert">${e.explain || e.message}</p>`;
  }
}

export async function render(main, ctx) {
  main.innerHTML = html`<div class="grid cols-2">
    <section class="card" aria-labelledby="sc-h">
      <div class="card-h"><h2 id="sc-h">Check a message</h2></div>
      <p class="small">Scored by the served scam-message model (POST /api/v2/message). Submit only text you are allowed to share: it is sent to this server only, and a redacted copy is kept in this server's message history.</p>
      <form id="sc-form" class="form" novalidate>
        <label class="field full">Message or call transcript (1 to 5,000 characters)<textarea name="text" id="sc-input" rows="5" maxlength="5000"></textarea></label>
        <label class="field">Channel<select name="channel">${["sms", "whatsapp", "email", "call"].map((c) => html`<option>${c}</option>`)}</select></label>
        <span></span>
        <label class="field">Payer bank (needed with an account)<input type="text" name="bank" maxlength="64" autocomplete="off"></label>
        <label class="field">Payer account (optional: links the message to this payer's later payments)<input type="text" name="account" maxlength="64" autocomplete="off"></label>
        <div class="row-btns full"><button class="btn primary" type="submit" id="sc-go">Check message</button>
          <button class="btn" type="button" data-sample="scam">Example: scam</button><button class="btn" type="button" data-sample="benign">Example: benign</button></div>
      </form>
      <div id="sc-out" aria-live="polite"></div>
    </section>
    <section class="card" aria-labelledby="sc-mc"><div class="card-h"><h2 id="sc-mc">Model card</h2></div><div id="sc-card"><p class="muted small">Loading…</p></div></section>
  </div>
  <section class="card mt" aria-labelledby="sc-hh"><div class="card-h"><h2 id="sc-hh">Message history</h2></div>
    <form id="sc-hf" class="toolbar"><label class="field grow">Search redacted text<input type="search" name="q" maxlength="200"></label>
      <label class="field">Band<select name="band"><option value="">any</option><option>high</option><option>caution</option><option>safe</option></select></label>
      <button class="btn" type="submit">Filter</button></form>
    <div id="sc-hist"><p class="muted small">Loading…</p></div></section>
  <section class="card mt" aria-labelledby="sc-lh"><div class="card-h"><h2 id="sc-lh">Linked payments and campaigns</h2></div><div id="sc-links"><p class="muted small">Loading…</p></div></section>`;
  const box = main;
  let model = null;
  const hist = await drawHistory(box);
  if (!ctx.isCurrent()) return;
  model = hist?.model ?? null;
  drawCard(box);
  drawLinks(box);
  $("#sc-hf", box).addEventListener("submit", (e) => { e.preventDefault(); drawHistory(box); });
  box.addEventListener("click", (e) => {
    const s = e.target.closest("[data-sample]");
    if (s) $("#sc-input", box).value = SAMPLES[s.dataset.sample];
  });
  $("#sc-form", box).addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = new FormData(e.target), out = $("#sc-out", box), go = $("#sc-go", box);
    const text = String(f.get("text") || "");
    if (!text.trim()) { out.innerHTML = html`<p class="msg err" role="alert">Enter a message to check: the text is empty.</p>`; return; }
    const sent = {text, channel: f.get("channel"), bank: String(f.get("bank") || "").trim(), account: String(f.get("account") || "").trim()};
    const body = {text, channel: sent.channel};
    if (sent.account && !sent.bank) { out.innerHTML = html`<p class="msg err" role="alert">Give the payer's bank as well: the backend links a message to a payer by bank and account.</p>`; return; }
    if (sent.account) Object.assign(body, {account: sent.account, bank: sent.bank});
    go.disabled = true;
    out.innerHTML = html`<p class="muted small" role="status">Scoring…</p>`;
    try {
      const r = await api("/v2/message", {method: "POST", body});
      if (!model) model = (await drawHistory(box))?.model ?? null; else drawHistory(box);
      out.innerHTML = resultView(r, text, sent, model);
      drawLinks(box);
    } catch (err) {
      out.innerHTML = html`<p class="msg err" role="alert">Not scored: ${err.status === 422 ? `the input was refused (${err.detail})` : (err.explain || err.message)}</p>`;
    } finally {
      go.disabled = false;
    }
  });
}
