const SAMPLES = {"mule": {"income": 0.9, "name_email_similarity": 0.0452381698427012, "prev_address_months_count": -1, "current_address_months_count": 84, "customer_age": 40, "days_since_request": 0.0076979065840055, "intended_balcon_amount": -0.5697656791958139, "payment_type": "AC", "zip_count_4w": 2539, "velocity_6h": 3655.14740343208, "velocity_24h": 3798.573074800848, "velocity_4w": 3107.0505436563453, "bank_branch_count_8w": 0, "date_of_birth_distinct_emails_4w": 1, "employment_status": "CA", "credit_risk_score": 249, "email_is_free": 1, "housing_status": "BA", "phone_home_valid": 0, "phone_mobile_valid": 1, "bank_months_count": -1, "has_other_cards": 0, "proposed_credit_limit": 2000.0, "foreign_request": 0, "source": "INTERNET", "session_length_in_minutes": 3.102063265364043, "device_os": "windows", "keep_alive_session": 0, "device_distinct_emails_8w": 1, "device_fraud_count": 0}, "genuine": {"income": 0.4, "name_email_similarity": 0.8707952377237682, "prev_address_months_count": 46, "current_address_months_count": 11, "customer_age": 40, "days_since_request": 0.007769235283125, "intended_balcon_amount": -1.1096952670520477, "payment_type": "AB", "zip_count_4w": 717, "velocity_6h": 2969.442803950711, "velocity_24h": 3663.957943681082, "velocity_4w": 3039.0498465342584, "bank_branch_count_8w": 1, "date_of_birth_distinct_emails_4w": 9, "employment_status": "CD", "credit_risk_score": 77, "email_is_free": 0, "housing_status": "BC", "phone_home_valid": 1, "phone_mobile_valid": 0, "bank_months_count": 2, "has_other_cards": 1, "proposed_credit_limit": 200.0, "foreign_request": 0, "source": "INTERNET", "session_length_in_minutes": 4.513869524714286, "device_os": "linux", "keep_alive_session": 1, "device_distinct_emails_8w": 1, "device_fraud_count": 0}};
const $ = id => document.getElementById(id);
const nf = n => Number(n).toLocaleString("en-US");
const pct = (a, b) => b ? (100 * a / b).toFixed(1) + "%" : "n/a";
let running = false, totals = {scored: 0, L1: 0, L2: 0, med: 0, high: 0};

async function api(path, opts = {}) {
  const r = await fetch(path, {method: opts.method || "GET", headers: {"Content-Type": "application/json"}, body: opts.body ? JSON.stringify(opts.body) : undefined});
  if (!r.ok) { const b = await r.json().catch(() => ({})); throw new Error(typeof b.detail === "string" ? b.detail : `${path} returned ${r.status}`); }
  return r.json();
}

async function boot() {
  try { const h = await api("/api/health"); $("status").textContent = h.engine_loaded ? "Engine: models loaded" : "Engine: models missing"; }
  catch (e) { $("status").textContent = "Engine: API not reachable"; }
}

$("btnReset").onclick = async () => {
  $("btnReset").disabled = true; $("status").textContent = "Ingesting earlier payments as account history (up to ~2 min for the full history)…";
  let r;
  try { r = await api("/api/v2/demo/reset", {method: "POST"}); }
  catch (e) { $("status").textContent = "Reset refused: " + e.message; $("btnReset").disabled = false; return; }
  totals = {scored: 0, L1: 0, L2: 0, med: 0, high: 0}; render({truth: {tp: 0, fp: 0, pos: 0}, feed: [], cases: [], position: 0, total: r.remaining});
  $("status").textContent = `History loaded: ${nf(r.history_ingested)} payments (${r.source}) in ${r.seconds}s`;
  $("btnRun").disabled = false; $("btnReset").disabled = false;
};
$("btnRun").onclick = () => { running = true; $("btnRun").disabled = true; $("btnPause").disabled = false; $("btnReset").disabled = true; loop(); };
$("btnPause").onclick = () => { running = false; $("btnPause").disabled = true; $("btnRun").disabled = false; $("btnReset").disabled = false; };

async function loop() {
  while (running) {
    const r = await api("/api/v2/demo/next?n=1500", {method: "POST"});
    totals.scored += r.scored; totals.L1 += r.exits.L1; totals.L2 += r.exits.L2; totals.med += r.bands.med; totals.high += r.bands.high;
    render(r); refreshDrift();
    if (r.done) { running = false; $("btnPause").disabled = true; $("btnReset").disabled = false; $("status").textContent = "Replay finished"; }
  }
}

function render(r) {
  $("fScored").textContent = nf(totals.scored); $("fL1").textContent = nf(totals.L1); $("fL2").textContent = nf(totals.L2);
  $("fMed").textContent = nf(totals.med); $("fBlock").textContent = nf(totals.high);
  $("fL1p").textContent = totals.scored ? `${pct(totals.L1, totals.scored)} approved by gatekeeper` : "approved by gatekeeper";
  $("mL1").style.width = totals.scored ? (100 * totals.L1 / totals.scored) + "%" : 0;
  $("fCases").textContent = nf(r.open_cases || 0);
  const T = r.truth; $("tPrec").textContent = pct(T.tp, T.tp + T.fp); $("tRec").textContent = pct(T.tp, T.pos);
  $("tTP").textContent = `${nf(T.tp)} / ${nf(T.pos)}`; $("tFP").textContent = nf(T.fp);
  $("tLat").textContent = r.latency_p50_ms != null ? r.latency_p50_ms.toFixed(2) + " ms" : "n/a";
  $("prog").style.width = r.total ? (100 * r.position / r.total) + "%" : 0;
  if (r.sim_time) $("simtime").textContent = `sim time ${r.sim_time} UTC · ${nf(r.position)} of ${nf(r.total)}`;
  renderFeed(r.feed || []); renderCases(r.cases || []);
}

function renderFeed(feed) {
  if (!feed.length) return;
  $("feed").innerHTML = feed.map(a => {
    const cls = a.decision.startsWith("Block") ? "block" : a.decision.startsWith("Hold") ? "hold" : "verify";
    return `<div class="alert"><div><div class="route"><span class="tag">demo</span>${esc(a.from)} → ${esc(a.to)}</div><div class="meta">${esc(a.ts)} · ${nf(a.amount)} ${esc(a.currency)} · ${esc(a.format)} · risk ${a.risk.toFixed(3)}${a.case_id ? " · " + esc(a.case_id.replace("CASE-", "Case ")) : ""}</div></div>
      <div style="display:flex;flex-direction:column;align-items:flex-end;gap:4px"><span class="pill ${cls}">${a.decision === "Step-up verification" ? "Verify customer" : esc(a.decision)}</span><span class="verdict ${a.truth ? "y" : "n"}">${a.truth ? "laundering ✓" : "not laundering"}</span></div>
      <div class="reasons">${a.reasons.map(x => `<span class="chip">${esc(x.feature)}: ${esc(x.display)}</span>`).join("")}</div></div>`;
  }).join("");
}

function renderCases(cases) {
  if (!cases.length) return;
  $("cases").innerHTML = cases.slice(0, 5).map(c => `<div class="case">${miniGraph(c.edges)}<div><b>${esc(c.case_id.replace("CASE-", "Case "))}</b><span>${c.alerts} blocked payments · ${c.accounts} accounts</span><span>${nf(c.total_amount)} moved · ${esc(c.formats.join(", "))}</span></div></div>`).join("");
}

function miniGraph(edges) {
  const ids = [...new Set(edges.flatMap(e => [e.from, e.to]))].slice(0, 14), W = 120, H = 84, R = 30;
  const pos = Object.fromEntries(ids.map((id, i) => { const a = 2 * Math.PI * i / ids.length - Math.PI / 2; return [id, [W / 2 + R * 1.45 * Math.cos(a), H / 2 + R * Math.sin(a)]]; }));
  const ink = getComputedStyle(document.documentElement).getPropertyValue("--ink-3").trim(), bad = getComputedStyle(document.documentElement).getPropertyValue("--bad").trim();
  const ls = edges.filter(e => pos[e.from] && pos[e.to]).map(e => `<line x1="${pos[e.from][0]}" y1="${pos[e.from][1]}" x2="${pos[e.to][0]}" y2="${pos[e.to][1]}" stroke="${ink}" stroke-width="1"/>`).join("");
  const ns = ids.map(id => `<circle cx="${pos[id][0]}" cy="${pos[id][1]}" r="3.6" fill="${bad}"/>`).join("");
  return `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="accounts in case">${ls}${ns}</svg>`;
}

async function onboard(kind) {
  const r = await api("/api/v2/onboard", {method: "POST", body: SAMPLES[kind]});
  const s = SAMPLES[kind];
  $("obres").innerHTML = `<b>${r.decision}</b><br>Risk ${r.risk.toFixed(4)} vs. threshold ${r.threshold.toFixed(4)} for age ${s.customer_age >= 50 ? "50+" : "under 50"} · device ${s.device_os} · housing ${s.housing_status} · name/email similarity ${s.name_email_similarity.toFixed(2)}<br><span style="color:var(--ink-3)">Dataset label: ${kind === "mule" ? "fraud" : "genuine"}</span>`;
}
// ---- L-1 lure detector
const MSG_SAMPLES = {
  "KYC block": "Dear SBI customer your account will be blocked today. Please update your PAN card immediately click http://sbi-kyc-update.in/verify",
  "Task job": "Hi, I am Neha from a recruitment team. Part time job: like YouTube videos and earn Rs 3,000 to Rs 8,000 daily from home. Join our Telegram task group.",
  "Voice clone": "Papa main police station mein hoon, mujhe pakad liya hai. Jaldi Rs 50,000 bhejo is number par 9876501234, mummy ko mat batana.",
  "Screen share": "Sir I am from your bank, to cancel the fraud transaction install AnyDesk and read out the OTP you just received.",
  "Genuine OTP": "482913 is your OTP for transaction of Rs 2,300 at MYNTRA on HDFC Bank card XX1234. Valid for 5 mins. Do not share this OTP with anyone.",
  "Genuine family": "Papa please send Rs 4,500 for the hostel fee, the last date is Friday. I will send the receipt.",
};
let lastMsg = null, payClock = 0;
$("msgSamples").innerHTML = Object.keys(MSG_SAMPLES).map(k => `<button class="s" data-k="${k}">${k}</button>`).join("");
$("msgSamples").onclick = e => { const k = e.target.dataset && e.target.dataset.k; if (k) { $("msgText").value = MSG_SAMPLES[k]; $("msgCheck").click(); } };
async function refreshDrift() {
  try {
    const d = await api("/api/v2/drift");
    $("driftBadge").className = "badge " + ({ok: "durable", warn: "mem", alert: "alert"}[d.status] || "mem");
    $("driftBadge").textContent = "drift: " + d.status + (d.warming_up ? " (warming up)" : "");
    const top = d.top_psi ? `top PSI ${esc(d.top_psi.name)} ${Number(d.top_psi.psi).toFixed(3)}` : "top PSI n/a";
    $("driftInfo").innerHTML = `${nf(d.window.size)} of ${nf(d.window.capacity)} recent payments · ${top} · ${d.reasons.length ? d.reasons.map(esc).join("; ") : "no threshold crossed"}`;
  } catch (err) { $("driftBadge").textContent = "drift: unavailable"; }
}

const esc = t => String(t).replace(/[&<>"]/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;"}[c]));

async function refreshFatigue() {
  const s = await api("/api/v2/alerts/summary");
  const sup = (s.suppressed_caution || 0) + (s.suppressed_duplicate || 0) + (s.suppressed_daily_cap || 0) + (s.suppressed_no_account || 0);
  $("fatigue").textContent = `Alert budget: ${s.messages || 0} messages scored · ${s.notified || 0} pushed · ${sup} held back (caution ${s.suppressed_caution || 0}, duplicate ${s.suppressed_duplicate || 0}, daily cap ${s.suppressed_daily_cap || 0}) · ${s.payments_escalated || 0} payments escalated`;
}

$("msgCheck").onclick = async () => {
  const text = $("msgText").value.trim(); if (!text) return;
  $("msgCheck").disabled = true;
  try {
    const r = await api("/api/v2/message", {method: "POST", body: {text, channel: "sms", timestamp: "2022-09-09 09:00", bank: "070", account: "DEMO-USER"}});
    lastMsg = r;
    const cls = r.band === "high" ? "block" : r.band === "caution" ? "hold" : "safe";
    const wf = r.workflow ? `<div><span class="wf">${esc(r.workflow.label)}</span><div class="desc">${esc(r.workflow.description)}</div></div>` : `<div class="desc">No scam workflow matched.</div>`;
    const chips = [...r.hard_signals.map(h => `<span class="chip hard">${esc(h.label)}</span>`), ...r.tactics.map(t => `<span class="chip">${esc(t.label)}</span>`)].join("");
    const ents = [...r.entities.upi_ids, ...r.entities.phones, ...r.entities.links].map(x => `<span class="chip">${esc(x)}</span>`).join("");
    const al = r.alert.notify ? `<div class="note bad"><b>Push warning sent:</b> ${esc(r.alert.message)}</div>` : `<div class="note">No push: ${esc(r.alert.reason)}</div>`;
    $("msgRes").innerHTML = `<div class="head"><span class="pill ${cls}">${r.band === "high" ? "High risk" : r.band === "caution" ? "Caution" : "Looks safe"}</span><span class="num">risk ${r.risk.toFixed(3)}</span><span class="status">model ${r.model_risk.toFixed(3)}</span></div>
      ${wf}${chips ? `<div class="reasons">${chips}</div>` : ""}${r.key_phrases.length ? `<div class="desc">Key phrases: ${r.key_phrases.map(esc).join(" · ")}</div>` : ""}
      ${ents ? `<div class="reasons"><span class="status">entities →</span>${ents}</div>` : ""}${al}`;
    $("msgPay").disabled = false; $("msgPayScam").disabled = !r.entities.upi_ids.length;
    refreshFatigue();
  } catch (e) { $("msgRes").textContent = "Message check failed: " + e.message; }
  $("msgCheck").disabled = false;
};

async function payAfter(toBank, toAcct) {
  payClock += 1;
  const t = `2022-09-09 ${String(10 + Math.floor(payClock / 60) % 12).padStart(2, "0")}:${String(payClock % 60).padStart(2, "0")}`;
  const r = await api("/api/v2/payment", {method: "POST", body: {timestamp: t, from_bank: "070", from_account: "DEMO-USER", to_bank: toBank, to_account: toAcct, amount_paid: 2500, payment_format: "ACH"}});
  const cls = r.band === "high" ? "bad" : r.band === "med" ? "warn" : "";
  const was = r.decision_before_lure ? ` (payment model alone: ${esc(r.decision_before_lure)})` : "";
  const lure = r.reasons.filter(x => /message/.test(x.feature)).map(x => `${esc(x.feature)}: ${esc(x.display)}`).join(" · ");
  $("msgRes").insertAdjacentHTML("beforeend", `<div class="note ${cls}"><b>Payment to ${esc(toAcct)} → ${esc(r.decision)}</b>${was}${lure ? "<br>" + lure : ""}${r.payer_prompt ? "<br><i>" + esc(r.payer_prompt) + "</i>" : ""}</div>`);
  refreshFatigue();
}
$("msgPay").onclick = () => payAfter("011", "NEW-PAYEE-" + Math.floor(Math.random() * 1e6));
$("msgPayScam").onclick = () => lastMsg && payAfter("upi", lastMsg.entities.upi_ids[0]);

$("obMule").onclick = () => onboard("mule"); $("obGen").onclick = () => onboard("genuine");
boot();

// ---- live feed: decisions for real webhook events, polled every 5 s while the tab is visible (no fake rows)
const ACT = {allow: "safe", verify: "verify", hold: "hold", block: "block"};
let liveTimer = null, liveBusy = false;
async function pollLive() {
  clearTimeout(liveTimer); liveTimer = null;
  if (document.hidden || liveBusy) return;  // a hidden tab costs no store commands; visibilitychange resumes it
  liveBusy = true;
  try {
    const r = await api("/api/v2/live");
    $("liveBadge").className = "badge " + (r.durable ? "durable" : "mem");
    $("liveBadge").textContent = r.durable ? "durable: " + r.store.kind : "not durable: in-memory";
    if (r.store.note) $("liveBadge").title = r.store.note;
    if (r.events.length) $("liveFeed").innerHTML = r.events.map(e => {
      const p = e.payment;
      return `<div class="alert"><div><div class="route"><span class="tag">live</span>${esc(p.from_bank)}·${esc(p.from_account)} → ${esc(p.to_bank)}·${esc(p.to_account)}</div>
        <div class="meta">${esc(e.received_at)} · event ${esc(e.event_id)} · ${nf(p.amount_paid)} ${esc(p.payment_currency)} · ${esc(p.payment_format)}${e.risk != null ? " · risk " + Number(e.risk).toFixed(3) : ""}${e.alert_id ? " · alert " + esc(e.alert_id) : ""}${e.pramaan ? " · PRAMAAN " + esc(e.pramaan.decision) : ""} · ${e.latency_ms} ms</div></div>
        <span class="pill ${ACT[e.action]}">${esc(e.action)}</span>
        <div class="reasons">${(e.reasons || []).slice(0, 3).map(x => `<span class="chip">${esc(x.feature)}: ${esc(x.display)}</span>`).join("")}</div></div>`;
    }).join("");
  } catch (err) { $("liveBadge").textContent = "live feed unavailable (" + err.message + ")"; }
  finally { liveBusy = false; }
  refreshDrift();
  if (!document.hidden) liveTimer = setTimeout(pollLive, 5000);
}
document.addEventListener("visibilitychange", () => { if (!document.hidden) pollLive(); });
pollLive();
