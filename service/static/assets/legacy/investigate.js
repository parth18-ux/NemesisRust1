const $ = id => document.getElementById(id);
let SEL = null, LAST = null, LANG = "en", CLOCK = Date.UTC(2022, 8, 9, 10, 0) / 60000;
async function api(path, opts = {}) {
  const r = await fetch(path, {method: opts.method || "GET", headers: {"Content-Type": "application/json", ...(opts.headers || {})}, body: opts.body ? JSON.stringify(opts.body) : undefined});
  if (!r.ok) { let d = r.status; try { d = (await r.json()).detail || d; } catch (e) {} throw new Error(d); }
  return r.json();
}
const ts = m => new Date(m * 60000).toISOString().slice(0, 16).replace("T", " ");
const esc = s => String(s ?? "").replace(/[&<>"]/g, c => ({"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;"}[c]));
const label = d => d === "Step-up verification" ? "Verify customer" : d === "Block + STR draft" ? "Hold + recommend block" : d;
const pillFor = d => /block/i.test(d) ? "p-high" : /Hold/.test(d) ? "p-med" : /Step|verif/i.test(d) ? "p-info" : "p-ok";
const nonce = () => (crypto.randomUUID ? crypto.randomUUID() : String(Math.random()).slice(2) + Date.now());
const authed = () => ({"X-Analyst-Key": $("who").value, "X-Nonce": nonce()});

const PRESETS = {
  arrest: {to: "SAFE07", amt: 185000, msg: "This is CBI cyber cell. A parcel in your name has drugs and your Aadhaar is linked to money laundering. You are under digital arrest. Stay on the video call and transfer the amount to the RBI safe account immediately. Do not tell anyone.",
           s: {sOnCall: 1, sVideo: 1, sCallMin: 95, sShare: 1, sPaste: 1, sHes: 35}},
  kyc: {to: "KYCFEE2", amt: 9999, msg: "Dear customer your KYC will expire today and your account will be blocked. Install AnyDesk so our support executive can update it, then pay the verification fee right now.",
        s: {sRemote: 1, sPaste: 1, sHidden: 60}},
  invest: {to: "VIP3", amt: 75000, msg: "Join our VIP trading group. Guaranteed returns of 20% weekly, double your money in a month. Pay the joining amount today itself.", s: {}},
  ato: {to: "DRAIN9", amt: 240000, msg: "", s: {sNewDev: 1, sChain: 1, sTyping: 1}},
  sim: {to: "", amt: 0, msg: "", s: {sNewDev: 1, sSim: 3}},
  genuine: {to: "PAPA1", amt: 1800, msg: "Hi beta, it's Papa. Please send 1800 for the electricity bill when you get a chance, thanks.", s: {sOnCall: 1, sCallMin: 4}},
  vendor: {to: "MULE01", amt: 15000, msg: "The finance office has revised the vendor details. Please remit the balance to the replacement beneficiary ahead of the afternoon cutoff.", s: {sOff: 1}},
};
const SFIELDS = ["sOnCall", "sVideo", "sCallMin", "sShare", "sRemote", "sPaste", "sHes", "sHidden", "sTyping", "sNewDev", "sSim", "sChain", "sOff"];
let PRESET = null;
document.querySelectorAll("[data-preset]").forEach(b => b.onclick = () => {
  const p = PRESETS[b.dataset.preset]; PRESET = b.dataset.preset;
  if (p.to) $("fTo").value = p.to; if (p.amt) $("fAmt").value = p.amt; $("fMsg").value = p.msg; $("fConsent").checked = true;
  SFIELDS.forEach(k => { const el = $(k); if (el.type === "checkbox") el.checked = !!p.s[k]; else el.value = p.s[k] ?? (k === "sSim" ? "" : 0); });
  $("aOut").textContent = PRESET === "sim" ? "SIM swap check runs at login, before the OTP is sent" : "preset loaded";
  document.querySelectorAll("[data-preset]").forEach(x => x.classList.toggle("primary", x === b));
});

function session() {
  if ($("sOff").checked) return null;
  const t = CLOCK, s = {on_call: $("sOnCall").checked, video_call: $("sVideo").checked, call_minutes: +$("sCallMin").value || 0, screen_share: $("sShare").checked,
    remote_access_app: $("sRemote").checked, payee_pasted: $("sPaste").checked, hesitation_s: +$("sHes").value || 0, app_hidden_s: +$("sHidden").value || 0,
    typing_anomaly: $("sTyping").checked, device_hash: $("sNewDev").checked ? "dev-new" : "dev-home", enrolled_device_hashes: ["dev-home"]};
  if ($("sSim").value !== "") s.sim_swap_hours = +$("sSim").value;
  if ($("sChain").checked) s.events = [{type: "password_reset", minute: t - 20}, {type: "new_device", minute: t - 14}, {type: "new_payee", minute: t - 6}, {type: "transfer", minute: t}];
  return s;
}

async function assess(p) { return api("/api/v2/assess", {method: "POST", body: p}); }

$("bReset").onclick = async () => { await api("/api/v2/demo/reset?warm=false", {method: "POST"}); SEL = null; LAST = null; $("resultPanel").hidden = true;
  $("card").innerHTML = '<div class="empty">Select an alert to see the evidence.</div>'; $("status").textContent = "state cleared"; refresh(); };
$("bMule").onclick = async () => {
  $("bMule").disabled = true; const F = await api("/api/v2/fixtures/mule"); let i = 0;
  for (const amt of F.deposits_inr) {
    const v = "V" + String(i + 1).padStart(2, "0"), t = CLOCK + i * F.gap_minutes;
    await assess({timestamp: ts(t), from_bank: "01" + String(i + 1).padStart(2, "0"), from_account: v, to_bank: F.mule.bank, to_account: F.mule.account, amount_paid: amt,
                  payment_currency: "Rupee", payment_format: "ACH", message: v === "V01" ? F.victim_message_for_V01 : null, consent: v === "V01"});
    await assess({timestamp: ts(t + 1), from_bank: F.mule.bank, from_account: F.mule.account, to_bank: F.collector.bank, to_account: F.collector.account,
                  amount_paid: Math.round(amt * F.forward_share * 100) / 100, payment_currency: "Rupee", payment_format: "ACH"});
    i++; $("status").textContent = `replayed ${2 * i} of ${2 * F.deposits_inr.length} payments`; await refresh();
  }
  CLOCK += F.deposits_inr.length * F.gap_minutes + 30; $("bMule").disabled = false;
};

$("bAssess").onclick = async () => {
  CLOCK += 2; const s = session();
  if (PRESET === "sim") {                                   // identity gate at login: before any payment
    const r = await api("/api/v2/identity/check", {method: "POST", body: s || {}});
    LAST = {identity: r}; showResult({risk_score: {score: r.decision === "deny_otp_reverify" ? 650 : r.decision === "step_up" ? 400 : 100,
      band: r.decision === "deny_otp_reverify" ? "analyst hold" : r.decision === "step_up" ? "verify" : "approve"},
      final_decision: r.decision === "deny_otp_reverify" ? "OTP withheld" : r.decision, context: {reasons: [r.reason || "no identity risk"]}, intervention: r.intervention});
    $("aOut").textContent = `identity check: ${r.decision.replace(/_/g, " ")}`; return;
  }
  const r = await assess({timestamp: ts(CLOCK), from_bank: "0900", from_account: $("fFrom").value, to_bank: "0200", to_account: $("fTo").value, amount_paid: +$("fAmt").value,
                          payment_currency: "Rupee", payment_format: "ACH", message: $("fMsg").value || null, consent: $("fConsent").checked,
                          use_intent_model: $("fModel").checked, session: s});
  LAST = r; showResult(r);
  $("aOut").textContent = `payment model: ${label(r.payment_decision)} · context: ${r.context_band} (${r.context.total} pts, ${r.context.session || 0} from the session) · final: ${label(r.shown_decision)}`;
  await refresh(); if (r.alert_id) show(r.alert_id);
};

function showResult(r) {
  $("resultPanel").hidden = false;
  const sc = r.risk_score.score; $("rScore").textContent = sc; $("rTick").style.left = `calc(${sc / 10}% - 1px)`;
  $("rBand").textContent = r.risk_score.band; $("rBand").className = "pill " + (sc >= 600 ? "p-high" : sc >= 300 ? "p-med" : "p-ok");
  $("rTime").textContent = r.time_to_decision_ms != null ? `decided in ${r.time_to_decision_ms} ms` : "";
  const reasons = (r.context && r.context.reasons) || [];
  $("rWhy").innerHTML = `<div class="label">Decision: ${esc(label(r.final_decision))}</div><ul>${reasons.slice(0, 8).map(x => `<li>${esc(x)}</li>`).join("") || "<li>no rule fired</li>"}</ul>`;
  $("rSig").textContent = r.signature ? `verdict signed ${r.signature.alg} · key ${r.signature.key_id} · sha256 ${r.signature.sha256.slice(0, 16)}…` : "";
  phone(r);
}

function phone(r) {
  const iv = r.intervention || {}; const P = $("phone");
  if (iv.show === "reverify") { P.innerHTML = `<h3>${LANG === "hi" ? "सुरक्षा जाँच" : "Security check"}</h3><div class="q">${esc(iv.message[LANG])}</div><div class="f">OTP: withheld</div>`; return; }
  if (iv.show !== "pause") { P.innerHTML = `<h3>${LANG === "hi" ? "भुगतान सफल" : "Payment sent"}</h3><div class="f">${LANG === "hi" ? "ग्राहक को कुछ अतिरिक्त नहीं दिखता।" : "The customer sees nothing extra."}</div>`; return; }
  const s = iv.screen[LANG];
  P.innerHTML = `<div class="label">${LANG === "hi" ? "RuseTect जाँच" : "RuseTect check"}</div><h3>${esc(s.title)}</h3><div class="q">${esc(s.question)}</div><div class="f">${esc(s.fact)}</div>
    <button class="primary" id="pStop">${esc(s.stop)}</button><button id="pGo">${esc(s.continue)}</button>
    <button id="pDuress" title="demo: the customer is forced to confirm and uses the duress PIN">${LANG === "hi" ? "दबाव में पिन डालें (डेमो)" : "Confirm under pressure with the duress PIN (demo)"}</button>
    <div class="f" id="pOut">${esc(iv.funds || "")}${iv.step_up && iv.step_up.length ? " · then " + esc(iv.step_up.join(", ")) : ""}</div>`;
  const aid = r.alert_id;
  $("pStop").onclick = async () => { if (aid) await api(`/api/v2/alerts/${aid}/customer`, {method: "POST", body: {answer: "stop"}}); $("pOut").textContent = LANG === "hi" ? "भुगतान रोका गया।" : "Payment stopped by the customer."; refresh(); };
  $("pGo").onclick = async () => { if (aid) await api(`/api/v2/alerts/${aid}/customer`, {method: "POST", body: {answer: "continue"}}); $("pOut").textContent = LANG === "hi" ? "30 मिनट बाद जारी होगा।" : "Released after 30 minutes unless an analyst acts."; refresh(); };
  $("pDuress").onclick = async () => {
    const acct = $("fFrom").value;
    try { await api("/api/v2/duress/register", {method: "POST", body: {account: acct, normal_pin: "2580", duress_pin: "0852", trusted_contacts: ["family member"]}}); } catch (e) {}
    const d = await api("/api/v2/payments/confirm", {method: "POST", body: {account: acct, pin: "0852", alert_id: aid}});
    P.innerHTML = `<h3>${esc(d.shown_to_customer || "Payment successful")}</h3><div class="f">${LANG === "hi" ? "स्कैमर को यही दिखता है।" : "This is what the scammer watching the screen sees."}</div>
      <div class="msg"><b>What really happened:</b> money ${esc(d.funds)}, ${d.trusted_contacts_alerted} trusted contact alerted, fraud desk: ${esc(d.fraud_desk)}.</div>`;
    refresh();
  };
}
document.querySelectorAll("[data-lang]").forEach(b => b.onclick = () => { LANG = b.dataset.lang; if (LAST) phone(LAST.identity ? {intervention: LAST.identity.intervention} : LAST); });

async function refresh() {
  const al = await api("/api/v2/alerts");
  $("nAlerts").textContent = al.length;
  $("alerts").innerHTML = al.length ? al.map(a => `<div class="al ${a.alert_id === SEL ? "sel" : ""}" data-id="${a.alert_id}">
      <div><div class="mono">${a.alert_id} · ${esc(a.payment.from_account)} → ${esc(a.payment.to_account)}</div>
      <div style="font-size:12px;color:var(--ink-2)">${esc(a.payment.timestamp)} · ₹${(+a.payment.amount_paid).toLocaleString("en-IN")} · ${esc(a.status.replace(/_/g, " "))}</div></div>
      <span class="pill ${pillFor(a.shown_decision)}">${esc(label(a.shown_decision))}</span></div>`).join("") : '<div class="empty">No alerts yet.</div>';
  document.querySelectorAll(".al").forEach(el => el.onclick = () => show(el.dataset.id));
  window._alerts = al; audit(); campaigns(); ledger();
}

function ring(sel, al) {
  const edges = al.map(a => [a.payment.from_account, a.payment.to_account, a.alert_id]);
  const comp = new Set([sel.payment.from_account, sel.payment.to_account]); let grew = true;
  while (grew) { grew = false; for (const [s, t] of edges) if ((comp.has(s) || comp.has(t)) && !(comp.has(s) && comp.has(t))) { comp.add(s); comp.add(t); grew = true; } }
  const E = edges.filter(([s, t]) => comp.has(s) && comp.has(t)), nodes = [...comp];
  const deg = Object.fromEntries(nodes.map(n => [n, E.filter(e => e[0] === n || e[1] === n).length]));
  const hub = nodes.slice().sort((a, b) => deg[b] - deg[a])[0], others = nodes.filter(n => n !== hub);
  const W = 520, H = 220, cx = W / 2, cy = H / 2, R = 90, pos = {[hub]: [cx, cy]};
  others.forEach((n, i) => { const a = 2 * Math.PI * i / Math.max(others.length, 1); pos[n] = [cx + 2.2 * R * Math.cos(a) * 0.95, cy + R * Math.sin(a)]; });
  const lines = E.map(([s, t, id]) => `<line x1="${pos[s][0]}" y1="${pos[s][1]}" x2="${pos[t][0]}" y2="${pos[t][1]}" stroke="${id === sel.alert_id ? "var(--bad)" : "var(--ink-3)"}" stroke-width="${id === sel.alert_id ? 2.5 : 1.2}"/>`).join("");
  const dots = nodes.map(n => `<circle cx="${pos[n][0]}" cy="${pos[n][1]}" r="${n === hub ? 9 : 6}" fill="${n === hub ? "var(--warn)" : "var(--panel)"}" stroke="var(--accent)" stroke-width="2"/><text x="${pos[n][0]}" y="${pos[n][1] - 12}" text-anchor="middle" font-size="10" fill="var(--ink-2)" font-family="monospace">${esc(n)}</text>`).join("");
  return `<svg viewBox="0 0 ${W} ${H}" width="100%" role="img" aria-label="Accounts linked to this alert">${lines}${dots}</svg>
    <div style="font-size:12px;color:var(--ink-3)">${nodes.length} accounts and ${E.length} alerted payments linked through shared accounts; red = this payment.</div>`;
}

async function show(id) {
  SEL = id; const a = (window._alerts || await api("/api/v2/alerts")).find(x => x.alert_id === id); if (!a) return;
  const x = a.assessed, c = x.context, P = x.payment;
  const bar = (name, v, max) => `<span>${name}</span><div class="bar"><div style="width:${100 * (v || 0) / max}%"></div></div><span class="mono">${v || 0} / ${max}</span>`;
  $("card").innerHTML = `
    <div class="row" style="justify-content:space-between;align-items:flex-start">
      <div><div class="label">${a.alert_id} · ${esc(a.payment.from_account)} → ${esc(a.payment.to_account)} · ₹${(+a.payment.amount_paid).toLocaleString("en-IN")}</div>
      <h2 style="margin:4px 0 0;font-size:24px">${esc(label(a.shown_decision))}</h2></div>
      <span class="pill ${pillFor(a.shown_decision)}">status: ${esc(a.status.replace(/_/g, " "))}</span></div>
    <div class="row" style="margin-top:8px"><span class="pill p-info">score ${x.risk_score ? x.risk_score.score : "?"} / 1000</span>
      <span class="pill p-info">payment model: ${esc(label(P.decision))} (risk ${P.risk ?? "L1 exit"})</span>
      <span class="pill ${c.band === "LOW" ? "p-ok" : c.band === "MEDIUM" ? "p-info" : "p-med"}">context: ${c.band} · ${c.total} pts</span>
      ${x.raised_by_context ? '<span class="pill p-med">raised by context veto</span>' : ""}</div>
    <div class="bars">${bar("message", c.context, 40)}${bar("payment", c.transaction, 30)}${bar("account graph", c.graph, 30)}${bar("session", c.session, 40)}</div>
    <div class="label">Why</div><ul>${[...c.reasons, ...(P.reasons || []).map(r => `model: ${r.feature} = ${r.display}`)].map(r => `<li>${esc(r)}</li>`).join("") || "<li>no reasons</li>"}</ul>
    ${c.redacted_text ? `<div class="label">Message (redacted, consented)</div><div class="msg">${esc(c.redacted_text)}</div>` : `<div class="label">Message</div><div class="msg">${c.consent ? "none" : "not used: no consent"}</div>`}
    <div class="label" style="margin-top:10px">Linked accounts</div>${ring(a, window._alerts || [])}
    <div class="row" style="margin-top:10px">
      <button data-act="release">Release</button><button data-act="verify">Verify customer</button><button data-act="hold">Hold</button><button data-act="approve_block">Approve block (needs 2 analysts)</button>
      <a href="/api/v2/alerts/${a.alert_id}/evidence" target="_blank" class="mono">signed evidence pack</a><button id="bStr">STR draft</button></div>
    <div class="mono" id="actOut" style="margin-top:6px;color:var(--ink-3)">acting as ${esc($("who").selectedOptions[0] ? $("who").selectedOptions[0].textContent : "?")} · ruleset ${esc(c.ruleset)} · risk points are rule outputs, not probabilities</div>
    <div id="strOut"></div>
    <div class="label" style="margin-top:10px">Actions</div><ul>${(a.actions || []).map(e => `<li>${esc(e.analyst)}: ${esc(e.action)}${e.note ? " · " + esc(e.note) : ""}</li>`).join("") || "<li>none yet</li>"}</ul>`;
  document.querySelectorAll("[data-act]").forEach(b => b.onclick = async () => {
    try { await api(`/api/v2/alerts/${a.alert_id}/review`, {method: "POST", body: {action: b.dataset.act}, headers: authed()}); await refresh(); show(a.alert_id); }
    catch (e) { $("actOut").textContent = "refused: " + e.message; }
  });
  $("bStr").onclick = async () => { const s = await api(`/api/v2/alerts/${a.alert_id}/str`);
    $("strOut").innerHTML = `<div class="label" style="margin-top:10px">${esc(s.status)}</div><pre class="str">${esc(s.summary)}\n\n${esc(JSON.stringify(s.sections, null, 1))}</pre>`; };
  refresh_sel();
}
function refresh_sel() { document.querySelectorAll(".al").forEach(el => el.classList.toggle("sel", el.dataset.id === SEL)); }

async function campaigns() {
  const cs = await api("/api/v2/campaigns"); $("nCamp").textContent = cs.length;
  if (!cs.length) return;
  $("campaigns").innerHTML = cs.slice(0, 8).map(c => `<div class="camp"><div class="row" style="justify-content:space-between"><b class="mono">${c.campaign_id}</b>
    <span class="pill p-med">${c.size} alerts · ${c.victims.length} payers · ₹${Math.round(c.total_amount).toLocaleString("en-IN")}</span></div>
    <div style="font-size:12px;color:var(--ink-2)">linked by: ${esc(c.linked_by.join(", "))} · payees ${esc(c.payees.join(", "))}</div>
    ${c.sample_script ? `<div class="msg" style="margin-top:6px">${esc(c.sample_script.slice(0, 220))}</div>` : ""}</div>`).join("");
}
async function ledger() {
  const L = await api("/api/v2/consortium");
  $("ledgerOk").textContent = L.entries.length ? (L.valid ? "signatures and chain valid ✓" : "ledger BROKEN") : "empty";
  $("ledger").innerHTML = L.entries.slice(-8).reverse().map(e => `<tr><td>${e.seq}</td><td>${esc(e.bank)}</td><td>${esc(e.registry.replace(/_/g, " "))}</td><td class="mono">${e.token.slice(0, 12)}…</td><td>${esc(e.reason)}</td></tr>`).join("");
  $("merkle").textContent = L.entries.length ? `Merkle root ${L.merkle_root.slice(0, 24)}… (publishable anchor; no account data leaves the bank)` : "";
}
$("bPublish").onclick = async () => {
  try { const e = await api("/api/v2/consortium/publish", {method: "POST", body: {bank: "BANK-B", identifier: "0200:" + $("fTo").value, reason: "confirmed mule at BANK-B"}, headers: authed()});
        $("pubOut").textContent = `BANK-B flagged payee ${$("fTo").value} (entry ${e.seq}); assess a payment to it to see the cross bank flag`; ledger(); }
  catch (e) { $("pubOut").textContent = "refused: " + e.message; }
};
async function audit() {
  const a = await api("/api/v2/audit");
  $("chain").textContent = a.chain_valid ? "chain valid ✓" : "chain BROKEN";
  $("audit").innerHTML = a.entries.slice(-12).reverse().map(e => `<tr><td>${e.seq}</td><td class="mono">${e.at.slice(11, 19)}</td><td>${esc(e.type.replace("_", " "))}</td><td class="mono">${esc(e.alert_id || "")}</td>
    <td>${esc(e.action || e.decision || "")} ${e.analyst ? "by " + esc(e.analyst) : e.actor ? "by " + esc(e.actor) : ""}</td><td class="mono">${e.hash.slice(0, 10)}…</td></tr>`).join("");
}
(async () => {
  const keys = await api("/api/v2/demo/analysts").catch(() => []);
  $("who").innerHTML = keys.map(k => `<option value="${esc(k.key)}">${esc(k.analyst)} (${esc(k.role)})</option>`).join("") || '<option value="">set an analyst key</option>';
  refresh();
})();
