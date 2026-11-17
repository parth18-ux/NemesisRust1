// A. Executive dashboard. Every number is read from /api/v2/dashboard, /v2/drift, /v2/model-card or /v2/transactions;
// a null is shown as "not available" with the backend's note, and replayed demo data is labelled as such.
import {api} from "./api.js";
import {$$, html, num, fixed, pct, money, sum, words, bandPill, demoPill, applyWidths} from "./util.js";

const BANDS = ["allow", "verify", "hold", "block"];
let bucket = "hour";

const settle = (p) => p.then((v) => ({ok: true, v}), (e) => ({ok: false, e}));
const errNote = (r) => html`<div class="banner err" role="alert">${r.e?.explain || r.e?.message || "unavailable"}</div>`;
const src = (s) => (s ? html`<p class="src">Source: ${s}</p>` : "");

function kpi(label, m, {sub, fmt = num, na} = {}) {
  const v = m?.value;
  const missing = v === null || v === undefined;
  return html`<article class="card kpi">
    <h3>${label}</h3>
    ${missing ? html`<p class="v na">not available</p><p class="sub">${na || m?.note || ""}</p>` : html`<p class="v">${fmt(v)}</p>${sub ? html`<p class="sub">${sub}</p>` : ""}`}
    ${src(m?.source)}
  </article>`;
}

function barList(items, {cls = () => "", label = (x) => x.name} = {}) {
  if (!items.length) return html`<p class="empty">None recorded yet.</p>`;
  const max = Math.max(...items.map((x) => x.count), 1);
  return html`<div class="bars">${items.map((x) => html`<div class="bar-row"><span>${label(x)}</span><span class="n">${num(x.count)}</span>
    <span class="track"><span class="fill ${cls(x)}" data-w="${(100 * x.count) / max}"></span></span></div>`)}</div>`;
}
const entries = (o) => Object.entries(o || {}).map(([name, count]) => ({name, count})).sort((a, b) => b.count - a.count);

function activityChart(series) {
  if (!series?.length) return html`<p class="empty">No stored decisions to chart yet.</p>`;
  const W = 720, H = 220, L = 40, B = 26, T = 8, R = 8;
  const max = Math.max(...series.map((p) => p.total), 1);
  const bw = (W - L - R) / series.length;
  const y = (v) => T + (H - T - B) * (1 - v / max);
  const ticks = [0, 0.5, 1].map((f) => Math.round(max * f));
  const every = Math.max(1, Math.ceil(series.length / 8));
  const cols = series.map((p, i) => {
    let acc = 0;
    const x = L + i * bw;
    const rects = BANDS.map((b) => {
      const v = p[b] || 0;
      if (!v) return "";
      const y0 = y(acc), y1 = y(acc + v);
      acc += v;
      return html`<rect class="b-${b}" x="${x + bw * 0.15}" y="${y1}" width="${Math.max(1, bw * 0.7)}" height="${Math.max(0.5, y0 - y1)}"/>`;
    });
    const tip = `${p.t} UTC: ${p.total} total, ${BANDS.map((b) => `${b} ${p[b] || 0}`).join(", ")}`;
    return html`<g class="col"><title>${tip}</title><rect class="hit" x="${x}" y="${T}" width="${bw}" height="${H - T - B}"/>${rects}
      ${i % every === 0 ? html`<text x="${x + bw / 2}" y="${H - 8}" text-anchor="middle">${String(p.t).slice(bucket === "hour" ? 5 : 0)}</text>` : ""}</g>`;
  });
  return html`<svg class="chart" viewBox="0 0 ${W} ${H}" role="img" aria-label="Stored decisions per ${bucket} by band, ${series.length} buckets">
    ${ticks.map((t) => html`<line class="gl" x1="${L}" x2="${W - R}" y1="${y(t)}" y2="${y(t)}"/><text x="${L - 6}" y="${y(t) + 3}" text-anchor="end">${t}</text>`)}
    <line class="ax" x1="${L}" x2="${W - R}" y1="${H - B}" y2="${H - B}"/>${cols}</svg>`;
}

const legend = () => html`<div class="legend">${BANDS.map((b) => html`<span><i class="sw-${b}"></i>${b}</span>`)}</div>`;

function bandSplit(byBand) {
  const total = sum(byBand);
  if (!total) return html`<p class="empty">No stored decisions yet.</p>`;
  return html`<div class="split" role="img" aria-label="${BANDS.map((b) => `${b} ${byBand[b] || 0}`).join(", ")}">
      ${BANDS.filter((b) => byBand[b]).map((b) => html`<span class="sw-${b}" data-flex="${byBand[b]}" title="${b}: ${byBand[b]}"></span>`)}</div>
    ${barList(BANDS.map((b) => ({name: b, count: byBand[b] || 0})), {cls: (x) => "f-" + x.name, label: (x) => bandPill(x.name)})}`;
}

function healthCard(h, drift) {
  const row = (label, okv, text) => html`<li><span class="dot ${okv === null ? "" : okv ? "ok" : "bad"}"></span>${label}<b>${text}</b></li>`;
  const d = drift.ok ? drift.v : null;
  const dcls = d ? (d.status === "ok" ? "p-ok" : d.status === "warn" ? "p-warn" : d.status === "alert" ? "p-bad" : "") : "";
  return html`<article class="card">
    <div class="card-h"><h2>Model and API health</h2><span class="pill ${h.ready ? "p-ok" : "p-bad"}">${h.ready ? "ready" : "not ready"}</span></div>
    <ul class="checks">
      ${row("Payment engine", h.engine_loaded, h.engine_loaded ? (h.hardened ? "loaded, hardened" : "loaded") : "not loaded")}
      ${row("Message model", h.message_model, h.message_model ? "loaded" : "not loaded")}
      ${row("PRAMAAN", h.pramaan, h.pramaan ? "loaded" : "not loaded")}
      ${row("Sanctions screen", h.sanctions, h.sanctions ? "loaded" : "not loaded")}
      ${row("Payee check", h.payee_check, h.payee_check ? "loaded" : "not loaded")}
      ${row("Store", h.store?.reachable !== false, `${h.store?.kind ?? "n/a"}${h.durable ? ", durable" : ", not durable"}`)}
      ${row("Automated blocking", null, h.automated_blocking ? "on" : "off: a block needs two analysts")}
      ${row("Version", null, `${h.version} · ruleset ${h.ruleset}`)}
    </ul>
    ${src(h.source)}
    <div class="sect">
      <div class="card-h"><h3>Drift</h3>${d ? html`<span class="pill ${dcls}">${d.status}${d.warming_up ? ", warming up" : ""}</span>` : ""}</div>
      ${d ? html`${d.reasons?.length ? html`<ul class="small">${d.reasons.map((r) => html`<li>${r}</li>`)}</ul>` : html`<p class="small muted">No drift reasons reported.</p>`}
        <p class="src">Source: GET /api/v2/drift, ${d.scope ?? ""}; window ${num(d.window?.size)} payments; reference ${d.reference ?? ""}</p>` : errNote(drift)}
    </div>
  </article>`;
}

function detectionCard(det, mc) {
  const t = det?.demo_stream_truth;
  const m = mc.ok ? mc.v : null;
  const op = m?.online_parity, xg = m?.aml_models?.["XGBoost+graph"];
  return html`<article class="card">
    <div class="card-h"><h2>Detection performance</h2></div>
    <div class="sect">
      <div class="card-h"><h3>Replay against synthetic labels</h3>${t ? demoPill() : ""}</div>
      ${t ? html`<ul class="checks"><li>True positives (block band, labelled laundering)<b>${num(t.tp)}</b></li><li>False positives<b>${num(t.fp)}</b></li>
          <li>Missed labelled payments<b>${num(t.fn)}</b></li><li>Labelled payments replayed<b>${num(t.pos)}</b></li></ul>`
        : html`<p class="small muted">not available: no demo replay has run on this instance.</p>`}
      <p class="small"><b>Confirmed fraud:</b> ${det?.confirmed_fraud ?? html`<span class="muted">not available</span>`} <span class="muted">${det?.confirmed_fraud_note ?? ""}</span></p>
      ${src(det?.source)}
    </div>
    <div class="sect">
      <h3>Offline evaluation (test split)</h3>
      ${m ? html`<ul class="checks">
          ${xg ? html`<li>XGBoost+graph PR-AUC<b>${fixed(xg.pr_auc)}</b></li><li>Precision / recall<b>${pct(xg.precision)} / ${pct(xg.recall)}</b></li>` : ""}
          ${op ? html`<li>Served engine PR-AUC on ${num(op.test_payments_scored)} test payments<b>${fixed(op.pr_auc_live)}</b></li>
            <li>Same decision as offline<b>${pct(op.same_decision_rate)}</b></li><li>Latency p50 / p99<b>${op.latency_ms_p50} / ${op.latency_ms_p99} ms</b></li>` : ""}</ul>
        <p class="src">Source: GET /api/v2/model-card. Dataset: ${m.dataset?.aml ?? ""}. Protocol: ${m.protocol?.aml ?? ""}. Synthetic data, not a bank's payments.</p>`
        : errNote(mc)}
    </div>
  </article>`;
}

function recentTable(tx) {
  if (!tx.ok) return errNote(tx);
  const rows = (tx.v.transactions || []).filter((r) => r.band && r.band !== "allow").slice(0, 8);
  if (!rows.length) return html`<p class="empty">No flagged payment is stored yet.</p>`;
  return html`<div class="table-wrap"><table>
    <caption class="sr">Most recent flagged payments</caption>
    <thead><tr><th scope="col">Payment</th><th scope="col">Time (UTC)</th><th scope="col">Payer</th><th scope="col">Payee</th><th scope="col" class="r">Amount</th>
      <th scope="col">Band</th><th scope="col">Typology</th><th scope="col">Alert</th></tr></thead>
    <tbody>${rows.map((r) => html`<tr>
      <td class="mono">${r.id} ${r.demo ? demoPill() : ""}</td><td class="mono">${r.timestamp}</td>
      <td class="mono">${r.from_bank}:${r.from_account}</td><td class="mono">${r.to_bank}:${r.to_account}</td>
      <td class="num r">${money(r.amount, r.currency)}</td><td>${bandPill(r.band)}</td>
      <td>${r.typologies?.length ? r.typologies.map(words).join(", ") : html`<span class="muted">none</span>`}</td>
      <td>${r.alert_id ? html`<a class="mono" href="#/alerts/${encodeURIComponent(r.alert_id)}">${r.alert_id}</a>` : html`<span class="muted">none</span>`}</td></tr>`)}</tbody>
  </table></div>${src(`GET /api/v2/transactions (newest first, ${tx.v.data} data)`)}`;
}

export async function render(main, ctx) {
  const [dash, drift, mc, tx] = await Promise.all([
    settle(api(`/v2/dashboard?bucket=${bucket}`)), settle(api("/v2/drift")), settle(api("/v2/model-card")), settle(api("/v2/transactions?limit=50")),
  ]);
  if (!ctx.isCurrent()) return;
  if (!dash.ok) { main.innerHTML = errNote(dash).toString(); return; }
  const d = dash.v, p = d.payments, a = d.alerts, inv = d.investigations, e = d.entities;
  const invBy = inv.by_status.value || {};
  const byBand = p.by_band.value || {};
  const kind = d.data.kind;
  const highRisk = {value: (byBand.hold || 0) + (byBand.block || 0), source: "stored decisions in the hold and block bands (see window)"};
  const openInv = {value: inv.total.value - (invBy.closed || 0), source: "investigations not closed"};
  const closedInv = {value: invBy.closed || 0, source: "investigations closed (maker-checker)"};
  const confirmed = {value: d.detection.confirmed_fraud, source: d.detection.source, note: d.detection.confirmed_fraud_note};

  main.innerHTML = html`
    ${kind === "demo_stream" || kind === "mixed" ? html`<div class="banner demo" role="note"><b>Demonstration data</b>
      <span>${kind === "mixed" ? "Live events and" : ""} replayed IBM AMLworld (synthetic) payments: ${num(d.data.demo_payments_replayed)} replayed on this instance. ${d.data.note}</span></div>` : ""}
    ${kind === "none" ? html`<div class="banner" role="note">No payment is stored yet. Live events arrive through the signed webhook; the <a href="/console">Live console</a> can replay demonstration data.</div>` : ""}
    <section class="grid kpis" aria-label="Key figures">
      ${kpi("Payments scored", p.scored, {sub: `${num(p.seen.value)} seen incl. warm-up history`})}
      ${kpi("Flagged and stored", p.flagged, {sub: `${num(p.stored.value)} stored decisions read`})}
      ${kpi("High risk (hold, block)", highRisk)}
      ${kpi("Confirmed fraud", confirmed)}
      ${kpi("Open alerts", a.open, {sub: `${num(a.unassigned.value)} unassigned · ${num(a.total.value)} total`})}
      ${kpi("Open investigations", openInv, {sub: `${num(inv.sla_breached.value)} past SLA`})}
      ${kpi("Closed investigations", closedInv)}
      ${kpi("Flagged payees", e.flagged_payees, {sub: `${num(e.ring_cases.value)} ring cases · ${num(e.campaigns.value)} campaigns`})}
    </section>
    <div class="grid cols-2" id="row1">
      <article class="card">
        <div class="card-h"><h2>Activity</h2>${legend()}
          <div class="seg" role="group" aria-label="Bucket">${["hour", "day"].map((b) => html`<button type="button" data-bucket="${b}" aria-pressed="${b === bucket}">${b}</button>`)}</div></div>
        ${activityChart(d.activity.series)}
        ${src(d.activity.source)}
      </article>
      <article class="card"><div class="card-h"><h2>Band distribution</h2></div>${bandSplit(byBand)}${src(p.by_band.source)}
        <div class="sect"><h3>Engine decisions</h3>${barList(entries(p.by_decision.value))}${src(p.by_decision.source)}</div></article>
    </div>
    <div class="grid cols-3">
      <article class="card"><div class="card-h"><h2>Alert severity</h2></div>
        <h3>By priority</h3>${barList(entries(a.by_priority.value), {cls: (x) => ({critical: "f-block", high: "f-hold", medium: "f-verify", low: "f-allow"})[x.name] || ""})}
        <div class="sect"><h3>By status</h3>${barList(entries(a.by_status.value))}</div>${src(a.by_priority.source)}</article>
      <article class="card"><div class="card-h"><h2>Investigation status</h2></div>${barList(entries(invBy), {label: (x) => words(x.name)})}
        <div class="sect"><h3>By priority</h3>${barList(entries(inv.by_priority.value))}</div>${src(inv.by_status.source)}</article>
      <article class="card"><div class="card-h"><h2>Emerging patterns</h2></div>
        <h3>Typologies (temporal motifs)</h3>${barList(d.typologies.value, {label: (x) => words(x.name)})}${src(d.typologies.source)}
        <div class="sect"><h3>Top reason codes</h3>${barList(d.reason_codes.value.slice(0, 6))}${src(d.reason_codes.source)}</div></article>
    </div>
    <div class="grid cols-2">
      <article class="card"><div class="card-h"><h2>Recent suspicious activity</h2><a class="btn" href="#/alerts">Open alerts</a></div>${recentTable(tx)}</article>
      <div class="stack">${healthCard(d.health, drift)}${detectionCard(d.detection, mc)}</div>
    </div>
    <p class="src">Generated ${d.generated_at} from GET /api/v2/dashboard. ${d.window?.note ?? ""}</p>`.toString();
  applyWidths(main);
  $$("[data-bucket]", main).forEach((b) => b.addEventListener("click", () => { bucket = b.dataset.bucket; render(main, ctx); }));
}
