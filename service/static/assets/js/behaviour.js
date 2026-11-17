// D. Behavioural intelligence, each signal from its existing endpoint and marked consent-based:
//   keystroke check   POST /api/v2/behaviour/enrol and /check (typing captured in this browser, or synthetic)
//   session signals   read from stored /api/v2/assess alerts (assessed.context.session_detail); nothing is recomputed
//   identity check    POST /api/v2/identity/check
//   PRAMAAN           POST /api/v2/authenticate and /api/v2/challenge/verify
//   duress PIN        POST /api/v2/duress/register and /api/v2/payments/confirm
// Mouse dynamics has a module and an offline evaluation but no route, and is shown as not available.
import {api} from "./api.js";
import {$, $$, html, fixed, words} from "./util.js";

const enc = encodeURIComponent;
const DEC = {allow: "p-ok", challenge: "p-warn", block: "p-bad", deny_otp_reverify: "p-bad"};
const decPill = (d) => html`<span class="pill ${DEC[d] ?? "p-info"}" data-decision="${d}">${words(d)}</span>`;
const consent = html`<span class="pill p-demo plain" title="Consent-based demonstration">consent-based demonstration</span>`;
const err = (e) => html`<p class="msg err" role="alert">${e.status === 422 ? `Refused as invalid input (${e.detail})` : (e.explain || e.message)}</p>`;
const tabId = Math.random().toString(36).slice(2, 8);
const PHRASE = "the quick brown fox jumps over the lazy dog then reads the paper";

// ---- keystroke timing: key-down to key-down latency (ms) of consecutive letters, averaged per letter pair

function digraphsOf(events) {
  const acc = {};
  for (let i = 1; i < events.length; i++) {
    const [a, ta] = events[i - 1], [b, tb] = events[i], dt = tb - ta;
    if (!/^[a-z]$/.test(a) || !/^[a-z]$/.test(b) || dt <= 20 || dt >= 1500) continue;
    (acc[a + b] ||= []).push(dt);
  }
  return Object.fromEntries(Object.entries(acc).map(([k, v]) => [k, Math.round(v.reduce((x, y) => x + y, 0) / v.length)]));
}

/** Synthetic typing: a fixed per-pair rhythm for the "owner", jittered per session; an impostor types at another rhythm. */
function synthetic(kind, seed) {
  let s = seed;
  const rnd = () => ((s = (s * 1103515245 + 12345) % 2147483648) / 2147483648);
  const out = {}, w = PHRASE.replace(/ /g, "");
  for (let i = 1; i < w.length; i++) {
    const k = w[i - 1] + w[i];
    const base = 90 + ((k.charCodeAt(0) * 31 + k.charCodeAt(1) * 17) % 120);
    out[k] = Math.round(kind === "owner" ? base * (0.92 + 0.16 * rnd()) : (330 - base) * (1.3 + 0.6 * rnd()));
  }
  return out;
}

const typingResult = (r, what) => html`<div class="bh-res" data-typing="${r.anomaly ? "anomaly" : r.ready ? "match" : "not_ready"}">
  <p>${what}: ${r.ready ? (r.anomaly ? html`<span class="pill p-bad">typing unlike the profile</span>` : html`<span class="pill p-ok">typing like the profile</span>`)
    : html`<span class="pill">profile not ready</span>`}</p>
  <dl class="kv"><dt>Method</dt><dd>${r.method ?? "n/a"}</dd><dt>Score</dt><dd class="mono">${r.score === null || r.score === undefined ? "n/a" : fixed(r.score, 3)}</dd>
    <dt>Threshold</dt><dd class="mono">${r.threshold ?? "n/a"}</dd><dt>Median z</dt><dd class="mono">${r.median_z ?? "n/a"}</dd>
    <dt>Shared pairs</dt><dd class="mono">${r.pairs ?? "n/a"}</dd>${r.note ? html`<dt>Note</dt><dd>${r.note}</dd>` : ""}</dl>
  <p class="small muted">A supporting signal only: the backend uses it as the session's typing_anomaly and never holds a payment on it alone.</p></div>`;

function wireTyping(box) {
  let events = [], account = `demo-typing-${tabId}`;
  const input = $("#bh-type", box), out = $("#bh-tout", box), st = $("#bh-tstat", box);
  const status = (m) => { st.textContent = m; };
  input.addEventListener("keydown", (e) => { if (e.key.length === 1) events.push([e.key.toLowerCase(), performance.now()]); });
  input.addEventListener("input", () => { if (!input.value) events = []; });
  const send = async (path, dg, what) => {
    try {
      const r = await api(path, {method: "POST", body: {account, digraphs: dg}});
      if (path.endsWith("enrol")) {
        status(`${r.sessions} enrolment session(s) stored for ${account}; profile ${r.ready ? "ready" : "needs at least 3"}.`);
        out.innerHTML = html`<p class="msg ok small">${what}: enrolled (${Object.keys(dg).length} letter pairs).</p>`;
      } else out.innerHTML = typingResult(r, what);
      return r;
    } catch (e) { out.innerHTML = err(e); return null; }
  };
  const typed = () => {
    const dg = digraphsOf(events);
    if (Object.keys(dg).length < 5) { out.innerHTML = html`<p class="msg err" role="alert">Type the sentence first: ${Object.keys(dg).length} letter pairs captured, at least 5 are needed.</p>`; return null; }
    return dg;
  };
  $("#bh-enrol", box).addEventListener("click", async () => { const dg = typed(); if (dg) { await send("/v2/behaviour/enrol", dg, "Your typing"); input.value = ""; events = []; } });
  $("#bh-check", box).addEventListener("click", async () => { const dg = typed(); if (dg) { await send("/v2/behaviour/check", dg, "Your typing"); input.value = ""; events = []; } });
  $("#bh-synth", box).addEventListener("click", async () => {
    account = `demo-synthetic-${tabId}-${Date.now() % 100000}`;
    for (let i = 0; i < 3; i++) if (!(await send("/v2/behaviour/enrol", synthetic("owner", 11 + i), `Synthetic owner session ${i + 1}`))) return;
    const own = await api("/v2/behaviour/check", {method: "POST", body: {account, digraphs: synthetic("owner", 99)}}).catch((e) => { out.innerHTML = err(e); });
    const imp = await api("/v2/behaviour/check", {method: "POST", body: {account, digraphs: synthetic("impostor", 7)}}).catch((e) => { out.innerHTML = err(e); });
    if (own && imp) out.innerHTML = html`<div class="cols-eq grid">${typingResult(own, "Synthetic owner sample")}${typingResult(imp, "Synthetic impostor sample")}</div>`;
    status(`Synthetic profile ${account}: 3 sessions enrolled, then one owner-like and one impostor-like sample checked.`);
  });
  $("#bh-reset", box).addEventListener("click", () => { account = `demo-typing-${tabId}-${Date.now() % 100000}`; events = []; input.value = ""; status(`New empty profile ${account}.`); out.innerHTML = ""; });
}

// ---- session signals from stored alerts

async function drawSessions(box) {
  const out = $("#bh-sess", box);
  try {
    const alerts = await api("/v2/alerts");
    const rows = alerts.filter((a) => a.assessed?.context?.session_detail);
    out.innerHTML = rows.length ? html`<div class="table-wrap"><table id="bh-stable"><thead><tr><th>Alert</th><th>Payer</th><th class="r">Points</th><th>Cues</th><th class="r">Coercion index</th><th>Identity</th><th>Decision</th></tr></thead><tbody>
      ${rows.slice(0, 25).map((a) => { const d = a.assessed.context.session_detail; return html`<tr><td><a class="mono" href="#/alerts/${enc(a.alert_id)}">${a.alert_id}</a></td>
        <td class="mono">${a.payment?.from_bank}:${a.payment?.from_account}</td><td class="r mono">${d.points}</td>
        <td class="bh-wrap">${(d.cues || []).length ? d.cues.map((c) => html`<span class="pill plain">${words(c)}</span> `) : html`<span class="muted">none</span>`}</td>
        <td class="r mono">${d.coercion_index ?? "n/a"}</td><td>${d.identity ? decPill(d.identity.decision) : html`<span class="muted">n/a</span>`}</td>
        <td>${a.assessed.final_decision ?? "n/a"}${d.force_verify ? html` <span class="small muted">(verify forced)</span>` : ""}</td></tr>`; })}</tbody></table></div>
      <p class="src">${rows.length} stored alert(s) with session signals, newest 25 shown. Read from GET /api/v2/alerts; the signals were sent by the channel with POST /api/v2/assess (session object).</p>`
      : html`<p class="empty">No stored alert carries session signals. They arrive only when a channel sends the session object (call, screen share, remote app, pasted payee, SIM swap, typing) with POST /api/v2/assess.</p>`;
  } catch (e) { out.innerHTML = err(e); }
}

// ---- PRAMAAN

const PRESETS = {
  owner: {device_id: "phone-a", sim: "", amount: 640, call: false, share: false, submit: 20, events: 30},
  swap: {device_id: "attacker-phone", sim: 1, amount: 60000, call: false, share: false, submit: 2, events: 2},
  coached: {device_id: "phone-a", sim: "", amount: 45000, call: true, share: true, submit: 25, events: 40},
};

function pramaanView(r) {
  return html`<div class="bh-res" data-pramaan="${r.decision}"><p>${decPill(r.decision)} ${r.challenge_type ? html`challenge <b>${words(r.challenge_type)}</b>` : ""}
      ${r.rule ? html`<span class="small muted">rule ${r.rule}</span>` : ""}</p>
    <dl class="kv"><dt>Risk score</dt><dd class="mono">${r.risk_score}</dd><dt>Customer sees</dt><dd>${r.message_en}<br><span class="small" lang="hi">${r.message_hi}</span></dd>
      <dt>Reasons</dt><dd>${(r.reasons || []).length ? html`<ul class="sc-ul">${r.reasons.map((x) => html`<li>${x.text_en} <span class="small muted">(${x.family}, +${x.contribution})</span></li>`)}</ul>` : html`<span class="muted">none (allowed)</span>`}</dd>
      <dt>By family</dt><dd class="mono small">${Object.entries(r.family_contribution || {}).map(([k, v]) => `${k} ${v}`).join(" · ")}</dd>
      <dt>Model</dt><dd class="mono">${r.model_version ?? "n/a"} · ${r.latency_ms} ms · decision ${r.decision_id}</dd></dl>
    ${r.decision === "challenge" ? html`<div class="row-btns"><button class="btn" type="button" data-ch="${r.decision_id}" data-pass="1">Challenge passed</button>
      <button class="btn" type="button" data-ch="${r.decision_id}" data-pass="0">Challenge failed</button></div>` : ""}</div>`;
}

function wirePramaan(box) {
  const form = $("#bh-pf", box), out = $("#bh-pout", box), log = $("#bh-plog", box), outcomes = [];
  const user = `demo-user-${tabId}`;
  let clock = Math.floor(Date.now() / 1000);
  const remember = (what, r) => {
    outcomes.unshift(html`<li><span class="mono small">${new Date().toISOString().slice(11, 19)}</span> ${what}: ${decPill(r.decision)} ${r.challenge_type ? words(r.challenge_type) : ""} <span class="small muted">${r.rule ?? ""}</span></li>`);
    log.innerHTML = html`${outcomes.slice(0, 12)}`;
  };
  $$("[data-preset]", box).forEach((b) => b.addEventListener("click", () => {
    const p = PRESETS[b.dataset.preset];
    form.device_id.value = p.device_id; form.sim.value = p.sim; form.amount.value = p.amount; form.call.checked = p.call; form.share.checked = p.share;
    form.submit_s.value = p.submit; form.events.value = p.events;
  }));
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    clock += 3600; // each demo attempt is an hour after the last, so the account builds a history
    const sim = form.sim.value.trim();
    const body = {user_id: user, ts: clock,
      device: {device_id: form.device_id.value.trim(), platform: "android", sim_change_hours: sim === "" ? null : Number(sim), screen_share_active: form.share.checked},
      network: {ip: "49.229.31.106", city: "Mumbai", lat: 19.07, lon: 72.87},
      behaviour: {time_to_submit_s: Number(form.submit_s.value), input_events: Number(form.events.value), unknown_call_active: form.call.checked},
      transaction: {amount: Number(form.amount.value), payee_id: "demo-payee"}};
    try {
      const r = await api("/v2/authenticate", {method: "POST", body});
      out.innerHTML = pramaanView(r);
      remember(`payment ${form.amount.value} from ${form.device_id.value}`, r);
    } catch (x) { out.innerHTML = err(x); }
  });
  out.addEventListener("click", async (e) => {
    const b = e.target.closest("[data-ch]");
    if (!b) return;
    try {
      const r = await api("/v2/challenge/verify", {method: "POST", body: {decision_id: b.dataset.ch, passed: b.dataset.pass === "1"}});
      out.innerHTML = html`<p class="small">After the challenge ${b.dataset.pass === "1" ? "passed" : "failed"}:</p>${pramaanView(r)}`;
      remember("challenge result", r);
    } catch (x) { out.innerHTML = err(x); }
  });
}

// ---- identity check and duress

function wireIdentity(box) {
  $("#bh-if", box).addEventListener("submit", async (e) => {
    e.preventDefault();
    const f = e.target, out = $("#bh-iout", box), sim = f.sim.value.trim();
    const body = {device_hash: f.dev.value.trim() || null, enrolled_device_hashes: f.enrolled.value.split(",").map((x) => x.trim()).filter(Boolean),
      sim_swap_hours: sim === "" ? null : Number(sim)};
    try {
      const r = await api("/v2/identity/check", {method: "POST", body});
      out.innerHTML = html`<div class="bh-res" data-identity="${r.decision}"><p>${decPill(r.decision)} ${r.reason ? html`<span class="small">${r.reason}</span>` : ""}</p>
        <dl class="kv"><dt>New device</dt><dd>${String(r.new_device)}</dd><dt>Recent SIM swap</dt><dd>${String(r.recent_sim_swap)}</dd>
          <dt>OTP</dt><dd>${r.intervention?.otp ?? "sent as usual"}</dd>${r.intervention?.message ? html`<dt>Customer sees</dt><dd>${r.intervention.message.en}</dd>` : ""}</dl></div>`;
    } catch (x) { out.innerHTML = err(x); }
  });
}

function wireDuress(box) {
  const out = $("#bh-dout", box), f = $("#bh-df", box);
  const acct = () => f.account.value.trim();
  $("#bh-dreg", box).addEventListener("click", async () => {
    try {
      const r = await api("/v2/duress/register", {method: "POST", body: {account: acct(), normal_pin: f.normal.value, duress_pin: f.duress.value, trusted_contacts: ["family member"]}});
      out.innerHTML = html`<p class="msg ok small">Registered ${r.account} with ${r.trusted_contacts} trusted contact(s). PINs are stored as salted PBKDF2 hashes in this server's memory.</p>`;
    } catch (x) { out.innerHTML = err(x); }
  });
  f.addEventListener("submit", async (e) => {
    e.preventDefault();
    try {
      const r = await api("/v2/payments/confirm", {method: "POST", body: {account: acct(), pin: f.pin.value}});
      out.innerHTML = html`<div class="bh-res" data-duress="${r.result}"><p>Result: <b>${words(r.result)}</b></p>
        ${r.result === "duress" ? html`<dl class="kv"><dt>Customer sees</dt><dd>${r.shown_to_customer}</dd><dt>Funds</dt><dd>${r.funds}</dd>
          <dt>Trusted contacts alerted</dt><dd>${r.trusted_contacts_alerted}</dd><dt>Fraud desk</dt><dd>${r.fraud_desk}</dd></dl>`
          : r.result === "not_registered" ? html`<p class="small muted">Register the account's PINs first.</p>` : ""}</div>`;
    } catch (x) { out.innerHTML = err(x); }
  });
}

export async function render(main, ctx) {
  const acct = `DEMO-${tabId.toUpperCase()}`;
  main.innerHTML = html`<div class="banner demo" role="note">${consent} Everything on this page is a demonstration that you start yourself. Typing timings,
      device ids and PINs entered here are sent only to this RuseTect server and kept in its memory (lost on restart); no real customer data is shown.</div>
  <div class="grid cols-eq">
    <section class="card" aria-labelledby="bh-kh"><div class="card-h"><h2 id="bh-kh">Keystroke check</h2>${consent}</div>
      <p class="small">Type the sentence below. Only key-down times of letters are captured (no text is sent): the browser sends the mean time between each pair of consecutive letters.
        Enrol three samples, then check a fourth. Or run the synthetic demonstration.</p>
      <p class="mono small">${PHRASE}</p>
      <label class="field">Type here<textarea id="bh-type" rows="2" autocomplete="off" autocapitalize="off" spellcheck="false"></textarea></label>
      <div class="row-btns mt"><button class="btn" id="bh-enrol" type="button">Enrol this sample</button><button class="btn primary" id="bh-check" type="button">Check this sample</button>
        <button class="btn" id="bh-synth" type="button">Run synthetic demo</button><button class="btn" id="bh-reset" type="button">New profile</button></div>
      <p class="small muted" id="bh-tstat" role="status">Profile demo-typing-${tabId}: no session enrolled.</p>
      <div id="bh-tout" aria-live="polite"></div>
      <p class="src">POST /api/v2/behaviour/enrol and /check. Verifier trained and tested on the Aalto 136M Keystrokes data (results/aalto_eval.json).</p></section>
    <section class="card" aria-labelledby="bh-ph"><div class="card-h"><h2 id="bh-ph">PRAMAAN payment authentication</h2>${consent}</div>
      <p class="small">Account-takeover check for a payment: allow, challenge (which one) or block, with reasons. Each attempt is for demo user <span class="mono">demo-user-${tabId}</span>, an hour after the previous one.</p>
      <p class="row-btns"><button class="btn" type="button" data-preset="owner">Owner, usual phone</button><button class="btn" type="button" data-preset="swap">New phone after SIM swap</button>
        <button class="btn" type="button" data-preset="coached">On a call, screen shared</button></p>
      <form id="bh-pf" class="form"><label class="field">Device id<input type="text" name="device_id" value="phone-a" maxlength="64" required></label>
        <label class="field">Amount<input type="text" name="amount" value="640" inputmode="decimal"></label>
        <label class="field">SIM changed (hours ago, blank if not)<input type="text" name="sim" inputmode="decimal"></label>
        <label class="field">Seconds to submit<input type="text" name="submit_s" value="20" inputmode="decimal"></label>
        <label class="field">Input events<input type="text" name="events" value="30" inputmode="numeric"></label>
        <span class="field"><label><input type="checkbox" name="call"> unknown call active</label><label><input type="checkbox" name="share"> screen share active</label></span>
        <div class="row-btns full"><button class="btn primary" type="submit">Authenticate payment</button></div></form>
      <div id="bh-pout" aria-live="polite"></div>
      <h3 class="mt">Outcomes in this tab</h3><ul class="log" id="bh-plog"><li class="muted">None yet. The server keeps no PRAMAAN decision history to list.</li></ul>
      <p class="src">POST /api/v2/authenticate, POST /api/v2/challenge/verify.</p></section>
    <section class="card" aria-labelledby="bh-ih"><div class="card-h"><h2 id="bh-ih">Identity check before an OTP</h2>${consent}</div>
      <form id="bh-if" class="form"><label class="field">Device hash<input type="text" name="dev" value="device-new" maxlength="128"></label>
        <label class="field">Enrolled device hashes (comma separated)<input type="text" name="enrolled" value="device-home" maxlength="500"></label>
        <label class="field">SIM swap (hours ago, blank if none)<input type="text" name="sim" value="5" inputmode="decimal"></label>
        <div class="row-btns full"><button class="btn primary" type="submit">Check identity</button></div></form>
      <div id="bh-iout" aria-live="polite"></div><p class="src">POST /api/v2/identity/check.</p></section>
    <section class="card" aria-labelledby="bh-dh"><div class="card-h"><h2 id="bh-dh">Duress PIN</h2>${consent}</div>
      <p class="small">A customer forced to pay enters the duress PIN: they see success, the money is held and trusted contacts are alerted.</p>
      <form id="bh-df" class="form"><label class="field">Account<input type="text" name="account" value="${acct}" maxlength="64"></label><span></span>
        <label class="field">Normal PIN<input type="text" name="normal" value="2580" inputmode="numeric" maxlength="12"></label>
        <label class="field">Duress PIN<input type="text" name="duress" value="0852" inputmode="numeric" maxlength="12"></label>
        <div class="row-btns full"><button class="btn" type="button" id="bh-dreg">Register PINs</button></div>
        <label class="field">PIN to confirm a payment<input type="text" name="pin" value="0852" inputmode="numeric" maxlength="12"></label>
        <div class="row-btns"><button class="btn primary" type="submit">Confirm payment</button></div></form>
      <div id="bh-dout" aria-live="polite"></div><p class="src">POST /api/v2/duress/register, POST /api/v2/payments/confirm.</p></section>
  </div>
  <section class="card mt" aria-labelledby="bh-sh"><div class="card-h"><h2 id="bh-sh">Session signals on stored alerts</h2></div><div id="bh-sess"><p class="muted small">Loading…</p></div></section>
  <section class="card mt" aria-labelledby="bh-uh"><div class="card-h"><h2 id="bh-uh">Not available</h2></div>
    <ul class="checks" id="bh-na">
      <li data-na="mouse"><span class="pill">not available</span> <span><strong>Mouse dynamics</strong>: module exists, not served. <span class="small muted">crates/rusetect/src/mouse_dynamics.rs scores a session with a per-customer ExtraTrees model trained against other people's actions, and it was evaluated offline on the Balabit desktop data (results/balabit_eval.json). The server ships no enrolment sessions and no impostor population to fit such a model, and the evaluation is desktop mouse data, not phone touch, so no route serves it.</span></span></li>
      <li data-na="aggregates"><span class="pill">not available</span> <span><strong>Behaviour summary counts</strong>: no endpoint aggregates typing checks, PRAMAAN decisions, identity checks or duress events.</span></li>
      <li data-na="history"><span class="pill">not available</span> <span><strong>PRAMAAN and typing history</strong>: the server keeps account state in memory for scoring but no route lists past decisions; profiles are lost on restart.</span></li>
    </ul></section>`;
  wireTyping(main);
  wirePramaan(main);
  wireIdentity(main);
  wireDuress(main);
  await drawSessions(main);
  if (!ctx.isCurrent()) return;
}
