// Small helpers shared by every view. `html` escapes every interpolated value unless it is wrapped in raw().

const ESC = {"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;", "`": "&#96;"};
export const esc = (v) => String(v ?? "").replace(/[&<>"'`]/g, (c) => ESC[c]);

class Raw { constructor(s) { this.s = s; } toString() { return this.s; } }
export const raw = (s) => new Raw(String(s));

function part(v) {
  if (v instanceof Raw) return v.s;
  if (Array.isArray(v)) return v.map(part).join("");
  if (v === null || v === undefined || v === false) return "";
  return esc(v);
}
export function html(strings, ...vals) {
  let out = strings[0];
  vals.forEach((v, i) => { out += part(v) + strings[i + 1]; });
  return raw(out);
}

export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

const NF = new Intl.NumberFormat("en-US");
export const num = (v) => (typeof v === "number" && !Number.isNaN(v) ? NF.format(v) : "n/a");
export const fixed = (v, d = 3) => (typeof v === "number" ? v.toFixed(d) : "n/a");
export const pct = (v, d = 1) => (typeof v === "number" ? (v * 100).toFixed(d) + "%" : "n/a");
export const money = (v, cur) => (typeof v === "number" ? NF.format(Math.round(v * 100) / 100) + (cur ? " " + cur : "") : "n/a");
export const sum = (o) => Object.values(o || {}).reduce((a, b) => a + (typeof b === "number" ? b : 0), 0);
export const words = (s) => String(s ?? "").replace(/_/g, " ");

/** The band of a final decision (allow, verify, hold, block), as the backend names them. */
export function bandOf(decision) {
  const d = String(decision || "").toLowerCase();
  if (d.startsWith("block")) return "block";
  if (d.startsWith("hold")) return "hold";
  if (d.startsWith("step") || d.startsWith("verify")) return "verify";
  if (d.startsWith("approve") || d === "allow") return "allow";
  return "";
}
export const bandPill = (band) => (band ? html`<span class="pill p-${band}">${band}</span>` : html`<span class="muted">n/a</span>`);
const PRIO = {critical: "p-bad", high: "p-warn", medium: "p-info", low: ""};
export const prioPill = (p) => (p ? html`<span class="pill ${PRIO[p] ?? ""}">${p}</span>` : html`<span class="muted">none</span>`);
export const demoPill = () => html`<span class="pill p-demo plain" title="Demonstration data">Demo</span>`;

/** Sets each [data-w] element's width (0 to 100 %) and [data-flex] its flex; the CSP allows no style attributes. */
export function applyWidths(root) {
  $$("[data-w]", root).forEach((e) => { e.style.width = Math.max(0, Math.min(100, +e.dataset.w || 0)) + "%"; });
  $$("[data-flex]", root).forEach((e) => { e.style.flex = String(+e.dataset.flex || 0); });
}

let toastTimer;
export function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.textContent = ""; }, 4200);
}
