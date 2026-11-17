// Fetch wrapper: JSON in and out, the analyst key the user chose (kept for this tab only), and a fresh X-Nonce on
// every write. No key is written in this file: demo keys are read from the server, and only when it runs in demo mode.

const KEY_SLOT = "rt-analyst-key";
let demoAnalysts = null;

export class ApiError extends Error {
  constructor(status, detail) {
    super(detail);
    this.status = status;
    this.detail = detail;
  }
  /** A message an analyst can act on: who may do this and what to change. */
  get explain() {
    if (this.status === 401) return `Sign-in needed: choose an analyst key in the top bar (server: ${this.detail}).`;
    if (this.status === 403) return `Not allowed for this role (server: ${this.detail}). Triage and case work need an analyst or supervisor key.`;
    if (this.status === 409) return `Conflict (server: ${this.detail}).`;
    if (this.status === 0) return `The server could not be reached (${this.detail}).`;
    return `Request failed with ${this.status} (server: ${this.detail}).`;
  }
}

export function getKey() {
  try { return sessionStorage.getItem(KEY_SLOT) || ""; } catch { return ""; }
}
export function setKey(k) {
  try { sessionStorage.setItem(KEY_SLOT, k || ""); } catch { /* storage blocked */ }
}

const nonce = () => (crypto.randomUUID ? crypto.randomUUID()
  : Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join(""));

export async function api(path, {method = "GET", body} = {}) {
  const headers = {accept: "application/json"};
  const key = getKey();
  if (key) headers["X-Analyst-Key"] = key;
  if (method !== "GET") {
    headers["X-Nonce"] = nonce();
    if (body !== undefined) headers["content-type"] = "application/json";
  }
  let r;
  try {
    r = await fetch("/api" + path, {method, headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: "same-origin"});
  } catch (e) {
    throw new ApiError(0, e.message || "network error");
  }
  const text = await r.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch { data = text; }
  if (!r.ok) {
    const d = data && typeof data === "object" ? data.detail : data;
    // a validation error is a list of {loc, msg}: show the messages, not the JSON
    const list = Array.isArray(d) && d.every((x) => x && typeof x.msg === "string") ? d.map((x) => x.msg.replace(/^Value error, /, "")).join("; ") : null;
    throw new ApiError(r.status, list ?? (typeof d === "string" ? d : JSON.stringify(d ?? r.statusText)));
  }
  return data;
}

/** The demo analysts the server offers (an empty list unless it runs in demo mode). */
export async function demoKeys() {
  if (demoAnalysts === null) {
    try { demoAnalysts = await api("/v2/demo/analysts"); } catch { demoAnalysts = []; }
    if (!Array.isArray(demoAnalysts)) demoAnalysts = [];
  }
  return demoAnalysts;
}
export const isDemo = async () => (await demoKeys()).length > 0;
const whoCache = new Map();
/** The analyst id and role behind the current key, from GET /api/v2/whoami (demo or configured keys), else null. */
export async function currentAnalyst() {
  const k = getKey();
  if (!k) return null;
  if (!whoCache.has(k)) whoCache.set(k, api("/v2/whoami").catch(() => null));
  return whoCache.get(k);
}

/** Demo mode only: replays the fictional mule fixture (victims pay one mule, who forwards to a collector) through
 *  POST /api/v2/assess, so the alert and network views have demonstration data. `progress(done, total)` is optional. */
export async function replayMule(progress = () => {}) {
  const F = await api("/v2/fixtures/mule");
  const t0 = Date.parse(F.start.replace(" ", "T") + ":00Z");
  const ts = (m) => new Date(t0 + m * 60000).toISOString().slice(0, 16).replace("T", " ");
  const n = F.deposits_inr.length;
  for (const [i, amt] of F.deposits_inr.entries()) {
    const v = "V" + String(i + 1).padStart(2, "0"), t = i * F.gap_minutes;
    progress(2 * i + 1, 2 * n);
    await api("/v2/assess", {method: "POST", body: {timestamp: ts(t), from_bank: "01" + String(i + 1).padStart(2, "0"), from_account: v, to_bank: F.mule.bank,
      to_account: F.mule.account, amount_paid: amt, payment_currency: "Rupee", payment_format: "ACH", message: i === 0 ? F.victim_message_for_V01 : null, consent: i === 0}});
    await api("/v2/assess", {method: "POST", body: {timestamp: ts(t + 1), from_bank: F.mule.bank, from_account: F.mule.account, to_bank: F.collector.bank,
      to_account: F.collector.account, amount_paid: Math.round(amt * F.forward_share * 100) / 100, payment_currency: "Rupee", payment_format: "ACH"}});
  }
  return F;
}
