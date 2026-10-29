import { safeGet, safeSet } from "./lib.js";

// Funding a Tacit wallet from another chain through NEAR Intents 1Click.
//
// The quote names a fresh shielded address of this wallet as the recipient.
// 1Click hands back a one-time deposit address on the origin chain; once the
// user pays it, solvers deliver ZEC straight into the shielded pool. Tacit
// never touches the user's other-chain wallet: they pay from wherever they
// like.

const API = "https://1click.chaindefuser.com/v0";
const ZEC = "nep141:zec.omft.near";

const ORIGINS = [
  ["nep141:eth-0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48.omft.near", "USDC on Ethereum", 6],
  ["nep141:base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near", "USDC on Base", 6],
  ["nep141:arb-0xaf88d065e77c8cc2239327c5edb3a432268e5831.omft.near", "USDC on Arbitrum", 6],
  ["nep141:sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near", "USDC on Solana", 6],
  ["nep141:eth.omft.near", "ETH on Ethereum", 18],
  ["nep141:base.omft.near", "ETH on Base", 18],
  ["nep141:sol.omft.near", "SOL", 9],
  ["nep141:btc.omft.near", "BTC", 8],
];

// Well-formed stand-ins for a preview quote's refund address, one per chain.
// 1Click rejects the zero address.
function previewRefund(assetId) {
  if (assetId.startsWith("nep141:sol")) return "So11111111111111111111111111111111111111112";
  if (assetId.startsWith("nep141:btc")) return "bc1qar0srrr7xfkvy5l643lydnw9re59gtzzwf5mdq";
  return "0x1111111111111111111111111111111111111111";
}

// The refund address must belong to the chain the money comes from, or a
// failed swap would refund into nowhere. Only the format can be checked here.
function refundProblem(assetId, addr) {
  if (assetId.startsWith("nep141:btc")) {
    return /^(bc1[02-9ac-hj-np-z]{11,71}|[13][1-9A-HJ-NP-Za-km-z]{25,34})$/.test(addr) ? null : "That isn't a Bitcoin address (bc1…, 1… or 3…).";
  }
  if (assetId.startsWith("nep141:sol")) {
    return /^[1-9A-HJ-NP-Za-km-z]{32,44}$/.test(addr) ? null : "That isn't a Solana address.";
  }
  if (!/^0x[0-9a-fA-F]{40}$/.test(addr)) return "That isn't an EVM address (0x followed by 40 hex characters).";
  if (/^0x0{40}$/i.test(addr)) return "That's the zero address; refunds sent there are lost.";
  return null;
}

const usd2 = (v) => `$${Number(v).toFixed(2)}`;

function toUnits(s, decimals) {
  const m = String(s).trim().match(/^(\d*)(?:\.(\d*))?$/);
  if (!m || (!m[1] && !m[2])) return null;
  const frac = (m[2] || "").slice(0, decimals).padEnd(decimals, "0");
  return (BigInt(m[1] || "0") * 10n ** BigInt(decimals) + BigInt(frac || "0")).toString();
}

async function oneclick(path, body) {
  const res = await fetch(API + path, body ? { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) } : {});
  const j = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(j.message || `1Click: ${res.status}`);
  return j;
}

export async function renderFund(_, ctx) {
  const { S, call, toast, esc, copy, unit, busy } = ctx;
  const view = document.getElementById("view");
  await S.ready;
  if (!S.wallet) {
    location.hash = "#/wallet";
    return;
  }
  if (S.net !== "main") {
    view.innerHTML = `
      <a href="#/wallet" class="faint">← Wallet</a>
      <div class="panel" style="max-width:640px;margin:16px auto">
        <span class="eyebrow">Fund from another chain</span>
        <h2>Cross-chain funding runs on mainnet</h2>
        <p class="muted">NEAR Intents solvers settle real assets, so swapping in from Ethereum, Solana or Bitcoin only works on a mainnet wallet. On ${esc(ctx.netName(S.net))}, use a faucet or send from another wallet to your address.</p>
        <div class="addr">${esc(S.wallet.address)}</div>
        <div class="row" style="margin-top:12px"><button class="btn small" id="cp">Copy address</button>${S.net === "test" ? `<a class="btn small" href="https://testnet.zecfaucet.com" target="_blank" rel="noopener">Testnet faucet ↗</a>` : ""}</div>
      </div>`;
    view.querySelector("#cp").onclick = () => copy(S.wallet.address);
    return;
  }

  view.innerHTML = `
    <a href="#/wallet" class="faint">← Wallet</a>
    <div class="split" style="margin-top:14px">
      <div class="panel">
        <span class="eyebrow">Fund from another chain</span>
        <h2>Pay with almost anything, receive shielded ZEC</h2>
        <div class="stack">
          <label class="field"><span>Pay with</span><select id="origin">${ORIGINS.map(([id, name]) => `<option value="${id}">${esc(name)}</option>`).join("")}</select></label>
          <label class="field"><span>Amount to send</span><input id="amt" inputmode="decimal" placeholder="25" /></label>
          <label class="field"><span>Refund address on that chain (if the swap can't complete)</span><input id="refund" placeholder="0x… / bc1… / So1…" /></label>
          <div class="row"><button class="btn" id="quote">Get a quote</button><button class="btn primary" id="go" disabled>Get deposit address</button></div>
        </div>
        <div id="out"></div>
      </div>
      <div class="panel">
        <h2>How this stays private</h2>
        <div class="stack muted" style="font-size:14px">
          <p style="margin:0">Each funding uses a <b style="color:var(--text)">fresh shielded address</b> of your wallet as the recipient, so it can't be linked to your other addresses.</p>
          <p style="margin:0">The swap itself is public on the origin chain. Once the ZEC lands in the shielded pool, nothing links your bids or escrow payments back to it.</p>
          <p style="margin:0">Swaps run through <a href="https://near-intents.org" target="_blank" rel="noopener">NEAR Intents</a> 1Click. Tacit never holds your funds or keys on the other chain.</p>
        </div>
      </div>
    </div>`;

  const q = (s) => view.querySelector(s);
  const body = async (dry) => {
    const [assetId, , decimals] = ORIGINS.find((o) => o[0] === q("#origin").value);
    const amount = toUnits(q("#amt").value, decimals);
    if (!amount || amount === "0") throw new Error("Enter an amount");
    const refundTo = q("#refund").value.trim();
    if (!dry && !refundTo) throw new Error("Enter a refund address on the origin chain");
    if (refundTo) {
      const problem = refundProblem(assetId, refundTo);
      if (problem) throw new Error(problem);
    }
    const recipient = await call("address", { index: 2_000_000 + Math.floor(Math.random() * 1_000_000) });
    return {
      dry,
      swapType: "EXACT_INPUT",
      slippageTolerance: 100,
      originAsset: assetId,
      depositType: "ORIGIN_CHAIN",
      destinationAsset: ZEC,
      amount,
      // A preview quote moves nothing, but 1Click still wants a well-formed
      // address on the origin chain; the real quote always uses the user's.
      refundTo: refundTo || previewRefund(assetId),
      refundType: "ORIGIN_CHAIN",
      recipient,
      recipientType: "DESTINATION_CHAIN",
      deadline: new Date(Date.now() + 60 * 60 * 1000).toISOString(),
      referral: "tacit",
    };
  };

  q("#quote").onclick = async () => {
    const b = busy("Asking solvers for a quote");
    b.onProgress({ label: "NEAR Intents solvers are bidding for your swap. This can take up to a minute.", spin: true });
    try {
      const r = await oneclick("/quote", await body(true));
      b.close();
      q("#out").innerHTML = `<div class="notice" style="margin-top:14px">You'd receive about <b style="color:var(--text)">${esc(r.quote.amountOutFormatted)} ZEC</b> (${usd2(r.quote.amountOutUsd)}), in roughly ${Math.ceil(r.quote.timeEstimate / 60)} min after your deposit confirms.</div>`;
      q("#go").disabled = false;
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };

  // A price is only good for the inputs it was quoted on.
  for (const id of ["#origin", "#amt", "#refund"]) {
    q(id).addEventListener("input", () => {
      q("#go").disabled = true;
      q("#out").innerHTML = "";
    });
  }

  const showDeposit = (qd, origin) => {
    q("#out").innerHTML = `
        <div class="notice good" style="margin-top:14px">Send exactly <b>${esc(qd.amountInFormatted)} ${esc(origin)}</b> to this one-time address before ${new Date(qd.deadline).toLocaleString()}. You'll receive about <b>${esc(qd.amountOutFormatted)} ZEC</b>.</div>
        <div class="notice warn" style="margin-top:10px">Send only <b>${esc(origin)}</b>, from that network, in one payment. Any other coin or network can't be swapped. If the swap can't complete, the money goes back to your refund address.</div>
        <div class="addr" style="margin-top:10px">${esc(qd.depositAddress)}</div>
        ${qd.depositMemo ? `<p class="muted" style="margin-top:8px">Memo (required): <span class="mono">${esc(qd.depositMemo)}</span></p>` : ""}
        <div class="row" style="margin-top:10px"><button class="btn small" id="cp">Copy</button><span class="faint" id="status">Waiting for your deposit…</span></div>
        <p class="faint" style="font-size:13px;margin-top:8px">This page remembers the swap, so you can reload it. The ZEC arrives in this wallet by itself.</p>`;
    q("#cp").onclick = () => copy(qd.depositAddress);
    poll(qd);
  };

  q("#go").onclick = async () => {
    const b = busy("Getting your deposit address");
    b.onProgress({ label: "Locking in the quote with a solver…", spin: true });
    try {
      const r = await oneclick("/quote", await body(false));
      b.close();
      const qd = r.quote;
      const origin = ORIGINS.find((o) => o[0] === q("#origin").value)[1];
      safeSet("tacit.fund.last", JSON.stringify({ qd, origin }));
      showDeposit(qd, origin);
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };

  // Follows the swap until it finishes; once it has, the page forgets it.
  function poll(qd) {
    const check = async () => {
      const el = q("#status");
      if (!el || !document.body.contains(el)) return clearInterval(timer);
      try {
        const st = await oneclick(`/status?depositAddress=${encodeURIComponent(qd.depositAddress)}`);
        el.textContent = {
          PENDING_DEPOSIT: "Waiting for your deposit…",
          KNOWN_DEPOSIT_TX: "Deposit seen, confirming…",
          PROCESSING: "Swapping…",
          SUCCESS: `Done. ZEC is on its way to your shielded address. Sync your wallet in a minute.`,
          INCOMPLETE_DEPOSIT: "Deposit was short; you'll be refunded.",
          REFUNDED: "Refunded to your refund address.",
          FAILED: "The swap failed; you'll be refunded.",
        }[st.status] || st.status;
        if (["SUCCESS", "REFUNDED", "FAILED"].includes(st.status)) {
          clearInterval(timer);
          safeSet("tacit.fund.last", "");
        }
      } catch {}
    };
    const timer = setInterval(check, 8000);
    check();
  }

  try {
    const last = JSON.parse(safeGet("tacit.fund.last") || "null");
    if (last && last.qd && new Date(last.qd.deadline).getTime() > Date.now() - 24 * 3600 * 1000) showDeposit(last.qd, last.origin);
  } catch {}
  void unit;
}
