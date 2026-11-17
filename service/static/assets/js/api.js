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
    throw new ApiError(r.status, typeof d === "string" ? d : JSON.stringify(d ?? r.statusText));
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
/** The analyst behind the current key when the server named it (demo mode), else null. */
export async function currentAnalyst() {
  const k = getKey();
  return (await demoKeys()).find((a) => a.key === k) || null;
}
