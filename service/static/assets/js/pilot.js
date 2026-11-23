// J. Pilot health: the three signals a shadow pilot watches each day, read from existing endpoints only:
// drift (GET /api/v2/drift), alert volume per UTC day (GET /api/v2/dashboard?bucket=day) and decision latency
// (GET /api/v2/live, the latest 50 live decisions). Nothing here is computed from a fixed number; a missing source says so.
import {api} from "./api.js";
import {html, num, fixed, pct} from "./util.js";

const settle = (p) => p.then((v) => ({ok: true, v}), (e) => ({ok: false, e}));
const errNote = (r) => html`<div class="banner err" role="alert">${r.e?.explain || r.e?.message || "unavailable"}</div>`;
const PILL = {ok: "p-ok", warn: "p-warn", alert: "p-bad"};
const status = (s) => html`<span class="pill ${PILL[s] || ""}">${s ?? "n/a"}</span>`;

function quantile(xs, q) {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(q * s.length))];
}

function driftCard(r) {
  if (!r.ok) return html`<article class="card"><div class="card-h"><h2>Drift</h2></div>${errNote(r)}</article>`;
  const d = r.v, t = d.thresholds || {};
  const feats = Object.entries(d.features || {}).sort((a, b) => (b[1].psi ?? -1) - (a[1].psi ?? -1));
  return html`<article class="card" id="pl-drift">
    <div class="card-h"><h2>Drift</h2>${status(d.status)}${d.warming_up ? html`<span class="pill plain">warming up</span>` : ""}</div>
    ${d.reasons?.length ? html`<ul class="small">${d.reasons.map((x) => html`<li>${x}</li>`)}</ul>` : html`<p class="small muted">No drift reasons reported.</p>`}
    <div class="table-wrap"><table>
      <caption class="sr">Population stability index (PSI) of the score and each input against the reference</caption>
      <thead><tr><th scope="col">Signal</th><th scope="col" class="r">PSI</th><th scope="col" class="r">Payments</th><th scope="col">Status</th></tr></thead>
      <tbody>${[["score", d.score || {}], ...feats].map(([k, v]) => html`<tr><td class="mono">${k}</td><td class="num r">${fixed(v.psi)}</td>
        <td class="num r">${num(v.n)}</td><td>${status(v.status)}</td></tr>`)}</tbody></table></div>
    <div class="sect"><h3>Band rates against the reference</h3>
      <div class="table-wrap"><table><caption class="sr">Share of decisions in each band, this window against the reference</caption>
        <thead><tr><th scope="col">Band</th><th scope="col" class="r">Window</th><th scope="col" class="r">Reference</th><th scope="col" class="r">Ratio</th><th scope="col">Status</th></tr></thead>
        <tbody>${Object.entries(d.bands || {}).map(([b, v]) => html`<tr><td>${b}</td><td class="num r">${pct(v.rate, 2)}</td><td class="num r">${pct(v.reference, 2)}</td>
          <td class="num r">${v.ratio === null ? "n/a" : fixed(v.ratio, 2) + "x"}</td><td>${v.checked ? status(v.status) : html`<span class="muted">not checked</span>`}</td></tr>`)}</tbody></table></div></div>
    <p class="src">Source: GET /api/v2/drift, ${d.scope ?? ""}; window ${num(d.window?.size)} of ${num(d.window?.capacity)} payments (minimum ${num(d.window?.min_size)});
      PSI warn ${t.psi_warn}, alert ${t.psi_alert}; band ratio warn ${t.rate_ratio_warn}x, alert ${t.rate_ratio_alert}x; reference ${d.reference ?? ""}.
      Against the shipped synthetic reference a bank's traffic alerts from day one: rebuild it from your own validation period (PILOT_ONBOARDING.md section 9).</p>
  </article>`;
}

function volumeCard(r) {
  if (!r.ok) return html`<article class="card"><div class="card-h"><h2>Alert volume per day</h2></div>${errNote(r)}</article>`;
  const series = [...(r.v.activity?.series || [])].reverse();
  const a = r.v.alerts || {};
  return html`<article class="card" id="pl-volume">
    <div class="card-h"><h2>Alert volume per day</h2></div>
    <p class="small">Open alerts <b>${num(a.open?.value)}</b> · unassigned <b>${num(a.unassigned?.value)}</b> · total <b>${num(a.total?.value)}</b>.
      Compare each day's flagged count with the review budget agreed for the pilot (PILOT.md).</p>
    ${series.length ? html`<div class="table-wrap"><table>
      <caption class="sr">Stored decisions per UTC day by band; flagged is verify, hold and block</caption>
      <thead><tr><th scope="col">Day (UTC)</th><th scope="col" class="r">Decisions</th><th scope="col" class="r">Flagged</th><th scope="col" class="r">Verify</th>
        <th scope="col" class="r">Hold</th><th scope="col" class="r">Block</th></tr></thead>
      <tbody>${series.map((p) => html`<tr><td class="mono">${String(p.t).slice(0, 10)}</td><td class="num r">${num(p.total)}</td>
        <td class="num r">${num((p.verify || 0) + (p.hold || 0) + (p.block || 0))}</td><td class="num r">${num(p.verify || 0)}</td>
        <td class="num r">${num(p.hold || 0)}</td><td class="num r">${num(p.block || 0)}</td></tr>`)}</tbody></table></div>`
      : html`<p class="empty">No stored decisions yet.</p>`}
    <p class="src">Source: GET /api/v2/dashboard?bucket=day (${r.v.activity?.source ?? "stored decisions"}). ${r.v.window?.note ?? ""}</p>
  </article>`;
}

function latencyCard(live, health) {
  const h = health.ok ? health.v : null;
  const head = html`<div class="card-h"><h2>Latency and feed</h2>${h ? html`<span class="pill ${h.ready ? "p-ok" : "p-bad"}">${h.ready ? "ready" : "not ready"}</span>` : ""}</div>`;
  if (!live.ok) return html`<article class="card" id="pl-latency">${head}${errNote(live)}</article>`;
  const ev = live.v.events || [];
  const lat = ev.map((e) => e.latency_ms).filter((x) => typeof x === "number");
  return html`<article class="card" id="pl-latency">${head}
    <ul class="checks">
      <li>Live decisions read<b>${num(ev.length)}</b></li>
      <li>Server latency p50<b>${lat.length ? fixed(quantile(lat, 0.5), 2) + " ms" : "n/a"}</b></li>
      <li>Server latency p95<b>${lat.length ? fixed(quantile(lat, 0.95), 2) + " ms" : "n/a"}</b></li>
      <li>Latest decision<b>${ev[0]?.received_at ?? "none yet"}</b></li>
      <li>Store<b>${live.v.store?.kind ?? (live.v.durable ? "durable" : "memory")}${live.v.durable ? ", durable" : ", not durable"}</b></li>
      ${h ? html`<li>Auth<b>${h.auth ?? "n/a"}</b></li>` : ""}
    </ul>
    ${ev.length ? "" : html`<p class="empty">No live decision yet: payments arrive through the signed webhook (POST /api/v2/events/payment).</p>`}
    <p class="src">Source: GET /api/v2/live (the latest 50 webhook decisions; server time per payment, not the sender's round trip) and GET /api/health.</p>
  </article>`;
}

export async function render(main, ctx) {
  const [drift, dash, live, health] = await Promise.all([
    settle(api("/v2/drift")), settle(api("/v2/dashboard?bucket=day")), settle(api("/v2/live")), settle(api("/health")),
  ]);
  if (!ctx.isCurrent()) return;
  main.innerHTML = html`
    <div class="banner" role="note">Shadow pilot: RuseTect scores a copy of the payment stream and changes no payment. Accuracy on a bank's real
      payments is not yet measured; these signals say whether the pilot is running as planned, not whether it is right.</div>
    <div class="grid cols-2">${driftCard(drift)}<div class="stack">${latencyCard(live, health)}${volumeCard(dash)}</div></div>`.toString();
}
