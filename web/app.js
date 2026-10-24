// The Tacit web app: views, routing and the glue between the coordinator,
// drand and the WebAssembly worker that holds the keys.

import {
  call, prove, api, beacon, roundTime, gatewayUrl, setGatewayUrl, safeGet, safeSet,
  sealPhrase, openPhrase, zec, parseZec, short, ago, esc, h, toast, copy,
} from "./lib.js";
import { renderSell } from "./sell.js";
import { renderEscrow } from "./escrow.js";
import { renderFund } from "./fund.js";
import { renderLanding } from "./landing.js";
import { renderFaq, renderAgents, renderTerms, gate } from "./pages.js";
import { mountTicker, onPrices, prices, zecUsd, usd } from "./prices.js";
import { reveals, glowCards, scramble } from "./motion.js";

// Zcash targets 75 s blocks; a regtest chain runs at whatever pace it is
// mined, so the app measures the recent average instead of assuming.
let BLOCK_SECS = 75;

async function measureBlockTime() {
  // Regtest block timestamps are synthetic; `tacit-dev mine-every` defaults
  // to a block every 20 s.
  if (S.net === "regtest") {
    BLOCK_SECS = 20;
    return;
  }
  try {
    const tip = S.info.tip;
    const blocks = await api(`/v1/chain/blocks?from=${Math.max(1, tip - 20)}&to=${tip}`);
    // The median gap ignores bursts of quickly mined blocks.
    const gaps = blocks.slice(1).map((b, i) => b.time - blocks[i].time).sort((a, b) => a - b);
    if (gaps.length >= 3) BLOCK_SECS = Math.min(600, Math.max(5, gaps[Math.floor(gaps.length / 2)]));
  } catch {}
}
const view = document.getElementById("view");

export const S = {
  info: null, // chain gateway info
  net: null, // "main" | "test" | "regtest"
  wallet: null, // wallet state from the worker
  locked: false,
  auctions: [],
  ready: null,
};

// ---------- boot ----------

S.ready = (async () => {
  // The wallet worker runs single threaded; only the prover needs threads.
  await call("init", { threads: false, gatewayUrl: gatewayUrl() });
  S.loaded = true;
})();

async function refreshChain() {
  const btn = document.getElementById("chain");
  try {
    S.info = await api("/v1/chain/info");
    const first = !S.net;
    S.net = normaliseNet(S.info.chain);
    if (first) await measureBlockTime();
    btn.className = "chain ok";
    document.getElementById("chain-label").textContent = `${netName(S.net)} · ${S.info.tip.toLocaleString()}`;
  } catch (e) {
    S.info = null;
    btn.className = "chain bad";
    document.getElementById("chain-label").textContent = "offline";
  }
}

function normaliseNet(chain) {
  const c = (chain || "").toLowerCase();
  if (c.startsWith("main")) return "main";
  if (c.startsWith("test")) return "test";
  return "regtest";
}

export const netName = (n) => ({ main: "Mainnet", test: "Testnet", regtest: "Regtest" })[n] || n;
export const unit = () => (S.net === "main" ? "ZEC" : "TAZ");

document.getElementById("chain").onclick = () => {
  const m = modal(`
    <h2>Chain gateway</h2>
    <p class="muted">Tacit syncs and broadcasts through a coordinator's chain gateway. It only ever sees public chain data and finished, encrypted transactions.</p>
    <label class="field"><span>Coordinator URL</span><input id="gw" value="${esc(gatewayUrl())}" /></label>
    ${S.info ? `<p class="faint" style="margin-top:12px">Connected: ${esc(S.info.node)}, ${esc(netName(S.net))}, tip ${S.info.tip}</p>` : ""}
    <div class="row" style="margin-top:16px"><span class="spacer"></span><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="save">Save</button></div>`);
  m.querySelector("#save").onclick = async () => {
    setGatewayUrl(m.querySelector("#gw").value.trim());
    await call("setGateway", { gatewayUrl: gatewayUrl() });
    if (warmed) await prove("setGateway", { gatewayUrl: gatewayUrl() });
    m.close();
    S.wallet = null;
    S.locked = false;
    await refreshChain();
    route();
  };
};

// ---------- modal ----------

export function modal(inner) {
  const back = h(`<div class="modal-back"><div class="modal">${inner}</div></div>`);
  back.close = () => back.remove();
  back.addEventListener("click", (e) => {
    if (e.target === back || e.target.closest("[data-close]")) back.close();
  });
  document.getElementById("modal-root").append(back);
  return back;
}

// A modal with a progress bar for long jobs in the worker.
export function busy(title) {
  const m = modal(`<h2>${esc(title)}</h2><p class="muted" id="busy-label">Starting…</p><div class="progress spin" id="busy-bar"><i></i></div>`);
  m.onProgress = (p) => {
    m.querySelector("#busy-label").textContent = p.label + (p.total ? ` (${p.done.toLocaleString()} of ${p.total.toLocaleString()} blocks)` : "");
    const bar = m.querySelector("#busy-bar");
    if (p.spin || !p.total) {
      bar.className = "progress spin";
      bar.firstElementChild.style.width = "";
    } else {
      bar.className = "progress";
      bar.firstElementChild.style.width = `${Math.round((100 * p.done) / Math.max(1, p.total))}%`;
    }
  };
  return m;
}

// ---------- wallet storage ----------

const walletKey = () => `tacit.wallet.${S.net}`;

export function storedWallet() {
  try {
    return JSON.parse(safeGet(walletKey()) || "null");
  } catch {
    return null;
  }
}

function storeWallet(rec) {
  safeSet(walletKey(), rec ? JSON.stringify(rec) : null);
}

export function saveScan(saved) {
  const rec = storedWallet();
  if (rec) {
    rec.saved = saved;
    storeWallet(rec);
  }
}

// Seat key and vault shares, sealed with the wallet passcode. Only
// readable while the wallet is unlocked.
export async function loadSeats() {
  if (!S.pass) throw new Error("Unlock your wallet first");
  const raw = safeGet(`tacit.seats.${S.net}`);
  if (raw) return JSON.parse(await openPhrase(JSON.parse(raw), S.pass));
  // An X25519 secret is any 32 random bytes (it is clamped when used).
  const secret = Array.from(crypto.getRandomValues(new Uint8Array(32)), (b) => b.toString(16).padStart(2, "0")).join("");
  const fresh = { secret, vaults: [] };
  await saveSeats(fresh);
  return fresh;
}

export async function saveSeats(seats) {
  safeSet(`tacit.seats.${S.net}`, JSON.stringify(await sealPhrase(JSON.stringify(seats), S.pass)));
}

export function nextRefundIndex() {
  const rec = storedWallet();
  rec.refund = (rec.refund || 0) + 1;
  storeWallet(rec);
  return rec.refund;
}

export function myBids() {
  try {
    return JSON.parse(safeGet(`tacit.bids.${S.net}`) || "[]");
  } catch {
    return [];
  }
}

function addBid(b) {
  const all = myBids();
  all.unshift(b);
  safeSet(`tacit.bids.${S.net}`, JSON.stringify(all));
}

// Builds the proving key in the worker once a wallet is open, so the first
// bid doesn't wait for it. Not at startup: it blocks the worker for a while.
let warmed = null;
export function warmProver() {
  if (!warmed) {
    const threads = self.crossOriginIsolated && typeof SharedArrayBuffer !== "undefined";
    warmed = prove("init", { threads, gatewayUrl: gatewayUrl() })
      .then(() => prove("warm"))
      .catch((e) => {
        warmed = null;
        throw e;
      });
  }
  return warmed;
}

// Proves a wallet transaction in the prover worker with the wallet's current
// state, then reloads the wallet worker with the result.
export async function proveWith(method, args, onProgress) {
  await warmProver();
  const rec = storedWallet();
  const r = await prove(method, { ...args, phrase: S.phrase, saved: rec.saved }, onProgress);
  saveScan(r.saved);
  S.wallet = await call("openWallet", { phrase: S.phrase, saved: r.saved });
  return r;
}

export async function syncWallet(quiet) {
  if (!S.wallet) return;
  try {
    const r = await call("sync", {});
    saveScan(r.saved);
    S.wallet = r.state;
  } catch (e) {
    if (!quiet) toast(e.message, "error");
  }
}

// ---------- routing ----------

const routes = [
  [/^\/?$/, () => renderLanding(ctx), "home"],
  [/^\/market$/, renderMarket, "market"],
  [/^\/auction\/([0-9a-f]{64})$/, renderAuction, "market"],
  [/^\/sell$/, (m) => renderSell(m, ctx), "sell"],
  [/^\/escrow(?:\/(.*))?$/, (m) => renderEscrow(m, ctx), "escrow"],
  [/^\/wallet$/, renderWallet, "wallet"],
  [/^\/fund$/, (m) => renderFund(m, ctx), "wallet"],
  [/^\/how$/, renderHow, "explore"],
  [/^\/faq$/, renderFaq, "explore"],
  [/^\/agents$/, () => renderAgents(ctx), "explore"],
  [/^\/terms$/, renderTerms, "explore"],
];

// Pages that act with money show the risk notice once first.
const GATED = new Set(["market", "sell", "escrow", "wallet"]);

let renderToken = 0;
let lastPath = null;
let ticker = null;

export async function route() {
  const path = location.hash.replace(/^#/, "") || "/";
  const token = ++renderToken;
  clearInterval(ticker);
  for (const [re, fn, tab] of routes) {
    const m = path.match(re);
    if (!m) continue;
    document.querySelectorAll("nav a").forEach((a) => a.classList.toggle("active", a.dataset.route === tab));
    document.body.classList.remove("menu-open", "mega-open");
    view.className = "";
    if (GATED.has(tab)) await gate(ctx);
    if (!current(token)) return;
    if (!S.loaded && tab !== "explore" && tab !== "home") {
      view.innerHTML = `<div class="panel" style="max-width:460px;margin:40px auto"><h2>Loading Tacit</h2><p class="muted">Starting the shielded wallet and the Ironwood prover in your browser.</p><div class="progress spin"><i></i></div></div>`;
    }
    try {
      await fn(m, token);
    } catch (e) {
      if (token === renderToken) view.innerHTML = `<div class="panel"><h2>Something went wrong</h2><p class="muted">${esc(e.message)}</p></div>`;
    }
    if (path !== lastPath) window.scrollTo(0, 0);
    lastPath = path;
    reveals(view);
    glowCards(view);
    return;
  }
  view.innerHTML = `<div class="empty"><h2>Not found</h2><a href="#/">Back home</a></div>`;
}

export const current = (token) => token === renderToken;
export function every(ms, fn) {
  clearInterval(ticker);
  ticker = setInterval(fn, ms);
}

// Shared helpers handed to the other view modules.
const ctx = {
  S, call, api, modal, busy, toast, esc, h, zec, parseZec, short, ago, copy, unit, netName,
  storedWallet, saveScan, syncWallet, route, current, every, needWallet, phase, blockSecs: () => BLOCK_SECS,
  loadSeats, saveSeats, nextRefundIndex, prove, proveWith, warmProver, refreshChain: () => refreshChain(), roundTime,
  usdPill,
};

/** A title with its last words dimmed, like Morpho's vault pages. */
function titleSplit(text) {
  const words = esc(text).split(" ");
  if (words.length < 3) return words.join(" ");
  const cut = Math.max(1, words.length - 2);
  return `${words.slice(0, cut).join(" ")} <span class="dim">${words.slice(cut).join(" ")}</span>`;
}

/** Dims the fractional or unit part of a figure: 0.55 → 0<i>.55</i>. */
function dimUnit(s) {
  const m = String(s).match(/^([\d,]+)(.*)$/);
  return m && m[2] ? `${m[1]}<i>${esc(m[2])}</i>` : esc(s);
}

/** A grey pill with the USD value of an amount in zatoshi. Testnet coins have
 * no market value, so it only shows on mainnet. */
export function usdPill(zat) {
  const p = zecUsd();
  if (!p || S.net !== "main") return "";
  return `<span class="usd-pill">${usd((zat / 1e8) * p)}</span>`;
}

// ---------- auctions ----------

// Where an auction stands, from the chain tip and the clock.
export function phase(terms, tip) {
  const now = Date.now() / 1000;
  const opensAt = roundTime(terms.close_round);
  // Once the round is out, bids are readable, so bidding is over whatever
  // the height says.
  if (now >= opensAt) return { key: "open", label: "Bids unsealed", secs: 0, opensAt };
  // A deposit counts if it is mined at or below bids_until_height, so the
  // next block must still be inside the window.
  if (tip != null && tip < terms.bids_until_height) {
    return { key: "bidding", label: "Bidding open", secs: (terms.bids_until_height - tip) * BLOCK_SECS, opensAt };
  }
  return { key: "sealed", label: "Sealed", secs: opensAt - now, opensAt };
}

async function loadAuctions() {
  const list = await api("/v1/auctions");
  return list.filter((a) => a.terms.network === S.net || !S.net);
}

const FILTERS = [
  ["all", "All"],
  ["bidding", "Bidding open"],
  ["sealed", "Sealed"],
  ["open", "Unsealed"],
];

function marketRow(a) {
  const t = a.terms;
  const p = phase(t, S.info && S.info.tip);
  const when = p.key === "bidding" ? `~${ago(p.secs)}` : p.key === "sealed" ? `opens in ${ago(p.secs)}` : "now";
  const rule = t.rule === "first_price" || t.rule === "FirstPrice" ? "First price" : "Second price";
  return `<tr data-phase="${p.key}" data-item="${esc(t.item.toLowerCase())}" onclick="location.hash='#/auction/${a.id}'">
    <td><div class="lot"><span class="lot-icon">${esc(t.item.trim()[0] || "T").toUpperCase()}</span><div><b>${esc(t.item)}</b><span class="sub mono">${a.id.slice(0, 12)}</span></div></div></td>
    <td class="num">${zec(t.bond)} ${unit()}<br>${usdPill(t.bond)}</td>
    <td class="num hide-sm">${zec(t.reserve)} ${unit()}<br>${usdPill(t.reserve)}</td>
    <td><span class="badge ${p.key}">${p.label}</span></td>
    <td class="hide-sm">${rule}</td>
    <td class="num">${when}</td>
  </tr>`;
}

async function renderMarket(_, token) {
  view.innerHTML = `
    <div class="row between page-head" style="align-items:flex-end">
      <div><span class="eyebrow">Market</span><h1 id="mk-title">Sealed auctions</h1><p>Every bid here is locked until its drand round. Bonds are identical, so the chain shows nothing about who bids or how much.</p></div>
      <span class="total-pill">Bonds in sealed auctions <b id="mk-total">–</b></span>
    </div>
    <div class="headline-stats">
      <div><span>Live auctions</span><b id="mk-live">–</b></div>
      <div><span>Bidding now</span><b id="mk-bidding">–</b></div>
      <div><span>Agents online</span><b id="mk-agents">–</b></div>
      <div><span>ZEC price</span><b id="mk-zec">–</b></div>
    </div>
    <div class="table-card">
      <div class="table-tools">
        <div class="chips">${FILTERS.map(([k, l], i) => `<button class="chip ${i === 0 ? "on" : ""}" data-f="${k}">${l}</button>`).join("")}</div>
        <div class="row" style="gap:8px"><div class="search"><input id="mk-q" placeholder="Filter lots" /></div><a class="btn small primary" href="#/sell">List an item</a></div>
      </div>
      <div class="table-wrap"><table class="data-table"><thead><tr><th>Lot</th><th class="num">Bond</th><th class="num hide-sm">Reserve</th><th>Status</th><th class="hide-sm">Rule</th><th class="num">Closes</th></tr></thead>
      <tbody id="mk-rows"><tr><td colspan="6"><div class="progress spin" style="margin:16px 0"><i></i></div></td></tr></tbody></table></div>
    </div>`;
  scramble(view.querySelector("#mk-title"), { duration: 600 });
  let filter = "all";
  const apply = () => {
    const q = view.querySelector("#mk-q").value.trim().toLowerCase();
    view.querySelectorAll("#mk-rows tr[data-phase]").forEach((tr) => {
      tr.style.display = (filter === "all" || tr.dataset.phase === filter) && (!q || tr.dataset.item.includes(q)) ? "" : "none";
    });
  };
  view.querySelectorAll(".chip").forEach((c) => {
    c.onclick = () => {
      filter = c.dataset.f;
      view.querySelectorAll(".chip").forEach((x) => x.classList.toggle("on", x === c));
      apply();
    };
  });
  view.querySelector("#mk-q").oninput = apply;
  onPrices((p) => {
    const el = view.querySelector("#mk-zec");
    if (el && p.ZEC) el.textContent = usd(p.ZEC.usd);
  });
  prices().catch(() => {});
  await S.ready;
  await refreshChain();
  if (!S.info) {
    view.querySelector("#mk-rows").innerHTML = `<tr><td colspan="6">${offline()}</td></tr>`;
    return;
  }
  const draw = async () => {
    const [list, agents] = await Promise.all([loadAuctions(), api("/v1/agents").catch(() => [])]);
    if (!current(token)) return;
    S.auctions = list;
    const phases = list.map((a) => phase(a.terms, S.info.tip).key);
    view.querySelector("#mk-live").textContent = list.length;
    view.querySelector("#mk-bidding").textContent = phases.filter((k) => k === "bidding").length;
    view.querySelector("#mk-agents").textContent = agents.length;
    view.querySelector("#mk-total").textContent = `${zec(list.filter((_, i) => phases[i] !== "open").reduce((s, a) => s + a.terms.bond, 0))} ${unit()}`;
    view.querySelector("#mk-rows").innerHTML = list.length
      ? list.map(marketRow).join("")
      : `<tr><td colspan="6"><div class="empty"><h3>No auctions yet on ${netName(S.net)}</h3><p>Be the first: <a href="#/sell">list something</a>.</p></div></td></tr>`;
    apply();
  };
  await draw();
  every(20000, async () => {
    await refreshChain();
    if (current(token)) await draw();
  });
}

function offline() {
  return `<div class="panel" style="grid-column:1/-1">
    <h3>Can't reach the coordinator</h3>
    <p class="muted">Tacit couldn't talk to <span class="mono">${esc(gatewayUrl())}</span>. Use the button at the top right to point it at a running coordinator.</p>
  </div>`;
}

async function renderAuction(m, token) {
  const id = m[1];
  await S.ready;
  await refreshChain();
  const rec = await api(`/v1/auctions/${id}`);
  const terms = rec.terms;
  // Never trust the coordinator's id: recompute it from the terms.
  const computed = await call("auctionId", { terms });
  if (computed !== id) throw new Error("The coordinator's terms don't hash to this auction id. Refusing to show them.");
  const vault = await call("vaultAddress", { network: terms.network, ufvk: terms.vault_ufvk });
  if (!current(token)) return;

  const rule = terms.rule === "first_price" || terms.rule === "FirstPrice" ? "First price" : "Second price";
  view.innerHTML = `
    <div class="row between" style="margin:6px 0 14px">
      <a href="#/market" class="faint">← Market</a>
      <span id="phase-badge"></span>
    </div>
    <h1 class="title-split">${titleSplit(terms.item)}</h1>
    <div class="meta-chips">
      <span class="meta-chip mono" title="Auction id: the hash of every term on this page">${id.slice(0, 6)}…${id.slice(-4)}</span>
      <span class="meta-chip">${netName(terms.network)}</span>
      <span class="meta-chip">${rule}</span>
      <span class="meta-chip">2 of 3 FROST vault</span>
      <span class="meta-chip">drand round ${terms.close_round.toLocaleString()}</span>
    </div>
    <div class="big-stats">
      <div><span>Bond (max bid)</span><b>${dimUnit(zec(terms.bond))}</b><small>${unit()} ${usdPill(terms.bond)}</small></div>
      <div><span>Reserve</span><b>${dimUnit(zec(terms.reserve))}</b><small>${unit()} ${usdPill(terms.reserve)}</small></div>
      <div><span>Bids until block</span><b>${terms.bids_until_height.toLocaleString()}</b><small id="bs-left">&nbsp;</small></div>
      <div><span>Unseals</span><b id="bs-unseal">–</b><small>${new Date(roundTime(terms.close_round) * 1000).toLocaleString()}</small></div>
    </div>
    <div class="timeline" id="timeline"></div>
    <div class="split" style="margin-top:18px">
      <div>
        <div class="panel" id="status-panel"></div>
        <div class="panel" id="audit-panel">
          <div class="row between"><h2 style="margin:0">Public audit</h2><button class="btn small" id="run-audit">Run audit in this tab</button></div>
          <p class="muted" style="margin-top:10px">Uses nothing but the published terms, public chain data and the drand beacon. It finds every deposit to the vault, opens the bids once they unseal, recomputes the settlement and checks what the vault paid.</p>
          <div id="audit-out"></div>
        </div>
      </div>
      <div class="sticky-side">
        <div class="panel" id="bid-panel"></div>
        <div class="panel">
          <h2>Terms</h2>
          <dl class="kv">
            <dt>Reserve</dt><dd>${zec(terms.reserve)} ${unit()}</dd>
            <dt>Bond (max bid)</dt><dd>${zec(terms.bond)} ${unit()}</dd>
            <dt>Bids until block</dt><dd>${terms.bids_until_height.toLocaleString()}</dd>
            <dt>Unseals at</dt><dd>${new Date(roundTime(terms.close_round) * 1000).toLocaleString()}</dd>
            <dt>drand round</dt><dd class="mono">${terms.close_round}</dd>
            <dt>Network</dt><dd>${netName(terms.network)}</dd>
          </dl>
          <p class="faint" style="margin:14px 0 6px;font-size:13px">Vault (2 of 3 FROST)</p>
          <div class="addr">${esc(vault)}</div>
          <p class="faint" style="margin:14px 0 6px;font-size:13px">Audit key (public by design)</p>
          <div class="addr" style="max-height:84px;overflow:auto">${esc(terms.vault_ufvk)}</div>
        </div>
      </div>
    </div>`;

  const draw = () => {
    const p = phase(terms, S.info && S.info.tip);
    view.querySelector("#phase-badge").outerHTML = `<span id="phase-badge" class="badge ${p.key}">${p.label}</span>`;
    const left = Math.max(0, terms.bids_until_height - (S.info ? S.info.tip : 0));
    view.querySelector("#bs-left").textContent = left ? `${left} blocks left` : "bidding closed";
    view.querySelector("#bs-unseal").innerHTML = p.key === "open" ? "Now" : dimUnit(ago(p.opensAt - Date.now() / 1000));
    const steps = [
      ["Bidding", `until block ${terms.bids_until_height.toLocaleString()}`, "bidding"],
      ["Sealed", "bids locked until the round", "sealed"],
      ["Unsealed", "beacon published", "open"],
      ["Settled", "one shielded payout", "settled"],
    ];
    const order = ["bidding", "sealed", "open", "settled"];
    const at = order.indexOf(S.settled && S.settled[id] ? "settled" : p.key);
    view.querySelector("#timeline").innerHTML = steps
      .map(([t, s, k], i) => `<div class="step ${i < at ? "done" : i === at ? "now" : ""}"><b>${t}</b>${s}</div>`)
      .join("");
    const sp = view.querySelector("#status-panel");
    if (p.key === "bidding") {
      sp.innerHTML = `<div class="row between"><div><span class="faint">Bidding closes in about</span><div class="countdown">${ago(p.secs)}</div></div>
        <div class="stat" style="text-align:right"><b>${Math.max(0, terms.bids_until_height - S.info.tip)}</b><span>blocks left</span></div></div>
        <p class="muted" style="margin:10px 0 0">Every bid deposits the same ${zec(terms.bond)} ${unit()} bond, so on chain the deposits all look the same until the bids unseal.</p>`;
    } else if (p.key === "sealed") {
      sp.innerHTML = `<span class="faint">Bids unseal in</span><div class="countdown">${ago(p.secs)}</div>
        <p class="muted" style="margin:10px 0 0">Bidding is over. Until drand publishes round ${terms.close_round}, nobody can read a single bid: not the seller, not the operator, not us.</p>`;
    } else {
      sp.innerHTML = `<h2>Bids are unsealed</h2><p class="muted" style="margin:0">drand round ${terms.close_round} is out. Anyone can open the bids now, and the vault's seats can settle. Run the audit to see the result.</p>`;
    }
    drawBid(p);
  };

  let bidDrawn = null;
  const drawBid = (p) => {
    // Redraw only when the phase changes, so a half-typed bid survives the
    // periodic refresh.
    const key = `${p.key}:${!!S.wallet}`;
    if (key === bidDrawn) return;
    bidDrawn = key;
    const bp = view.querySelector("#bid-panel");
    if (p.key !== "bidding") {
      const mine = myBids().filter((b) => b.auction === id);
      bp.innerHTML = `<h2>Bidding closed</h2>${
        mine.length
          ? `<p class="muted">You bid ${mine.map((b) => `${zec(b.amount)} ${unit()}`).join(", ")}. Run the audit to see how it went. Refunds go to a fresh address in your wallet.</p>`
          : `<p class="muted">This auction no longer takes bids.</p>`
      }`;
      return;
    }
    if (!S.wallet) {
      bp.innerHTML = `<h2>Place a sealed bid</h2><p class="muted">You need a Tacit wallet with at least ${zec(terms.bond + 20000)} ${unit()} to bid. It lives in this browser and takes a minute to set up.</p><a class="btn primary wide" href="#/wallet">Open wallet</a>`;
      return;
    }
    const mid = Math.round((terms.reserve + terms.bond) / 2 / 1000) * 1000;
    bp.innerHTML = `
      <h2>Place a sealed bid</h2>
      <label class="field"><span>Your bid</span><div class="input-suffix"><input id="amt" inputmode="decimal" value="${zec(Math.max(terms.reserve, mid))}" /><b>${unit()}</b></div></label>
      <input type="range" id="slider" min="${terms.reserve}" max="${terms.bond}" step="${Math.max(1, Math.round((terms.bond - terms.reserve) / 200))}" value="${Math.max(terms.reserve, mid)}" style="margin:12px 0" />
      <dl class="kv" style="margin-bottom:14px">
        <dt>You deposit (bond)</dt><dd>${zec(terms.bond)} ${unit()}</dd>
        <dt>Network fee</dt><dd>~0.0001 ${unit()}</dd>
        <dt>Wallet balance</dt><dd>${zec(S.wallet.spendable)} ${unit()}</dd>
      </dl>
      <button class="btn primary wide" id="bid">Seal and deposit</button>
      <p class="faint" style="font-size:13px;margin:12px 0 0">If you win you pay ${rule === "First price" ? "your bid" : "the second-highest bid (or the reserve)"}, and the rest of the bond comes back. If you don't, the whole bond comes back to a fresh, unlinkable address.</p>`;
    const amt = bp.querySelector("#amt");
    const slider = bp.querySelector("#slider");
    slider.oninput = () => (amt.value = zec(slider.value));
    amt.oninput = () => {
      const v = parseZec(amt.value);
      if (v != null) slider.value = v;
    };
    bp.querySelector("#bid").onclick = () => placeBid(terms, id, parseZec(amt.value));
  };

  view.querySelector("#run-audit").onclick = () => runAudit(terms, id);
  draw();
  every(15000, async () => {
    await refreshChain();
    if (current(token)) draw();
  });
}

async function placeBid(terms, id, amount) {
  if (amount == null || amount < terms.reserve || amount > terms.bond) {
    toast(`Bid between ${zec(terms.reserve)} and ${zec(terms.bond)} ${unit()}`, "error");
    return;
  }
  const opens = new Date(roundTime(terms.close_round) * 1000).toLocaleString();
  const c = modal(`
    <h2>Seal this bid?</h2>
    <dl class="kv" style="margin:14px 0">
      <dt>Your bid</dt><dd><b>${zec(amount)} ${unit()}</b></dd>
      <dt>Bond deposited now</dt><dd>${zec(terms.bond)} ${unit()}</dd>
      <dt>Readable from</dt><dd>${esc(opens)}</dd>
    </dl>
    <p class="muted">Your bid is encrypted to drand round ${terms.close_round} and travels inside a shielded memo. Nobody can read it before then. Once you send it, you can't change or cancel it.</p>
    <div class="row"><span class="spacer"></span><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="go">Seal and send</button></div>`);
  c.querySelector("#go").onclick = async () => {
    c.close();
    const b = busy("Placing your sealed bid");
    try {
      const refundIndex = nextRefundIndex();
      b.onProgress({ label: "Starting the prover", spin: true });
      const r = await proveWith("bid", { terms, amount, refundIndex }, b.onProgress);
      addBid({ auction: id, item: terms.item, amount, txid: r.txid, refundIndex, time: Date.now() });
      b.close();
      modal(`
        <h2>Bid sealed and sent</h2>
        <p class="notice good">Your ${zec(terms.bond)} ${unit()} bond is on its way to the vault. Proving took ${(r.proveMs / 1000).toFixed(1)} s, right here in your browser.</p>
        <p class="faint" style="margin-top:12px;font-size:13px">Transaction</p><div class="addr">${esc(r.txid)}</div>
        <div class="row" style="margin-top:16px"><span class="spacer"></span><button class="btn primary" data-close>Done</button></div>`);
      route();
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };
}

async function runAudit(terms, id) {
  const out = view.querySelector("#audit-out");
  const btn = view.querySelector("#run-audit");
  btn.disabled = true;
  out.innerHTML = `<p class="muted" id="al">Starting…</p><div class="progress spin" id="ab"><i></i></div>`;
  const onProgress = (p) => {
    const l = out.querySelector("#al");
    const bar = out.querySelector("#ab");
    if (!l) return;
    l.textContent = p.label + (p.total ? ` · ${p.done} / ${p.total}` : "");
    if (p.total) {
      bar.className = "progress";
      bar.firstElementChild.style.width = `${Math.round((100 * p.done) / Math.max(1, p.total))}%`;
    }
  };
  try {
    const now = Date.now() / 1000;
    const b = now >= roundTime(terms.close_round) ? await beacon(terms.close_round) : null;
    const t0 = performance.now();
    const r = await call("audit", { terms, beacon: b }, onProgress);
    const secs = ((performance.now() - t0) / 1000).toFixed(1);
    S.settled = S.settled || {};
    if (r.verdict === "honest") S.settled[id] = true;
    out.innerHTML = auditHtml(r, terms, b, secs);
  } catch (e) {
    out.innerHTML = `<p class="notice bad">${esc(e.message)}</p>`;
  } finally {
    btn.disabled = false;
  }
}

function auditHtml(r, terms, b, secs) {
  const verdicts = {
    open: ["sealed", "Bids still sealed", `Found ${r.deposits.length} deposit${r.deposits.length === 1 ? "" : "s"}. Every bid stays sealed until drand round ${terms.close_round}.`],
    awaiting_settlement: ["open", "Unsealed, not settled yet", "The bids are open and the result is fixed. The vault hasn't paid out yet."],
    honest: ["honest", "Settled honestly", "The vault paid exactly what the rules require, no more and no less."],
    mismatch: ["bad", "Settlement doesn't match", "The vault's payments differ from what the rules require. See below."],
  };
  const [cls, title, text] = verdicts[r.verdict];
  const mine = new Set(myBids().map((x) => x.txid));
  const o = r.outcome;
  return `
    <div class="notice ${cls === "honest" ? "good" : cls === "bad" ? "bad" : ""}" style="margin-top:14px">
      <div class="row between"><b>${title}</b><span class="badge ${cls}">${r.verdict.replace("_", " ")}</span></div>
      <div style="margin-top:4px">${text}</div>
    </div>
    ${o ? `<div class="grid cols-2" style="margin-top:14px;grid-template-columns:repeat(3,1fr)">
      <div class="stat"><b>${o.price != null ? zec(o.price) : "–"}</b><span>clearing price (${unit()})</span></div>
      <div class="stat"><b>${r.deposits.filter((d) => d.bid != null).length}</b><span>bids opened</span></div>
      <div class="stat"><b>${zec(o.fee)}</b><span>network fee</span></div></div>` : ""}
    <div class="table-wrap" style="margin-top:14px"><table>
      <thead><tr><th>Deposit</th><th>Block</th><th class="num">Bond</th><th class="num">Bid</th><th>Status</th></tr></thead>
      <tbody>${
        r.deposits.length
          ? r.deposits
              .map((d) => {
                const txid = d.id.slice(0, 64).match(/../g).reverse().join("");
                return `<tr><td class="mono">${d.id.slice(0, 10)}…${mine.has(txid) ? ' <span class="badge bidding">you</span>' : ""}</td><td>${d.height}</td><td class="num">${zec(d.value)}</td><td class="num">${d.bid != null ? zec(d.bid) : '<span class="faint">sealed</span>'}</td><td>${esc(d.status)}</td></tr>`;
              })
              .join("")
          : `<tr><td colspan="5" class="faint">No deposits yet</td></tr>`
      }</tbody></table></div>
    ${r.settlement ? `<p class="faint" style="margin:14px 0 6px;font-size:13px">Settlement transaction (block ${r.settlement.height})</p><div class="addr">${esc(r.settlement.txid)}</div>` : ""}
    ${r.missing.length || r.unexpected.length ? `<p class="notice bad" style="margin-top:12px">Missing: ${r.missing.map((p) => `${zec(p.amount)} to ${short(p.to)}`).join(", ") || "none"}<br/>Unexpected: ${r.unexpected.map((p) => `${zec(p.amount)} to ${short(p.to)}`).join(", ") || "none"}</p>` : ""}
    <p class="faint" style="font-size:12.5px;margin:12px 0 0">Scanned to block ${r.scanned_to.toLocaleString()} in ${secs} s${b ? `, beacon for round ${b.round} verified against the drand quicknet key` : ""}. Who made each deposit stays private: the audit key shows amounts and memos, not senders.</p>`;
}

// ---------- wallet ----------

export async function needWallet() {
  if (S.wallet) return true;
  location.hash = "#/wallet";
  return false;
}

async function renderWallet(_, token) {
  await S.ready;
  await refreshChain();
  if (!S.info) {
    view.innerHTML = offline();
    return;
  }
  const rec = storedWallet();
  if (!S.wallet && rec) return renderUnlock(rec, token);
  if (!S.wallet) return renderOnboard(token);

  const w = S.wallet;
  const bids = myBids();
  view.innerHTML = `
    <div class="row between" style="margin-bottom:18px">
      <div><span class="eyebrow">${netName(S.net)} wallet</span><h1 style="font-size:34px;margin:0">${zec(w.balance)} <span class="muted" style="font-size:20px">${unit()}</span></h1>
      <span class="faint">${zec(w.spendable)} spendable · synced to block ${w.height.toLocaleString()}</span></div>
      <div class="row"><button class="btn" id="sync">Sync</button><button class="btn ghost" id="lock">Lock</button></div>
    </div>
    <div class="split">
      <div>
        <div class="panel">
          <div class="row between"><h2 style="margin:0">Receive</h2><div class="tabs"><button class="on" data-a="0">Main address</button><button data-a="fresh">Fresh address</button></div></div>
          <p class="muted" style="margin:10px 0">A shielded Ironwood address. Every fresh address is unlinkable to the others.</p>
          <div class="addr" id="addr">${esc(w.address)}</div>
          <div class="row" style="margin-top:12px"><button class="btn small" id="copy">Copy</button><a class="btn small" href="#/fund">Fund from another chain →</a>${S.net === "test" ? `<a class="btn small ghost" href="https://testnet.zecfaucet.com" target="_blank" rel="noopener">Testnet faucet ↗</a>` : ""}</div>
        </div>
        <div class="panel">
          <h2>Send</h2>
          <div class="stack">
            <label class="field"><span>To (unified address)</span><input id="to" placeholder="u1…" /></label>
            <label class="field"><span>Amount</span><div class="input-suffix"><input id="amount" inputmode="decimal" placeholder="0.0" /><b>${unit()}</b></div></label>
            <label class="field"><span>Memo (optional, encrypted)</span><input id="memo" maxlength="512" /></label>
            <button class="btn primary" id="send">Send shielded</button>
          </div>
        </div>
      </div>
      <div>
        <div class="panel">
          <h2>My bids</h2>
          ${bids.length ? `<table><tbody>${bids.map((b) => `<tr><td><a href="#/auction/${b.auction}">${esc(b.item)}</a><div class="faint mono" style="font-size:12px">${b.txid.slice(0, 16)}…</div></td><td class="num">${zec(b.amount)} ${unit()}</td></tr>`).join("")}</tbody></table>` : `<p class="muted">No bids yet. <a href="#/">Browse the market</a>.</p>`}
        </div>
        <div class="panel">
          <h2>Notes</h2>
          ${w.notes.length ? `<table><thead><tr><th>Block</th><th class="num">Value</th><th>State</th></tr></thead><tbody>${w.notes.slice().reverse().map((n) => `<tr><td>${n.height}</td><td class="num">${zec(n.value)}</td><td>${n.spent_in ? '<span class="faint">spent</span>' : n.pending ? '<span class="badge sealed">pending</span>' : n.change ? "change" : "received"}</td></tr>`).join("")}</tbody></table>` : `<p class="muted">Nothing received yet.</p>`}
          ${w.notes.some((n) => n.pending && !n.spent_in) ? `<button class="btn small ghost" id="clear" style="margin-top:10px">Release pending notes</button>` : ""}
        </div>
      </div>
    </div>`;

  view.querySelector("#copy").onclick = () => copy(view.querySelector("#addr").textContent);
  view.querySelectorAll(".tabs button").forEach((b) => {
    b.onclick = async () => {
      view.querySelectorAll(".tabs button").forEach((x) => x.classList.toggle("on", x === b));
      view.querySelector("#addr").textContent = b.dataset.a === "0" ? w.address : await call("address", { index: 1000 + Math.floor(Math.random() * 1e6) });
    };
  });
  view.querySelector("#sync").onclick = async () => {
    const b = busy("Syncing");
    try {
      const r = await call("sync", {}, b.onProgress);
      saveScan(r.saved);
      S.wallet = r.state;
      b.close();
      route();
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };
  view.querySelector("#lock").onclick = async () => {
    await call("closeWallet");
    S.wallet = null;
    S.pass = null;
    S.phrase = null;
    route();
  };
  const clear = view.querySelector("#clear");
  if (clear)
    clear.onclick = async () => {
      const r = await call("clearPending");
      saveScan(r.saved);
      S.wallet = r.state;
      route();
    };
  view.querySelector("#send").onclick = async () => {
    const to = view.querySelector("#to").value.trim();
    const amount = parseZec(view.querySelector("#amount").value);
    const memo = view.querySelector("#memo").value;
    if (!to || !amount) return toast("Enter an address and an amount", "error");
    const b = busy("Sending");
    try {
      b.onProgress({ label: "Starting the prover", spin: true });
      const r = await proveWith("send", { to, amount, memo }, b.onProgress);
      b.close();
      toast(`Sent. Proved in ${(r.proveMs / 1000).toFixed(1)} s.`, "good");
      route();
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };
  every(45000, async () => {
    if (!current(token)) return;
    const before = S.wallet && S.wallet.height;
    await syncWallet(true);
    if (current(token) && S.wallet && S.wallet.height !== before) route();
  });
}

function renderUnlock(rec, token) {
  view.innerHTML = `
    <div class="panel" style="max-width:460px;margin:40px auto">
      <span class="eyebrow">${netName(S.net)} wallet</span>
      <h2>Unlock</h2>
      <p class="muted">Your wallet is stored encrypted in this browser.</p>
      <label class="field"><span>Passcode</span><input id="pass" type="password" autocomplete="current-password" /></label>
      <button class="btn primary wide" id="go" style="margin-top:14px">Unlock</button>
      <button class="btn ghost wide" id="forget" style="margin-top:8px">Remove this wallet from the browser</button>
    </div>`;
  const go = async () => {
    try {
      const pass = view.querySelector("#pass").value;
      const phrase = await openPhrase(rec.sealed, pass);
      S.pass = pass;
      S.phrase = phrase;
      S.wallet = await call("openWallet", { phrase, saved: rec.saved });
      const b = busy("Catching up");
      try {
        const r = await call("sync", {}, b.onProgress);
        saveScan(r.saved);
        S.wallet = r.state;
      } catch (e) {
        toast(e.message, "error");
      }
      b.close();
      warmProver().catch(() => {});
      if (current(token)) route();
    } catch (e) {
      toast(e.message, "error");
    }
  };
  view.querySelector("#go").onclick = go;
  view.querySelector("#pass").onkeydown = (e) => e.key === "Enter" && go();
  view.querySelector("#forget").onclick = () => {
    const m = modal(`<h2>Remove wallet?</h2><p class="muted">This deletes the encrypted wallet from this browser. You can only get your funds back with the 24 word recovery phrase.</p>
      <div class="row"><span class="spacer"></span><button class="btn ghost" data-close>Cancel</button><button class="btn" id="yes">Remove</button></div>`);
    m.querySelector("#yes").onclick = () => {
      storeWallet(null);
      m.close();
      route();
    };
  };
}

function renderOnboard(token) {
  view.innerHTML = `
    <div style="max-width:620px;margin:20px auto">
      <span class="eyebrow">${netName(S.net)}</span>
      <h1 style="font-size:36px">Your wallet, in this tab</h1>
      <p class="muted">A Tacit wallet is a shielded Zcash wallet that runs entirely in your browser. It syncs by decrypting blocks locally and proves every transaction right here. The coordinator never sees a key or an address.</p>
      <div class="grid cols-2" style="margin-top:22px">
        <button class="panel" id="new" style="text-align:left;cursor:pointer;color:inherit"><h3>Create a new wallet</h3><p class="muted" style="margin:0">Get a 24 word recovery phrase and a fresh shielded address.</p></button>
        <button class="panel" id="restore" style="text-align:left;cursor:pointer;color:inherit;margin-top:0"><h3>Restore</h3><p class="muted" style="margin:0">Bring back a Tacit wallet from its recovery phrase.</p></button>
      </div>
    </div>`;
  view.querySelector("#new").onclick = async () => {
    const phrase = await call("newPhrase");
    const m = modal(`
      <h2>Write down your recovery phrase</h2>
      <p class="muted">These 24 words are the only way back into this wallet. Anyone who has them can spend your funds.</p>
      <div class="phrase">${phrase.split(" ").map((w) => `<span>${w}</span>`).join("")}</div>
      <label class="field" style="margin-top:16px"><span>Choose a passcode to lock it in this browser</span><input id="p1" type="password" autocomplete="new-password" /></label>
      <label class="row" style="margin-top:12px;gap:8px;color:var(--muted);font-size:14px"><input type="checkbox" id="ok" style="width:auto" /> I wrote the phrase down somewhere safe</label>
      <div class="row" style="margin-top:16px"><span class="spacer"></span><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="go">Create wallet</button></div>`);
    m.querySelector("#go").onclick = async () => {
      const pass = m.querySelector("#p1").value;
      if (pass.length < 6) return toast("Use a passcode of at least 6 characters", "error");
      if (!m.querySelector("#ok").checked) return toast("Confirm you've saved the phrase", "error");
      m.close();
      await createWallet(phrase, pass, null, token);
    };
  };
  view.querySelector("#restore").onclick = () => {
    const m = modal(`
      <h2>Restore a wallet</h2>
      <label class="field"><span>Recovery phrase</span><textarea id="ph" placeholder="24 words"></textarea></label>
      <label class="field" style="margin-top:12px"><span>Scan from block (the block the wallet was made, or earlier)</span><input id="bh" inputmode="numeric" placeholder="${S.info.tip - 1000}" /></label>
      <label class="field" style="margin-top:12px"><span>New passcode for this browser</span><input id="p1" type="password" autocomplete="new-password" /></label>
      <div class="row" style="margin-top:16px"><span class="spacer"></span><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="go">Restore</button></div>`);
    m.querySelector("#go").onclick = async () => {
      const phrase = m.querySelector("#ph").value.trim().toLowerCase().split(/\s+/).join(" ");
      if (!(await call("checkPhrase", { phrase }))) return toast("That isn't a valid recovery phrase", "error");
      const pass = m.querySelector("#p1").value;
      if (pass.length < 6) return toast("Use a passcode of at least 6 characters", "error");
      const bh = parseInt(m.querySelector("#bh").value, 10);
      m.close();
      await createWallet(phrase, pass, Number.isFinite(bh) ? Math.max(1, bh - 1) : Math.max(1, S.info.tip - 1000), token);
    };
  };
}

async function createWallet(phrase, pass, birthday, token) {
  const b = busy("Setting up your wallet");
  try {
    const r = await call("createWallet", { network: S.net, phrase, birthday }, b.onProgress);
    storeWallet({ sealed: await sealPhrase(phrase, pass), saved: r.saved, refund: 0, created: Date.now() });
    S.pass = pass;
    S.phrase = phrase;
    safeSet(`tacit.seats.${S.net}`, null);
    S.wallet = r.state;
    if (birthday != null) {
      const s = await call("sync", {}, b.onProgress);
      saveScan(s.saved);
      S.wallet = s.state;
    }
    b.close();
    warmProver().catch(() => {});
    toast("Wallet ready", "good");
    if (current(token)) route();
  } catch (e) {
    b.close();
    toast(e.message, "error");
  }
}

// ---------- how it works ----------

function renderHow() {
  view.innerHTML = `
    <span class="eyebrow">How Tacit works</span>
    <h1 style="font-size:clamp(28px,4vw,44px)">A sealed envelope, a shared safe, and a public ledger</h1>
    <p class="muted" style="max-width:720px;font-size:17px">Sealed-bid auctions are the fairest way to sell something scarce, but online they have always needed a trusted auctioneer who can read every bid. Tacit removes that auctioneer, using three tools Zcash and drand already provide.</p>
    <div class="flow" style="margin:26px 0">
      <div><h3>Lock the vault</h3><p>Seller, operator and arbiter run a distributed key generation. Together they make a 2-of-3 FROST Ironwood vault. Nobody ever holds the whole key.</p></div>
      <div><h3>Seal a bid</h3><p>Your browser encrypts the bid to a future drand round, puts it in a shielded memo and deposits a fixed bond to the vault. Every deposit looks the same.</p></div>
      <div><h3>Unseal</h3><p>When drand publishes that round, every bid becomes readable to everyone at the same moment. No one can peek early, and no one can refuse to reveal.</p></div>
      <div><h3>Settle and audit</h3><p>Two seats co-sign one shielded transaction: the seller is paid and the losers are refunded. Anyone can check it with the public audit key.</p></div>
    </div>
    <div class="grid cols-2">
      <div class="panel">
        <h2>Who learns what</h2>
        <table class="privacy-table"><thead><tr><th></th><th>Public</th><th>Seller</th><th>Operator</th></tr></thead><tbody>
          <tr><td>A bid, before close</td><td class="no">no</td><td class="no">no</td><td class="no">no</td></tr>
          <tr><td>Bids, after close</td><td class="yes">yes</td><td class="yes">yes</td><td class="yes">yes</td></tr>
          <tr><td>Who placed a bid</td><td class="no">no</td><td class="no">no</td><td class="no">no</td></tr>
          <tr><td>Clearing price</td><td class="yes">yes</td><td class="yes">yes</td><td class="yes">yes</td></tr>
          <tr><td>Where refunds go</td><td class="no">unlinkable</td><td class="no">unlinkable</td><td class="no">unlinkable</td></tr>
        </tbody></table>
        <p class="faint" style="font-size:13px;margin:12px 0 0">Timing and network metadata still exist. Bid over Tor, and fund your wallet ahead of time. The full threat model is in the repository.</p>
      </div>
      <div class="panel">
        <h2>Why it can't cheat</h2>
        <div class="stack">
          <p class="muted" style="margin:0"><b style="color:var(--text)">No early peeking.</b> Bids are timelock-encrypted with drand's threshold BLS network. Until the round's beacon exists, nobody can decrypt them, including the people who run Tacit.</p>
          <p class="muted" style="margin:0"><b style="color:var(--text)">No runaway custodian.</b> Bonds can move only with two of three FROST signatures, and each cosigner recomputes the settlement and refuses any payment the rules don't call for.</p>
          <p class="muted" style="margin:0"><b style="color:var(--text)">No hidden outcome.</b> The vault's viewing key is published in the terms, and the auction id commits to it. Any browser can replay the auction and check the payout.</p>
        </div>
      </div>
    </div>`;
}

// ---------- start ----------

// Header: glass when scrolled, mega menu, mobile menu, live ticker.
const top = document.getElementById("top");
addEventListener("scroll", () => top.classList.toggle("scrolled", scrollY > 8), { passive: true });
const megaTrigger = document.getElementById("mega-trigger");
const mega = document.getElementById("mega");
let megaTimer = 0;
const setMega = (open) => {
  clearTimeout(megaTimer);
  document.body.classList.toggle("mega-open", open);
  megaTrigger.setAttribute("aria-expanded", String(open));
};
megaTrigger.onclick = () => setMega(!document.body.classList.contains("mega-open"));
megaTrigger.onmouseenter = () => setMega(true);
[megaTrigger, mega].forEach((el) => (el.onmouseleave = () => (megaTimer = setTimeout(() => setMega(false), 220))));
mega.onmouseenter = () => clearTimeout(megaTimer);
addEventListener("keydown", (e) => e.key === "Escape" && setMega(false));
document.getElementById("menu-btn").onclick = () => document.body.classList.toggle("menu-open");
mountTicker(document.getElementById("ticker"), ["TACIT · SEALED MARKETS ON ZCASH", "TESTNET LIVE", "SHIELDS UP"]);
document.querySelectorAll(".wordmark").forEach((w) => w.parentElement.addEventListener("mouseenter", () => scramble(w, { duration: 500 })));

window.addEventListener("hashchange", route);
refreshChain().then(route);
