// Shielded escrow: buyer, seller and an arbiter hold a 2 of 3 vault. The
// buyer pays in; the vault pays out once, either to the seller or back to
// the buyer. Either side can give the money to the other with the arbiter's
// automatic cosignature; only a dispute needs the arbiter's own judgement.

const enc = (o) => btoa(unescape(encodeURIComponent(JSON.stringify(o)))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const dec = (s) => JSON.parse(decodeURIComponent(escape(atob(s.replace(/-/g, "+").replace(/_/g, "/")))));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function renderEscrow(m, ctx) {
  const { S } = ctx;
  const view = document.getElementById("view");
  await S.ready;
  await ctx.refreshChain();
  if (!S.info) {
    view.innerHTML = `<div class="panel"><h2>Can't reach the coordinator</h2></div>`;
    return;
  }
  const sub = m[1] || "";
  if (!S.wallet || !S.pass) {
    view.innerHTML = `
      <div class="panel" style="max-width:560px;margin:30px auto">
        <span class="eyebrow">Escrow</span>
        <h2>Open your wallet first</h2>
        <p class="muted">Your wallet funds the escrow and holds your seat in its vault.</p>
        <a class="btn primary" href="#/wallet">Open wallet</a>
      </div>`;
    if (sub) ctx.S.after = location.hash;
    return;
  }
  if (sub.startsWith("join/")) return renderJoin(sub.slice(5), ctx);
  if (/^[0-9a-f]{64}$/.test(sub)) return renderDeal(sub, ctx);
  return renderHome(ctx);
}

async function renderHome(ctx) {
  const { S, api, call, busy, toast, esc, zec, parseZec, unit, loadSeats, saveSeats, copy } = ctx;
  const view = document.getElementById("view");
  const seats = await loadSeats();
  const deals = seats.vaults.filter((v) => v.kind === "escrow");
  const agents = await api("/v1/agents").catch(() => []);
  const arbiters = agents.filter((a) => a.role === "arbiter");

  view.innerHTML = `
    <span class="eyebrow">Escrow</span>
    <h1 style="font-size:clamp(28px,4vw,42px)">Pay for something without trusting a stranger</h1>
    <p class="muted" style="max-width:720px">The money sits in a shielded 2-of-3 vault held by you, the other side and an arbiter. When the deal goes through, the buyer releases it to the seller. If it falls through, the seller refunds the buyer. The arbiter only steps in when you can't agree. Nobody on chain can see who is trading with whom.</p>
    <div class="split" style="margin-top:22px">
      <div class="panel">
        <h2>Start an escrow</h2>
        <div class="stack">
          <div class="tabs" id="role"><button class="on" data-r="buyer">I'm buying</button><button data-r="seller">I'm selling</button></div>
          <label class="field"><span>What is it for?</span><input id="item" maxlength="256" placeholder="e.g. Logo design, 3 concepts" /></label>
          <label class="field"><span>Price</span><div class="input-suffix"><input id="amount" inputmode="decimal" placeholder="0.5" /><b>${unit()}</b></div></label>
          <label class="field"><span>Arbiter</span><select id="arb">${arbiters.map((a) => `<option value="${a.seat}">${esc(a.name)} · ${a.seat.slice(0, 8)}</option>`).join("") || `<option value="">none online</option>`}</select></label>
          <button class="btn primary" id="go">Create invite link</button>
        </div>
        <div id="invite"></div>
      </div>
      <div class="panel">
        <h2>Your escrows</h2>
        ${deals.length ? `<table><tbody>${deals.map((d) => `<tr><td><a href="#/escrow/${d.ref}">${esc(d.item)}</a><div class="faint" style="font-size:12px">you are the ${d.role}</div></td><td class="num">${zec(d.amount)} ${unit()}</td></tr>`).join("")}</tbody></table>` : `<p class="muted">None yet.</p>`}
      </div>
    </div>`;

  let role = "buyer";
  view.querySelectorAll("#role button").forEach((b) => {
    b.onclick = () => {
      role = b.dataset.r;
      view.querySelectorAll("#role button").forEach((x) => x.classList.toggle("on", x === b));
    };
  });

  view.querySelector("#go").onclick = async () => {
    const item = view.querySelector("#item").value.trim();
    const amount = parseZec(view.querySelector("#amount").value);
    const arbiter = view.querySelector("#arb").value;
    if (!item || !amount) return toast("Describe the deal and set a price", "error");
    if (!arbiter) return toast("An arbiter needs to be online", "error");
    const me = await call("seatPublic", { secret: seats.secret });
    const addr = await call("address", { index: ctx.nextRefundIndex() });
    const invite = { v: 1, net: S.net, role, item, amount, seat: me, addr, arbiter };
    const link = `${location.origin}${location.pathname}#/escrow/join/${enc(invite)}`;
    const since = Math.floor(Date.now() / 1000) - 5;
    const box = view.querySelector("#invite");
    box.innerHTML = `
      <div class="notice good" style="margin-top:16px">Send this link to the ${role === "buyer" ? "seller" : "buyer"}. Keep this tab open: the vault is created the moment they accept.</div>
      <div class="addr" style="margin-top:10px">${esc(link)}</div>
      <div class="row" style="margin-top:10px"><button class="btn small" id="cp">Copy link</button><span class="faint" id="wait"><span class="spinner" style="display:inline-block;vertical-align:middle"></span> waiting for them…</span></div>`;
    box.querySelector("#cp").onclick = () => copy(link);
    view.querySelector("#go").disabled = true;

    // Wait for the other side to open the key generation room.
    let room = null;
    for (let i = 0; i < 1800 && !room; i++) {
      const rooms = await api(`/v1/rooms?seat=${me}&since=${since}`).catch(() => []);
      room = rooms.find((r) => r.purpose === "dkg" && r.seats.includes(arbiter));
      if (!room) await sleep(2000);
      if (!document.body.contains(box)) return;
    }
    if (!room) return toast("Nobody accepted the invite", "error");
    const b = busy("Creating the escrow vault");
    try {
      const created = (await api("/v1/chain/info")).tip;
      const share = await call("dkg", { network: S.net, secret: seats.secret, room: room.id, seats: room.seats }, b.onProgress);
      b.onProgress({ label: "Waiting for the escrow terms", spin: true });
      let rec = null;
      for (let i = 0; i < 90 && !rec; i++) {
        const list = await api("/v1/escrows");
        rec = list.find((e) => e.terms.vault_ufvk === share.ufvk);
        if (!rec) await sleep(2000);
      }
      if (!rec) throw new Error("The other side never published the escrow terms");
      const t = rec.terms;
      const mine = role === "buyer" ? t.buyer : t.seller;
      if (t.item !== item || t.amount !== amount || mine !== addr || (await call("escrowId", { terms: t })) !== rec.id) {
        throw new Error("The published terms don't match your invite. Don't fund this escrow.");
      }
      seats.vaults.push({ kind: "escrow", ref: rec.id, item, amount, role, share, created_height: t.created_height || created });
      await saveSeats(seats);
      b.close();
      location.hash = `#/escrow/${rec.id}`;
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };
  void busy;
}

async function renderJoin(code, ctx) {
  const { S, api, call, busy, toast, esc, zec, unit, loadSeats, saveSeats } = ctx;
  const view = document.getElementById("view");
  let inv;
  try {
    inv = dec(code);
  } catch {
    view.innerHTML = `<div class="panel"><h2>That invite link is broken</h2></div>`;
    return;
  }
  if (inv.net !== S.net) {
    view.innerHTML = `<div class="panel"><h2>Wrong network</h2><p class="muted">This escrow is on ${esc(inv.net)}, your wallet is on ${esc(S.net)}.</p></div>`;
    return;
  }
  const myRole = inv.role === "buyer" ? "seller" : "buyer";
  view.innerHTML = `
    <div class="panel" style="max-width:560px;margin:20px auto">
      <span class="eyebrow">Escrow invite</span>
      <h2>${esc(inv.item)}</h2>
      <dl class="kv" style="margin:14px 0">
        <dt>Price</dt><dd><b>${zec(inv.amount)} ${unit()}</b></dd>
        <dt>You are</dt><dd>the ${myRole}</dd>
        <dt>Arbiter</dt><dd class="mono">${inv.arbiter.slice(0, 16)}…</dd>
      </dl>
      <p class="muted">${myRole === "buyer" ? `You'll pay ${zec(inv.amount)} ${unit()} into a shared vault. It goes to the seller only when you release it, or when the arbiter rules for them in a dispute.` : `The buyer pays into a shared vault. You get the money when they release it, or when the arbiter rules for you in a dispute.`}</p>
      <button class="btn primary wide" id="ok">Accept and create the vault</button>
    </div>`;
  view.querySelector("#ok").onclick = async () => {
    const b = busy("Creating the escrow vault");
    try {
      const seats = await loadSeats();
      const me = await call("seatPublic", { secret: seats.secret });
      const myAddr = await call("address", { index: ctx.nextRefundIndex() });
      const order = inv.role === "buyer" ? [inv.seat, me, inv.arbiter] : [me, inv.seat, inv.arbiter];
      const created = (await api("/v1/chain/info")).tip;
      const room = await api("/v1/rooms", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ seats: order, purpose: "dkg" }),
      });
      const share = await call("dkg", { network: S.net, secret: seats.secret, room: room.id, seats: order }, b.onProgress);
      const terms = {
        network: S.net,
        vault_ufvk: share.ufvk,
        seats: order,
        buyer: inv.role === "buyer" ? inv.addr : myAddr,
        seller: inv.role === "seller" ? inv.addr : myAddr,
        item: inv.item,
        amount: inv.amount,
        created_height: created,
      };
      const id = await call("escrowId", { terms });
      const rec = await api("/v1/escrows", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(terms),
      });
      if (rec.id !== id) throw new Error("The coordinator computed a different escrow id");
      seats.vaults.push({ kind: "escrow", ref: id, item: inv.item, amount: inv.amount, role: myRole, share, created_height: created });
      await saveSeats(seats);
      b.close();
      toast("Escrow created", "good");
      location.hash = `#/escrow/${id}`;
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };
}

async function renderDeal(id, ctx) {
  const { S, api, call, busy, toast, esc, zec, unit, loadSeats, modal, short } = ctx;
  const view = document.getElementById("view");
  const seats = await loadSeats();
  const deal = seats.vaults.find((v) => v.kind === "escrow" && v.ref === id);
  const rec = await api(`/v1/escrows/${id}`);
  const t = rec.terms;
  if ((await call("escrowId", { terms: t })) !== id) throw new Error("These escrow terms don't hash to this id");
  view.innerHTML = `
    <a href="#/escrow" class="faint">← Escrow</a>
    <h1 style="font-size:clamp(26px,3.6vw,40px);margin-top:10px">${esc(t.item)}</h1>
    <div class="split">
      <div>
        <div class="panel" id="state"><div class="progress spin"><i></i></div></div>
      </div>
      <div class="panel">
        <h2>Terms</h2>
        <dl class="kv">
          <dt>Price</dt><dd>${zec(t.amount)} ${unit()}</dd>
          <dt>Your role</dt><dd>${deal ? deal.role : "observer"}</dd>
          <dt>Created at block</dt><dd>${t.created_height.toLocaleString()}</dd>
        </dl>
        <p class="faint" style="margin:14px 0 6px;font-size:13px">Vault address</p>
        <div class="addr">${esc(deal ? deal.share.address : await call("vaultAddress", { network: t.network, ufvk: t.vault_ufvk }))}</div>
        <p class="faint" style="margin:14px 0 6px;font-size:13px">Escrow id</p>
        <div class="addr">${id}</div>
      </div>
    </div>`;
  if (!deal) {
    view.querySelector("#state").innerHTML = `<p class="muted">You don't hold a seat in this escrow.</p>`;
    return;
  }
  const st = await call("vaultStatus", { network: S.net, share: deal.share, createdHeight: deal.created_height });
  const funded = st.balance >= t.amount - 1;
  const panel = view.querySelector("#state");
  const myRole = deal.role;
  const seatNo = myRole === "buyer" ? 1 : 2;
  let html;
  if (st.spent) {
    html = `<span class="badge settled">Closed</span><h2 style="margin-top:10px">The vault has paid out</h2><p class="muted">Transaction</p><div class="addr">${esc(st.spent)}</div>`;
  } else if (!funded) {
    html = `<span class="badge bidding">Waiting for payment</span>
      <h2 style="margin-top:10px">${zec(st.balance)} of ${zec(t.amount)} ${unit()} paid in</h2>
      ${myRole === "buyer" ? `<p class="muted">Pay the price into the vault from your Tacit wallet. Nobody on chain can tell it's an escrow payment.</p><button class="btn primary" id="fund">Pay ${zec(t.amount)} ${unit()} into escrow</button>` : `<p class="muted">The buyer hasn't paid in yet. This page updates when they do.</p>`}`;
  } else {
    html = `<span class="badge settled">Funded</span>
      <h2 style="margin-top:10px">${zec(st.balance)} ${unit()} held in escrow</h2>
      ${myRole === "buyer"
        ? `<p class="muted">Once you have what you paid for, release the money to the seller. The arbiter cosigns automatically, since only you can give up your own claim.</p><div class="row"><button class="btn primary" id="release">Release to seller</button><button class="btn ghost" id="dispute">Open a dispute</button></div>`
        : `<p class="muted">If the deal is off, refund the buyer. The arbiter cosigns automatically. Otherwise, wait for the buyer to release the money.</p><div class="row"><button class="btn" id="refund">Refund the buyer</button><button class="btn ghost" id="dispute">Open a dispute</button></div>`}`;
  }
  panel.innerHTML = html;

  const pay = async (payee, label) => {
    const b = busy(label);
    try {
      b.onProgress({ label: "Starting the prover", spin: true });
      await ctx.warmProver();
      const r = await ctx.prove(
        "vaultPayout",
        {
          network: S.net,
          share: deal.share,
          createdHeight: deal.created_height,
          secret: seats.secret,
          to: payee === "seller" ? t.seller : t.buyer,
          cosigner: 3,
          purpose: `escrow:${id}:${payee}:${seatNo}`,
        },
        b.onProgress,
      );
      b.close();
      toast(`Paid ${zec(r.amount)} ${unit()} to the ${payee}`, "good");
      ctx.route();
    } catch (e) {
      b.close();
      toast(e.message, "error");
    }
  };
  const q = (s) => panel.querySelector(s);
  if (q("#fund"))
    q("#fund").onclick = async () => {
      const b = busy("Paying into escrow");
      try {
        const r = await ctx.proveWith("send", { to: deal.share.address, amount: t.amount, memo: "" }, b.onProgress);
        void r;
        b.close();
        toast("Paid in. It shows up after the next block.", "good");
      } catch (e) {
        b.close();
        toast(e.message, "error");
      }
    };
  if (q("#release")) q("#release").onclick = () => pay("seller", "Releasing to the seller");
  if (q("#refund")) q("#refund").onclick = () => pay("buyer", "Refunding the buyer");
  if (q("#dispute"))
    q("#dispute").onclick = () => {
      const m = modal(`
        <h2>Open a dispute</h2>
        <p class="muted">The arbiter never signs a payment to yourself automatically. A dispute asks the arbiter to look at the case and decide by hand. Tell them what happened through your usual channel, then submit the request.</p>
        <div class="row" style="margin-top:14px"><span class="spacer"></span><button class="btn ghost" data-close>Cancel</button><button class="btn primary" id="go">Ask for ${myRole === "buyer" ? "a refund" : "payment"}</button></div>`);
      m.querySelector("#go").onclick = () => {
        m.close();
        pay(myRole === "buyer" ? "buyer" : "seller", "Waiting for the arbiter's decision");
      };
    };
  void short;
}
