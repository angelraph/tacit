// Live market prices for the ticker, the landing page and USD values in the
// app. CoinGecko first; Coinbase as a fallback for ZEC. Prices are only
// ever displayed: nothing in Tacit's protocol depends on them.

export const COINS = [
  { id: "zcash", sym: "ZEC", name: "Zcash", color: "#f4b728", ink: "#1a1200" },
  { id: "bitcoin", sym: "BTC", name: "Bitcoin", color: "#f7931a", ink: "#1a0d00" },
  { id: "ethereum", sym: "ETH", name: "Ethereum", color: "#8c9eff", ink: "#0d1033" },
  { id: "solana", sym: "SOL", name: "Solana", color: "#14f195", ink: "#032016" },
  { id: "near", sym: "NEAR", name: "NEAR", color: "#e8e8e8", ink: "#111" },
  { id: "usd-coin", sym: "USDC", name: "USD Coin", color: "#2775ca", ink: "#fff" },
  { id: "tether", sym: "USDT", name: "Tether", color: "#26a17b", ink: "#fff" },
  { id: "dogecoin", sym: "DOGE", name: "Dogecoin", color: "#c2a633", ink: "#1a1500" },
];

let cache = null;
let cachedAt = 0;
let inflight = null;
const listeners = new Set();

async function fetchPrices() {
  const ids = COINS.map((c) => c.id).join(",");
  try {
    const r = await fetch(`https://api.coingecko.com/api/v3/simple/price?ids=${ids}&vs_currencies=usd&include_24hr_change=true`);
    if (!r.ok) throw new Error(String(r.status));
    const j = await r.json();
    return Object.fromEntries(COINS.map((c) => [c.sym, j[c.id] ? { usd: j[c.id].usd, change: j[c.id].usd_24h_change } : null]));
  } catch {
    // Coinbase has no 24h change on this endpoint, but a price beats nothing.
    const out = {};
    await Promise.all(
      COINS.map(async (c) => {
        try {
          const r = await fetch(`https://api.coinbase.com/v2/prices/${c.sym}-USD/spot`);
          const j = await r.json();
          out[c.sym] = { usd: Number(j.data.amount), change: null };
        } catch {
          out[c.sym] = null;
        }
      }),
    );
    return out;
  }
}

/** Current prices, refreshed at most once a minute. */
export async function prices() {
  if (cache && Date.now() - cachedAt < 60_000) return cache;
  inflight ||= fetchPrices()
    .then((p) => {
      cache = p;
      cachedAt = Date.now();
      listeners.forEach((fn) => fn(p));
      return p;
    })
    .finally(() => (inflight = null));
  return inflight;
}

export function onPrices(fn) {
  listeners.add(fn);
  if (cache) fn(cache);
  return () => listeners.delete(fn);
}

export const zecUsd = () => cache?.ZEC?.usd ?? null;

/** Seven days of hourly ZEC prices, for sparklines. */
let chart = null;
export async function zecWeek() {
  if (chart) return chart;
  try {
    const r = await fetch("https://api.coingecko.com/api/v3/coins/zcash/market_chart?vs_currency=usd&days=7");
    const j = await r.json();
    chart = j.prices.map((p) => p[1]);
  } catch {
    chart = [];
  }
  return chart;
}

export function usd(v) {
  if (v == null || !Number.isFinite(v)) return "–";
  if (v >= 1000) return "$" + v.toLocaleString(undefined, { maximumFractionDigits: 0 });
  if (v >= 1) return "$" + v.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  return "$" + v.toLocaleString(undefined, { maximumFractionDigits: 4 });
}

export function pct(v) {
  if (v == null || !Number.isFinite(v)) return "";
  return `${v >= 0 ? "+" : ""}${v.toFixed(2)}%`;
}

/** An SVG sparkline with a soft gradient fill. */
export function sparkline(values, { width = 240, height = 56, stroke = "#8e7bff" } = {}) {
  if (!values || values.length < 2) return "";
  const min = Math.min(...values);
  const max = Math.max(...values);
  const span = max - min || 1;
  const pts = values.map((v, i) => [(i / (values.length - 1)) * width, height - 4 - ((v - min) / span) * (height - 8)]);
  const line = pts.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(" ");
  const id = "sg" + Math.random().toString(36).slice(2, 7);
  return `<svg viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" aria-hidden="true">
    <defs><linearGradient id="${id}" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="${stroke}" stop-opacity=".35"/><stop offset="1" stop-color="${stroke}" stop-opacity="0"/></linearGradient></defs>
    <path d="${line} L${width} ${height} L0 ${height} Z" fill="url(#${id})"/>
    <path d="${line}" fill="none" stroke="${stroke}" stroke-width="1.8" vector-effect="non-scaling-stroke"/>
  </svg>`;
}

export function coinIcon(sym, size = 22) {
  const c = COINS.find((x) => x.sym === sym) || { color: "#6d5cff", ink: "#fff" };
  const label = sym === "ZEC" ? "ⓩ" : sym.slice(0, sym.length > 3 ? 2 : 3);
  return `<span class="coin" style="width:${size}px;height:${size}px;background:${c.color};color:${c.ink};font-size:${Math.round(size * 0.38)}px">${label}</span>`;
}

// ---------- the ticker bar ----------

export function mountTicker(el, extra = []) {
  const render = (p) => {
    const items = COINS.map((c) => {
      const q = p[c.sym];
      if (!q) return "";
      const cls = q.change == null ? "" : q.change >= 0 ? "up" : "down";
      return `<span class="ticker-item">${coinIcon(c.sym, 16)}<b>${c.sym}</b>${usd(q.usd)}<span class="${cls}">${pct(q.change)}</span></span>`;
    }).join("");
    const tags = extra.map((t) => `<span class="ticker-item"><span class="tag">${t}</span></span>`).join("");
    // Twice, so the marquee loops seamlessly.
    const once = tags + items;
    el.innerHTML = `<div class="ticker-track">${once}${once}</div>`;
  };
  onPrices(render);
  prices().catch(() => {});
  setInterval(() => prices().catch(() => {}), 60_000);
}
