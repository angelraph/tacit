// The landing page: a hero that sells the idea in one look, live numbers,
// a pinned story of a sealed bid, and every way into the app.

import { reveals, scramble, idleGlitch, blurSweep, countUp, glowCards, onScrollProgress } from "./motion.js";
import { prices, onPrices, zecWeek, sparkline, usd, pct, coinIcon, COINS } from "./prices.js";
import { FAQ, faqList, wireFaq } from "./faq.js";

const TRIO = [
  {
    k: "01",
    title: "Seals every bid",
    body: "Your browser encrypts the bid to a future drand round and hides it in a shielded memo. Until that round is published, nobody on Earth can read it: not the seller, not the operator, not us.",
    vis: "seal",
  },
  {
    k: "02",
    title: "Settles itself",
    body: "Bonds sit in a 2-of-3 FROST vault that no single party controls. When the bids open, an operator and an arbiter each recompute the result and cosign exactly that payout, nothing else.",
    vis: "vault",
  },
  {
    k: "03",
    title: "Audits in public",
    body: "The vault's viewing key is part of the auction terms. Any browser can replay the auction from the chain and the beacon, and prove the vault paid what the rules demand, without learning who bid.",
    vis: "audit",
  },
];

const STORY = [
  { k: "THE PROBLEM", h: "Online auctions have an auctioneer who sees everything.", p: "Every bid, every bidder, every reserve. Whoever runs the auction can front-run it, leak it, or quietly change the outcome." },
  { k: "STEP 1 · SEAL", h: "Tacit puts every bid in the dark.", p: "Bids are timelock-encrypted to a drand round and deposited as identical shielded bonds. On chain, every bid looks the same." },
  { k: "STEP 2 · REVEAL", h: "At the bell, they open for everyone at once.", p: "drand publishes the round. Every bid becomes readable to everyone in the same instant, and no bidder can refuse to reveal." },
  { k: "STEP 3 · SETTLE", h: "The price is public. The people never were.", p: "The vault pays the seller and refunds everyone else in one shielded transaction. Anyone can verify it. Nobody learns who bid." },
];

const GO = [
  { href: "#/market", t: "Place a sealed bid", d: "Browse live auctions and bid without being seen.", art: ["#6d5cff", "#2ee6b6"] },
  { href: "#/sell", t: "Sell something", d: "Open a vault with two agents and publish in a minute.", art: ["#9a86ff", "#ff9f43"] },
  { href: "#/escrow", t: "Start an escrow", d: "Pay a stranger with an arbiter, privately.", art: ["#4a43f0", "#b6a8ff"] },
  { href: "#/wallet", t: "Open your wallet", d: "A shielded Zcash wallet that lives in this tab.", art: ["#2ee6b6", "#6d5cff"] },
  { href: "#/fund", t: "Fund from any chain", d: "Turn BTC, ETH, SOL or USDC into shielded ZEC.", art: ["#f4b728", "#6d5cff"] },
  { href: "#/agents", t: "Run an agent", d: "Be an operator or arbiter seat for the network.", art: ["#ff6b81", "#4a43f0"] },
];

function goArt([a, b]) {
  return `<div class="go-art"><i style="width:150px;height:150px;left:12%;top:-30px;background:${a}"></i><i style="width:120px;height:120px;right:14%;top:40px;background:${b}"></i><i style="width:80px;height:80px;left:48%;top:60px;background:#fff;opacity:.25"></i></div>`;
}

function storyArt() {
  const rays = Array.from({ length: 96 }, (_, i) => {
    const a = (i / 96) * Math.PI * 2;
    const r1 = 196 + (i % 3) * 6;
    const r2 = r1 + 26 + (i % 4) * 10;
    return `<line x1="${300 + Math.cos(a) * r1}" y1="${300 + Math.sin(a) * r1}" x2="${300 + Math.cos(a) * r2}" y2="${300 + Math.sin(a) * r2}"/>`;
  }).join("");
  const bids = [
    [230, 250, "0.42"],
    [360, 220, "0.61"],
    [300, 330, "0.55"],
    [205, 370, "0.38"],
    [395, 355, "0.70"],
  ]
    .map(
      ([x, y, v], i) => `<g class="bid" transform="translate(${x} ${y})" style="--i:${i}">
        <circle r="34" fill="#0b0918" stroke="url(#bidGrad)" stroke-width="1.5"/>
        <text class="sealed" text-anchor="middle" dy="5" fill="#b6a8ff" font-family="JetBrains Mono" font-size="15">▒▒▒</text>
        <text class="opened" text-anchor="middle" dy="5" fill="#fff" font-family="Space Grotesk" font-weight="700" font-size="17" opacity="0">${v}</text>
      </g>`,
    )
    .join("");
  return `<svg viewBox="0 0 600 600" aria-hidden="true">
    <defs>
      <linearGradient id="rayGrad" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#9a86ff"/><stop offset="1" stop-color="#2ee6b6"/></linearGradient>
      <linearGradient id="bidGrad" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#9a86ff"/><stop offset="1" stop-color="#4a43f0"/></linearGradient>
      <radialGradient id="sun"><stop offset="0" stop-color="#6d5cff" stop-opacity=".55"/><stop offset="1" stop-color="#6d5cff" stop-opacity="0"/></radialGradient>
    </defs>
    <circle cx="300" cy="300" r="290" fill="url(#sun)"/>
    <g class="rays" style="transform-origin:300px 300px">${rays}</g>
    <circle cx="300" cy="300" r="186" fill="none" stroke="rgba(182,168,255,.25)"/>
    ${bids}
    <circle class="eclipse" cx="300" cy="300" r="190" fill="#06060a" stroke="rgba(182,168,255,.35)"/>
    <g class="check" opacity="0"><circle cx="300" cy="300" r="56" fill="#0b0918" stroke="#2ee6b6" stroke-width="2"/><path d="M276 300l16 16 32-34" fill="none" stroke="#2ee6b6" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/></g>
  </svg>`;
}

function trioVisual(kind) {
  const rows = {
    seal: ["sealed bid", [["encrypted to", "a future drand round"], ["carried in", "a shielded memo"], ["deposit", "the same bond as everyone"], ["readable", "by everyone, at the bell"]]],
    vault: ["2 of 3 FROST vault", [["seat 1", "seller"], ["seat 2", "operator · recomputes, then signs"], ["seat 3", "arbiter · recomputes, then signs"], ["whole key", "never exists anywhere"]]],
    audit: ["audit from public terms", [["deposits", "found with the viewing key"], ["beacon", "checked against drand quicknet"], ["bids", "opened and ranked"], ["settlement", "compared payment by payment"]]],
  }[kind];
  const color = { seal: "#4a43f0,#9a86ff,#2ee6b6,#4a43f0", vault: "#2ee6b6,#6d5cff,#9a86ff,#2ee6b6", audit: "#9a86ff,#2ee6b6,#4a43f0,#9a86ff" }[kind];
  return `<div class="vis-seal"><div class="blob" style="background:conic-gradient(${color})"></div><div class="cipher"><b>${rows[0]}</b>${rows[1]
    .map(([k, v]) => `<div class="row between" style="gap:12px;margin-top:6px"><span class="faint">${k}</span><span>${v}</span></div>`)
    .join("")}</div></div>`;
}

export async function renderLanding(ctx) {
  const { S, api, esc, zec, unit, phase, ago } = ctx;
  const view = document.getElementById("view");
  view.className = "wide";
  view.innerHTML = `
    <section class="hero">
      <div class="ring"><i></i><i></i></div>
      <div class="hero-inner">
        <span class="eyebrow reveal">Private markets on Zcash</span>
        <h1 class="hero-title" id="hero-title">
          <span class="line"><span class="word">Sealed</span> <span class="word">bids.</span></span>
          <span class="line"><span class="word">Public</span> <span class="word grad-text">price.</span></span>
          <span class="line"><span class="word">Private</span> <span class="word">people.</span></span>
        </h1>
        <p class="hero-sub reveal" style="--d:.15s">Tacit runs sealed-bid auctions and escrow on Zcash's shielded pool. Bids stay locked until a public beacon opens them for everyone at once, and anyone can audit the result without learning who took part.</p>
        <div class="hero-cta reveal" style="--d:.3s">
          <a class="btn primary" href="#/try">Try it in 3 minutes <span class="arrow">↗</span></a>
          <a class="btn glass" href="#/market">Launch app</a>
          <a class="btn glass" href="#/proof">See the proof</a>
        </div>
      </div>
      <div class="price-card left" id="pc-zec">
        <div class="pc-head"><span class="row" style="gap:8px">${coinIcon("ZEC", 20)} Zcash · live</span><span id="pc-zec-chg"></span></div>
        <div class="pc-price" id="pc-zec-price">–</div>
        <div class="faint" style="font-size:12px">7 day price</div>
        <div id="pc-zec-spark"></div>
      </div>
      <div class="price-card" id="pc-net">
        <div class="pc-head"><span class="row" style="gap:8px"><img src="./logo.svg" width="20" alt=""> Tacit network</span><span class="badge settled" style="height:20px">live</span></div>
        <div class="pc-price" id="pc-tip">–</div>
        <div class="faint" style="font-size:12px" id="pc-net-sub">block height</div>
        <div class="row" style="margin-top:12px;gap:16px;font-size:13px"><span><b id="pc-auctions">–</b> <span class="muted">auctions</span></span><span><b id="pc-agents">–</b> <span class="muted">agents</span></span></div>
      </div>
      <a class="scroll-hint" href="#stats"><span>SCROLL</span><i></i></a>
    </section>

    <section class="stats-band" id="stats">
      <div class="reveal"><span>ZEC price</span><b id="st-zec">–</b><small id="st-zec-chg">live from CoinGecko</small></div>
      <div class="reveal" style="--d:.08s"><span>Live auctions</span><b id="st-auctions">–</b><small>sealed and settling on chain</small></div>
      <div class="reveal" style="--d:.16s"><span>Agents online</span><b id="st-agents">–</b><small>operators and arbiters</small></div>
      <div class="reveal" style="--d:.24s"><span>Chain height</span><b id="st-tip">–</b><small id="st-net">connecting…</small></div>
    </section>

    <div class="asset-marquee" aria-label="Fund from"><div class="asset-track" id="assets"></div></div>

    <section class="story" id="story">
      <div class="story-pin">
        <div class="story-art" id="story-art">${storyArt()}</div>
        <div class="story-steps">
          ${STORY.map((s, i) => `<div class="story-step ${i === 0 ? "on" : ""}"><div class="k">${s.k}</div><h3>${s.h}</h3><p>${s.p}</p></div>`).join("")}
          <div class="story-dots">${STORY.map((_, i) => `<i class="${i === 0 ? "on" : ""}"></i>`).join("")}</div>
        </div>
      </div>
    </section>

    <section class="section trio-wrap" data-tab="0" id="trio">
      <div class="columns">${"<i></i>".repeat(6)}</div>
      <div class="trio">${TRIO.map((t, i) => `<button class="${i === 0 ? "on" : ""}" data-i="${i}"><small>+ ${t.k}</small>Tacit<br>${t.title}</button>`).join("")}</div>
      <div class="trio-body" id="trio-body"></div>
    </section>

    <section class="section">
      <div class="steps-panel reveal">
        <span class="eyebrow">Get started</span>
        <h2 style="font:700 clamp(30px,4.4vw,56px)/.95 var(--display);text-transform:uppercase;letter-spacing:-.04em;margin:0">Four steps. No custodian.</h2>
        <div class="steps">
          ${[
            ["Open a wallet", "Create a shielded wallet in this tab. Keys never leave it.", '<rect x="5" y="9" width="22" height="16" rx="3"/><path d="M20 17h7M5 13h22"/>'],
            ["Fund it", "Tap for free testnet coins, connect Noir or any Zcash wallet, or swap in from any chain.", '<circle cx="11" cy="16" r="6"/><circle cx="21" cy="16" r="6"/>'],
            ["Seal a bid", "Pick a lot, set your price, press seal. Proving takes about 20 seconds.", '<rect x="7" y="13" width="18" height="13" rx="3"/><path d="M11 13V9a5 5 0 0110 0v4"/>'],
            ["Watch it settle", "When the round opens, agents settle and you audit it yourself.", '<circle cx="16" cy="16" r="11"/><path d="M11 16l4 4 7-8"/>'],
          ]
            .map(
              ([t, d, ico], i) => `<div class="stepc reveal" style="--d:${0.1 * i}s"><div class="ico"><svg viewBox="0 0 32 32" fill="none" stroke="#b6a8ff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">${ico}</svg><span class="n">${i + 1}</span></div><h3>${t}</h3><p>${d}</p></div>`,
            )
            .join("")}
        </div>
      </div>
    </section>

    <section class="section" style="padding-top:20px">
      <div class="section-head reveal">
        <h2>Live<br>auctions</h2>
        <div class="row"><span class="total-pill">Bonds sealed <b id="sealed-total">–</b></span><a class="btn" href="#/market">Open market <span class="arrow">↗</span></a></div>
      </div>
      <div class="table-card reveal"><div class="table-wrap"><table class="data-table"><thead><tr><th>Lot</th><th class="num">Bond</th><th class="num hide-sm">Reserve</th><th>Status</th><th class="num hide-sm">Closes</th></tr></thead><tbody id="live-rows"><tr><td colspan="5"><div class="progress spin"><i></i></div></td></tr></tbody></table></div></div>
    </section>

    <section class="section">
      <div class="section-head reveal"><h2>Get<br>started</h2><p>Every part of Tacit runs in your browser against public infrastructure. Pick a door.</p></div>
      <div class="go-grid">
        ${GO.map((g, i) => `<a class="go reveal" style="--d:${0.06 * i}s" href="${g.href}">${goArt(g.art)}<div class="go-body"><h3>${g.t} <span class="arrow">↗</span></h3><p>${g.d}</p></div></a>`).join("")}
      </div>
    </section>

    <section class="section">
      <div class="section-head reveal"><h2>Who learns<br>what</h2><p>Privacy you can state in a table. The full threat model is in the repository.</p></div>
      <div class="table-card reveal"><div class="table-wrap"><table class="privacy-table">
        <thead><tr><th style="padding-left:18px"></th><th>Public</th><th>Seller</th><th>Operator</th><th class="hide-sm">Auditor</th></tr></thead>
        <tbody>
          <tr><td style="padding-left:18px">A bid, before the bell</td><td class="no">no</td><td class="no">no</td><td class="no">no</td><td class="no hide-sm">no</td></tr>
          <tr><td style="padding-left:18px">Bids, after the bell</td><td class="yes">yes</td><td class="yes">yes</td><td class="yes">yes</td><td class="yes hide-sm">yes</td></tr>
          <tr><td style="padding-left:18px">Who placed a bid</td><td class="no">no</td><td class="no">no</td><td class="no">no</td><td class="no hide-sm">no</td></tr>
          <tr><td style="padding-left:18px">Clearing price</td><td class="yes">yes</td><td class="yes">yes</td><td class="yes">yes</td><td class="yes hide-sm">yes</td></tr>
          <tr><td style="padding-left:18px">Where refunds go</td><td class="no">unlinkable</td><td class="no">unlinkable</td><td class="no">unlinkable</td><td class="no hide-sm">unlinkable</td></tr>
        </tbody></table></div></div>
    </section>

    <section class="section">
      <div class="faq">
        <div class="reveal"><h2>Frequently<br>asked<br>questions</h2><p class="muted">Still unsure? <a href="#/faq">Read every answer</a> or <a href="#/how">see how it works</a>.</p></div>
        <div class="reveal" style="--d:.1s">${faqList(FAQ.slice(0, 6))}</div>
      </div>
    </section>

    <section class="section" style="padding-top:10px">
      <div class="cta-strip reveal"><h3>Testnet is live. Seal your first bid.</h3><a class="btn" href="#/wallet">Open wallet <span class="arrow">↗</span></a></div>
    </section>`;

  // ---------- motion ----------
  const title = view.querySelector("#hero-title");
  title.querySelectorAll(".word").forEach((w, i) => setTimeout(() => scramble(w, { duration: 700 }), 120 * i));
  blurSweep(title);
  setTimeout(() => title.querySelectorAll(".word").forEach((w, i) => i % 2 === 0 && idleGlitch(w, 4200 + i * 700)), 2500);
  reveals(view);
  glowCards(view);
  wireFaq(view);

  // pinned story
  const steps = view.querySelectorAll(".story-step");
  const dots = view.querySelectorAll(".story-dots i");
  const art = view.querySelector("#story-art");
  const eclipse = art.querySelector(".eclipse");
  const rays = art.querySelector(".rays");
  const check = art.querySelector(".check");
  onScrollProgress(view.querySelector("#story"), (p) => {
    const idx = Math.min(STORY.length - 1, Math.floor(p * STORY.length * 0.999));
    steps.forEach((s, i) => s.classList.toggle("on", i === idx));
    dots.forEach((d, i) => d.classList.toggle("on", i === idx));
    // 0 → 0.25: shadow slides over the bids; 0.25 → 0.5: they stay sealed;
    // 0.5 → 0.75: it slides away and the bids open; then a check.
    const cover = p < 0.25 ? p / 0.25 : p < 0.5 ? 1 : p < 0.75 ? 1 - (p - 0.5) / 0.25 : 0;
    eclipse.setAttribute("cx", String(300 - 520 * (1 - cover)));
    eclipse.setAttribute("opacity", String(Math.min(1, cover * 1.4)));
    rays.style.transform = `rotate(${p * 120}deg)`;
    const open = p >= 0.55;
    art.querySelectorAll(".bid").forEach((b) => {
      b.querySelector(".sealed").setAttribute("opacity", open ? "0" : "1");
      b.querySelector(".opened").setAttribute("opacity", open ? "1" : "0");
    });
    check.setAttribute("opacity", p > 0.8 ? "1" : "0");
  });

  // trio
  const trio = view.querySelector("#trio");
  const body = view.querySelector("#trio-body");
  const show = (i) => {
    trio.dataset.tab = String(i);
    trio.querySelectorAll(".trio button").forEach((b, j) => b.classList.toggle("on", j === i));
    body.innerHTML = `<div class="trio-text" style="--col:1"><p>${TRIO[i].body}</p><a class="btn small" href="${["#/market", "#/how", "#/market"][i]}">${["Try a sealed bid", "How the vault works", "Audit an auction"][i]} <span class="arrow">↗</span></a></div><div class="trio-visual">${trioVisual(TRIO[i].vis)}</div>`;
  };
  trio.querySelectorAll(".trio button").forEach((b) => (b.onclick = () => show(Number(b.dataset.i))));
  show(0);
  let auto = setInterval(() => {
    if (!document.body.contains(trio)) return clearInterval(auto);
    if (trio.matches(":hover")) return;
    show((Number(trio.dataset.tab) + 1) % TRIO.length);
  }, 7000);

  // ---------- live data ----------
  const assets = view.querySelector("#assets");
  const drawAssets = (p) => {
    const once = COINS.map((c) => {
      const q = p?.[c.sym];
      return `<div class="asset">${coinIcon(c.sym, 34)}<div><b>${c.name}</b><span>${q ? `${usd(q.usd)} <span style="color:${q.change >= 0 ? "var(--teal)" : "var(--red)"}">${pct(q.change)}</span>` : c.sym}</span></div></div>`;
    }).join("");
    assets.innerHTML = once + once;
  };
  drawAssets(null);
  onPrices((p) => {
    if (!document.body.contains(assets)) return;
    drawAssets(p);
    const z = p.ZEC;
    if (z) {
      view.querySelector("#pc-zec-price").textContent = usd(z.usd);
      const chg = view.querySelector("#pc-zec-chg");
      chg.textContent = pct(z.change);
      chg.style.color = z.change >= 0 ? "var(--teal)" : "var(--red)";
      countUp(view.querySelector("#st-zec"), z.usd, { prefix: "$", decimals: 2 });
      view.querySelector("#st-zec-chg").textContent = `${pct(z.change)} in 24 hours`;
    }
  });
  prices().catch(() => {});
  zecWeek().then((w) => {
    const el = view.querySelector("#pc-zec-spark");
    if (el) el.innerHTML = sparkline(w, { stroke: "#9a86ff" });
  });

  const refresh = async () => {
    try {
      const [info, list, agents] = await Promise.all([api("/v1/chain/info"), api("/v1/auctions"), api("/v1/agents").catch(() => [])]);
      if (!document.body.contains(view.querySelector("#st-tip"))) return;
      const net = info.chain;
      const live = list.filter((a) => a.terms.network === net);
      countUp(view.querySelector("#st-tip"), info.tip);
      countUp(view.querySelector("#pc-tip"), info.tip);
      view.querySelector("#st-net").textContent = `Zcash ${ctx.netName(net)} · ${esc(info.node.split(" ")[0])}`;
      view.querySelector("#pc-net-sub").textContent = `${ctx.netName(net)} block height`;
      countUp(view.querySelector("#st-auctions"), live.length);
      view.querySelector("#pc-auctions").textContent = live.length;
      countUp(view.querySelector("#st-agents"), agents.length);
      view.querySelector("#pc-agents").textContent = agents.length;
      const open = live.filter((a) => phase(a.terms, info.tip).key !== "open");
      view.querySelector("#sealed-total").textContent = `${zec(open.reduce((s, a) => s + a.terms.bond, 0))} ${unit()}`;
      const rows = live.slice(0, 6);
      view.querySelector("#live-rows").innerHTML = rows.length
        ? rows
            .map((a) => {
              const t = a.terms;
              const ph = phase(t, info.tip);
              const when = ph.key === "bidding" ? `in ~${ago(ph.secs)}` : ph.key === "sealed" ? `opens in ${ago(ph.secs)}` : "open";
              return `<tr onclick="location.hash='#/auction/${a.id}'"><td><div class="lot"><span class="lot-icon">${esc(t.item.trim()[0] || "T").toUpperCase()}</span><div><b>${esc(t.item)}</b><span class="sub mono">${a.id.slice(0, 10)}</span></div></div></td>
                <td class="num">${zec(t.bond)} ${unit()}${ctx.usdPill(t.bond)}</td><td class="num hide-sm">${zec(t.reserve)} ${unit()}</td>
                <td><span class="badge ${ph.key}">${ph.label}</span></td><td class="num hide-sm">${when}</td></tr>`;
            })
            .join("")
        : `<tr><td colspan="5" class="empty">No auctions yet on ${ctx.netName(net)}. <a href="#/sell">Be the first to list one.</a></td></tr>`;
    } catch {
      const r = view.querySelector("#live-rows");
      if (r) r.innerHTML = `<tr><td colspan="5" class="empty">The coordinator is offline right now.</td></tr>`;
    }
  };
  await refresh();
  ctx.every(20000, refresh);
}
