// The pages a product needs besides the app itself: FAQ, the agent
// directory, terms and risks, and the one-time risk gate.

import { FAQ, faqList, wireFaq } from "./faq.js";
import { reveals, glowCards } from "./motion.js";

export function renderFaq() {
  const view = document.getElementById("view");
  view.innerHTML = `
    <div class="page-head reveal"><span class="eyebrow">FAQ</span><h1>Questions, answered plainly</h1><p>What Tacit hides, what it shows, who holds the money and what it costs. If something here is unclear, the protocol and threat model on GitHub are the final word.</p></div>
    <div class="faq">
      <div class="reveal">
        <div class="panel">
          <h2>Still curious?</h2>
          <p class="muted">See the four steps a sealed bid goes through, or read the exact rules the code implements.</p>
          <div class="stack">
            <a class="btn wide" href="#/how">How it works <span class="arrow">↗</span></a>
            <a class="btn wide ghost" href="https://github.com/angelraph/tacit/blob/main/docs/PROTOCOL.md" target="_blank" rel="noopener">Protocol rules <span class="arrow">↗</span></a>
            <a class="btn wide ghost" href="https://github.com/angelraph/tacit/blob/main/docs/THREAT_MODEL.md" target="_blank" rel="noopener">Threat model <span class="arrow">↗</span></a>
          </div>
        </div>
      </div>
      <div class="reveal" style="--d:.1s">${faqList(FAQ)}</div>
    </div>`;
  wireFaq(view);
  reveals(view);
}

export async function renderAgents(ctx) {
  const { api, esc, ago, S } = ctx;
  const view = document.getElementById("view");
  const net = S && S.net === "main" ? "main" : "test";
  const lwd = net === "main" ? "https://zec.rocks:443" : "https://testnet.zec.rocks:443";
  const gw = (localStorage.getItem("tacit.gateway") || location.origin).replace(/\/$/, "");
  view.innerHTML = `
    <div class="page-head reveal"><span class="eyebrow">Agents</span><h1>The seats that never sleep</h1><p>Every vault has three key shares: the seller's, an operator's and an arbiter's. Moving money takes two. Pick an operator and an arbiter run by different people and no single party can touch the funds, and each agent signs only the payout it has recomputed from the published rules.</p></div>
    <div class="split">
      <div class="table-card reveal">
        <div class="table-tools"><b>Online now</b><span class="total-pill">Seen in the last 10 min</span></div>
        <div id="agent-list" style="padding:4px 18px"><div class="progress spin" style="margin:20px 0"><i></i></div></div>
      </div>
      <div class="panel reveal" style="--d:.1s">
        <h2>Become an arbiter</h2>
        <p class="muted">Two commands on any Linux machine or server. An agent needs no funds: it only holds key shares and cosigns what the rules allow, so running one makes every market it joins more trustworthy.</p>
        <pre class="cmd">curl -L -o tacit https://github.com/angelraph/tacit/releases/latest/download/tacit-linux-x86_64
chmod +x tacit
./tacit --network ${net} init --server ${esc(lwd)} \\
  --coordinator ${esc(gw)}
./tacit --network ${net} agent --role arbiter \\
  --name "My arbiter" --run-by "Your name"</pre>
        <p class="faint" style="font-size:13px;margin:12px 0 0">It appears in this list within a minute, and sellers can pick it. Arbiters rule on escrow disputes with <span class="mono">./tacit decide &lt;room&gt;</span>.</p>
      </div>
    </div>`;
  reveals(view);
  try {
    const agents = await api("/v1/agents");
    const now = Date.now() / 1000;
    view.querySelector("#agent-list").innerHTML = agents.length
      ? agents
          .map(
            (a) => `<div class="agent-row"><span class="avatar">${esc(a.name.trim()[0] || "A").toUpperCase()}</span><div style="flex:1;min-width:0"><b>${esc(a.name)}</b> <span class="faint" style="font-size:13px">run by ${esc(a.run_by || "unnamed")}</span><div class="faint mono" style="font-size:12px;overflow:hidden;text-overflow:ellipsis">${a.seat}</div></div><span class="badge ${a.role === "operator" ? "sealed" : "settled"}">${a.role}</span><span class="faint hide-sm" style="font-size:12.5px;min-width:70px;text-align:right">${ago(now - a.seen)} ago</span></div>`,
          )
          .join("")
      : `<p class="empty">No agents online right now.</p>`;
  } catch {
    view.querySelector("#agent-list").innerHTML = `<p class="empty">The coordinator is offline.</p>`;
  }
}

// Every auction this coordinator has seen, audited from public data alone.
export async function renderProof(ctx) {
  const { api, esc, zec, unit, short, ago, S, netName } = ctx;
  const view = document.getElementById("view");
  view.innerHTML = `
    <div class="page-head reveal"><span class="eyebrow">Proof</span><h1>Every market, <span class="dim">audited in public</span></h1><p>Each auction below is audited from its published terms, public chain data and the drand beacon, by the same code that runs when you press "Run audit in this tab". Don't take this page's word for it: open any auction and audit it yourself.</p></div>
    <div id="proof-body"><div class="progress spin" style="margin:30px 0"><i></i></div></div>`;
  reveals(view);
  let p;
  try {
    p = await api("/v1/proof");
  } catch (e) {
    view.querySelector("#proof-body").innerHTML = `<p class="empty">The coordinator didn't answer: ${esc(e.message)}</p>`;
    return;
  }
  const now = Date.now() / 1000;
  const badge = (v) =>
    ({
      honest: '<span class="badge settled">settled honestly</span>',
      no_bids: '<span class="badge settled">closed, no bids</span>',
      mismatch: '<span class="badge bad">mismatch</span>',
      awaiting_settlement: '<span class="badge open">unsealed</span>',
      open: '<span class="badge sealed">bidding / sealed</span>',
    })[v] || '<span class="badge">auditing</span>';
  view.querySelector("#proof-body").innerHTML = `
    <div class="big-stats reveal">
      <div><b>${p.auctions}</b><span>auctions on ${esc(netName(S.net))}</span></div>
      <div><b>${p.sealed_bids}</b><span>sealed bids</span></div>
      <div><b>${p.settled_honestly}</b><span>settled honestly</span></div>
      <div><b>${zec(p.bonded)}</b><span>${unit()} bonded</span></div>
      <div><b>${p.mismatches}</b><span>mismatches</span></div>
    </div>
    <div class="table-card reveal" style="margin-top:22px">
      <div class="table-tools"><b>Auction ledger</b><span class="total-pill">re-audited every 2 minutes</span></div>
      <div class="table-wrap"><table>
        <thead><tr><th>Lot</th><th>Verdict</th><th class="num">Bids</th><th class="num">Price</th><th class="hide-sm">Settlement</th><th class="hide-sm">Audited</th></tr></thead>
        <tbody>${
          p.rows.length
            ? p.rows
                .map(
                  (r) => `<tr><td><a href="#/auction/${r.id}">${esc(r.item || short(r.id, 6))}</a></td><td>${badge(r.verdict)}</td><td class="num">${r.deposits}</td><td class="num">${r.price != null ? `${zec(r.price)} ${unit()}` : "–"}</td><td class="hide-sm mono faint">${r.settlement ? short(r.settlement, 8) : "–"}</td><td class="hide-sm faint">${r.audited ? `${ago(now - r.audited)} ago` : "pending"}</td></tr>`,
                )
                .join("")
            : `<tr><td colspan="6" class="faint">No auctions yet.</td></tr>`
        }</tbody></table></div>
    </div>`;
  reveals(view);
}

export function renderTerms() {
  const view = document.getElementById("view");
  view.innerHTML = `
    <div class="page-head reveal"><span class="eyebrow">Terms and risks</span><h1>Read this before you use real money</h1><p>Tacit is open-source software you run in your own browser. Nobody operates it on your behalf, and nobody can reverse a transaction for you.</p></div>
    <div class="prose reveal">
      <h2>No custody</h2>
      <p>Tacit never holds your funds or keys. Your wallet lives in your browser, encrypted with your passcode. Auction bonds and escrow payments sit in 2-of-3 vaults whose key shares belong to the seats you chose. Anyone running a coordinator or an agent is a separate party from the people who wrote this software.</p>
      <h2>Risks you accept</h2>
      <ul>
        <li><b>Software risk.</b> This is new code built for the Zecathon. Bugs can lose funds. Start on testnet, then with small amounts.</li>
        <li><b>Key loss.</b> If you lose your recovery phrase and your browser data, nobody can recover your wallet.</li>
        <li><b>Collusion.</b> Any two seats of a vault can move its funds. Pick agents you trust not to collude with the other side.</li>
        <li><b>Liveness.</b> If drand stops, bids stay sealed until it resumes. If agents go offline, settlement waits for them.</li>
        <li><b>Metadata.</b> Timing and your IP address are not hidden by Tacit. Use Tor if that matters to you.</li>
        <li><b>Cross-chain swaps.</b> Funding through NEAR Intents 1Click relies on third-party solvers and is public on the origin chain.</li>
      </ul>
      <h2>Your responsibility</h2>
      <p>You are responsible for making sure your use of Tacit is lawful where you live, and for the things you buy and sell. Tacit is provided as is, without warranty of any kind, under the MIT or Apache 2.0 licence.</p>
    </div>`;
  reveals(view);
}

const GATE_KEY = "tacit.accepted.v1";

/** Shows the one-time risk notice before the app is first used. */
export function gate(ctx) {
  try {
    if (localStorage.getItem(GATE_KEY)) return Promise.resolve();
  } catch {
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    const m = ctx.modal(`
      <div class="gate-head"><img src="./logo.svg" alt="" /></div>
      <div class="gate-body">
        <h2 style="font-size:21px">Before you continue</h2>
        <p class="muted" style="font-size:14px">Tacit is open-source software that runs entirely in your browser. Nobody holds your funds or your keys, and nobody can undo a transaction. It is new code: start on testnet, then with amounts you can afford to lose.</p>
        <p class="muted" style="font-size:14px">Your recovery phrase is the only way back into your wallet.</p>
        <label class="row" style="gap:10px;flex-wrap:nowrap;align-items:flex-start;padding:14px;border-radius:12px;background:rgba(255,255,255,.04);border:1px solid var(--line);font-size:13.5px;color:var(--muted)">
          <input type="checkbox" id="gate-ok" style="margin-top:2px" />
          <span>I've read the <a href="#/terms" data-close>terms and risks</a>, and I'm responsible for making sure my use of Tacit is lawful where I am.</span>
        </label>
        <button class="btn primary wide" id="gate-go" disabled style="margin-top:16px">Continue</button>
      </div>`);
    m.querySelector(".modal").classList.add("gate");
    const ok = m.querySelector("#gate-ok");
    const go = m.querySelector("#gate-go");
    let accepted = false;
    // Dismissing without agreeing leaves the app for the home page.
    m.close = () => {
      m.remove();
      if (!accepted && !location.hash.startsWith("#/terms")) location.hash = "#/";
      resolve();
    };
    ok.onchange = () => (go.disabled = !ok.checked);
    go.onclick = () => {
      accepted = true;
      try {
        localStorage.setItem(GATE_KEY, String(Date.now()));
      } catch {}
      m.close();
    };
  });
}

export { glowCards };
