// Plumbing shared by the views: the worker, the coordinator API, drand,
// encrypted local storage and formatting.

// ---------- workers ----------
//
// Two copies of the WebAssembly: the wallet worker syncs, scans and runs
// vault seats; the prover worker builds the proving key and proves
// transactions. Proving takes tens of seconds and would otherwise freeze
// everything else.

function spawn() {
  const w = new Worker(new URL("./worker.js", import.meta.url), { type: "module" });
  const pending = new Map();
  let nextId = 1;
  w.onmessage = ({ data }) => {
    const p = pending.get(data.id);
    if (!p) return;
    if (data.progress) {
      p.onProgress && p.onProgress(data.progress);
      return;
    }
    pending.delete(data.id);
    data.error ? p.reject(new Error(data.error)) : p.resolve(data.result);
  };
  return (method, args, onProgress) => {
    const id = nextId++;
    return new Promise((resolve, reject) => {
      pending.set(id, { resolve, reject, onProgress });
      w.postMessage({ id, method, args });
    });
  };
}

export const call = spawn();
export const prove = spawn();

// ---------- settings ----------

const DEFAULT_GATEWAY = window.TACIT_GATEWAY || "http://127.0.0.1:8650";

export function gatewayUrl() {
  const fromQuery = new URLSearchParams(location.search).get("gateway");
  if (fromQuery) safeSet("tacit.gateway", fromQuery);
  return (safeGet("tacit.gateway") || DEFAULT_GATEWAY).replace(/\/$/, "");
}

export function setGatewayUrl(url) {
  safeSet("tacit.gateway", url.replace(/\/$/, ""));
}

export function safeGet(k) {
  try {
    return localStorage.getItem(k);
  } catch {
    return null;
  }
}

export function safeSet(k, v) {
  try {
    v == null ? localStorage.removeItem(k) : localStorage.setItem(k, v);
  } catch {}
}

// ---------- coordinator ----------

export async function api(path, init) {
  const res = await fetch(gatewayUrl() + path, init);
  const text = await res.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    body = { error: text };
  }
  if (!res.ok) throw new Error(body.error || `${res.status} from the coordinator`);
  return body;
}

export const postJson = (path, body) =>
  api(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });

// ---------- drand quicknet ----------

const QUICKNET = "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971";
const RELAYS = ["https://api.drand.sh", "https://drand.cloudflare.com", "https://api2.drand.sh"];

// The beacon for a round, or null if it hasn't been published yet. The
// worker verifies it against the quicknet key before anything trusts it.
export async function beacon(round) {
  for (const relay of RELAYS) {
    try {
      const res = await fetch(`${relay}/${QUICKNET}/public/${round}`);
      if (res.status === 404 || res.status === 425) return null;
      if (!res.ok) continue;
      const b = await res.json();
      if (b.round !== round) continue;
      if (await call("verifyBeacon", { round, signature: b.signature })) return b;
    } catch {}
  }
  return null;
}

// Quicknet: genesis 1692803367, a round every 3 seconds.
export const roundTime = (round) => 1692803367 + (round - 1) * 3;
export const roundAt = (unix) => Math.floor((unix - 1692803367) / 3) + 1;

// ---------- encrypted phrase storage ----------

const enc = new TextEncoder();
const dec = new TextDecoder();
const b64 = (u8) => btoa(String.fromCharCode(...u8));
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));

async function keyFrom(passcode, salt) {
  const base = await crypto.subtle.importKey("raw", enc.encode(passcode), "PBKDF2", false, ["deriveKey"]);
  return crypto.subtle.deriveKey(
    { name: "PBKDF2", salt, iterations: 310000, hash: "SHA-256" },
    base,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}

export async function sealPhrase(phrase, passcode) {
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const key = await keyFrom(passcode, salt);
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, enc.encode(phrase)));
  return { salt: b64(salt), iv: b64(iv), ct: b64(ct) };
}

export async function openPhrase(sealed, passcode) {
  const key = await keyFrom(passcode, unb64(sealed.salt));
  try {
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: unb64(sealed.iv) }, key, unb64(sealed.ct));
    return dec.decode(pt);
  } catch {
    throw new Error("Wrong passcode");
  }
}

// ---------- formatting ----------

export const ZAT = 100_000_000;

export function zec(zat, digits) {
  const v = Number(zat) / ZAT;
  const s = digits == null ? v.toFixed(8).replace(/\.?0+$/, "") : v.toFixed(digits);
  return s === "" ? "0" : s;
}

export function parseZec(s) {
  const m = String(s).trim().match(/^(\d*)(?:\.(\d{0,8}))?$/);
  if (!m || (m[1] === "" && !m[2])) return null;
  return Number(m[1] || 0) * ZAT + Number((m[2] || "").padEnd(8, "0"));
}

export function short(s, n = 10) {
  return s && s.length > 2 * n + 1 ? `${s.slice(0, n)}…${s.slice(-n)}` : s;
}

export function ago(secs) {
  const s = Math.abs(Math.round(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
}

export function esc(s) {
  return String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

export function h(html) {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  return t.content.firstElementChild;
}

export function toast(msg, kind = "info") {
  const el = h(`<div class="toast ${kind}">${esc(msg)}</div>`);
  document.getElementById("toasts").append(el);
  setTimeout(() => el.classList.add("out"), 4200);
  setTimeout(() => el.remove(), 4800);
}

/** A QR code as an SVG string (dark modules on white, for any scanner). */
export function qrSvg(text, cell = 4) {
  if (typeof window.qrcode !== "function") return "";
  const q = window.qrcode(0, "L");
  q.addData(text, "Byte");
  q.make();
  return q.createSvgTag({ cellSize: cell, margin: 3, scalable: true });
}

// ---------- extension wallets ----------
//
// Zcash browser extensions that inject a provider for dApps. Noir's lives at
// window.noirwallet.zcash; its send API takes decimal ZEC strings and a UTF-8
// memo of at most 512 bytes. Extensions inject after page load, so detection
// waits a moment before giving up.

export const EXTENSIONS = [
  {
    id: "noir",
    name: "Noir Wallet",
    url: "https://docs.zknoir.com",
    provider: () => window.noirwallet && window.noirwallet.isNoirWallet && window.noirwallet.zcash,
  },
];

export async function detectExtension(waitMs = 1200) {
  const until = Date.now() + waitMs;
  for (;;) {
    for (const x of EXTENSIONS) {
      const p = x.provider();
      if (p) return { ...x, zcash: p };
    }
    if (Date.now() > until) return null;
    await new Promise((r) => setTimeout(r, 150));
  }
}

const rpc = (z, method, params) => z.request(params ? { method, params } : { method });

/** Asks the extension for access (a popup only the first time) and returns its addresses. */
export async function extConnect(ext) {
  let acct = null;
  try {
    acct = await rpc(ext.zcash, "zcash_getAccounts");
  } catch {}
  if (!acct || !acct.shielded) acct = await rpc(ext.zcash, "zcash_requestAccounts");
  if (!acct || !acct.shielded) throw new Error(`${ext.name} didn't share a shielded address`);
  return acct;
}

/** Spendable shielded balance in zatoshi, or null when the extension won't say. */
export async function extBalance(ext) {
  try {
    const b = await rpc(ext.zcash, "zcash_getBalance", []);
    return parseZec(b.shielded ?? b.available ?? b.total ?? "");
  } catch {
    return null;
  }
}

/** Sends through the extension; resolves to the txid once it is broadcast. */
export async function extSend(ext, { to, amountZat, memo }) {
  const r = await rpc(ext.zcash, "zcash_sendTransaction", [{ to, amount: zec(amountZat), memo }]);
  return typeof r === "string" ? r : r && (r.txid || r.txId || r.hash);
}

export async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("Copied");
  } catch {
    toast("Couldn't copy", "error");
  }
}
