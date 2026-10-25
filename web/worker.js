// Everything that touches keys or proofs runs here, off the page's main
// thread: the WebAssembly wallet, the Ironwood prover and auction audits.
// The page talks to it with {id, method, args} messages.

let tacit = null;
let wallet = null;
// A proved bid waiting for the bidder's go-ahead (prover worker only).
let pendingBid = null;
let phrase = null;
let gateway = "";
let proverReady = false;

// Small enough that progress moves on a busy chain, large enough to keep
// requests few on a quiet one.
const BATCH = 100;

async function api(path, init) {
  const res = await fetch(gateway + path, init);
  const text = await res.text();
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    body = { error: text };
  }
  if (!res.ok) throw new Error(body.error || `${res.status} from the chain gateway`);
  return body;
}

const getText = async (path) => JSON.stringify(await api(path));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function sendAll(room, from, out) {
  for (const m of out) {
    await api(`/v1/rooms/${room}/messages`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ from, to: m.to, round: m.round, body: m.body }),
    });
  }
}

function progress(id, data) {
  self.postMessage({ id, progress: data });
}

function walletState() {
  return {
    network: wallet.network(),
    address: wallet.address(0),
    height: wallet.height(),
    balance: Number(wallet.balance()),
    spendable: Number(wallet.spendable()),
    notes: JSON.parse(wallet.notes()),
  };
}

// Scans from a scanner's height to the chain tip in batches. The scanner
// skips the tree's hashing unless a block pays it; then it stops and asks
// for the tree state there, and the scan picks up from that block.
//
// Downloads run ahead of the scan (up to AHEAD requests in flight), and the
// batch size follows how heavy recent blocks were: a quiet chain goes 500
// blocks a request, a flooded one 50.
const AHEAD = 3;
async function catchUp(id, scanner, label) {
  const info = await api("/v1/chain/info");
  const start = scanner.height();
  let h = start;
  let size = BATCH;
  let queued = [];
  let fetchedTo = h;
  const fill = () => {
    while (queued.length < AHEAD && fetchedTo < info.tip) {
      const from = fetchedTo + 1;
      const to = Math.min(info.tip, fetchedTo + size);
      queued.push({ from, to, text: getText(`/v1/chain/blocks?from=${from}&to=${to}`) });
      fetchedTo = to;
    }
  };
  fill();
  while (h < info.tip) {
    const batch = queued.shift();
    const blocks = await batch.text;
    const perBlock = blocks.length / (batch.to - batch.from + 1);
    size = perBlock < 4000 ? 500 : perBlock < 40000 ? BATCH : 50;
    fill();
    try {
      scanner.scan(blocks);
    } catch (e) {
      const need = /need the tree state at block (\d+)/.exec(e && e.message);
      if (!need) throw e;
      scanner.setTree(await getText(`/v1/chain/tree/${need[1]}`));
      // The scan stopped inside this batch: drop what was fetched ahead and
      // carry on from the block that pays us.
      queued.forEach((q) => q.text.catch(() => {}));
      queued = [];
      fetchedTo = scanner.height();
      fill();
    }
    h = scanner.height();
    progress(id, { label, done: h - start, total: info.tip - start, height: h, tip: info.tip });
  }
  return info;
}

const methods = {
  async init({ threads, gatewayUrl }) {
    gateway = gatewayUrl.replace(/\/$/, "");
    if (!tacit) {
      const pkg = threads ? "./pkg-threads/tacit_wasm.js" : "./pkg/tacit_wasm.js";
      tacit = await import(pkg);
      await tacit.default();
      if (threads) await tacit.initThreadPool(navigator.hardwareConcurrency || 4);
    }
    return { ok: true };
  },

  setGateway({ gatewayUrl }) {
    gateway = gatewayUrl.replace(/\/$/, "");
    return { ok: true };
  },

  // Building the proving key takes a while, so do it before anyone asks.
  warm() {
    if (!proverReady) {
      const t = performance.now();
      tacit.warmProver();
      proverReady = true;
      return { ms: performance.now() - t };
    }
    return { ms: 0 };
  },

  newPhrase({ words } = {}) {
    return tacit.newPhrase(words || 24);
  },

  sealBidMemo({ terms, amount, refundAddress, text }) {
    return tacit.sealBidMemo(JSON.stringify(terms), BigInt(amount), refundAddress, !!text);
  },

  checkPhrase({ phrase }) {
    return tacit.checkPhrase(phrase);
  },

  async createWallet({ network, phrase: p, birthday }) {
    let height = birthday;
    if (height == null) height = (await api("/v1/chain/info")).tip;
    const tree = await getText(`/v1/chain/tree/${height}`);
    wallet = new tacit.Wallet(network, p, tree);
    phrase = p;
    return { saved: wallet.save(), state: walletState() };
  },

  // A long catch-up (restoring an older wallet) on the threaded build, which
  // trial-decrypts on every core. Returns the state for the wallet worker.
  async syncSaved({ phrase: p, saved }, id) {
    wallet = tacit.Wallet.load(p, saved);
    phrase = p;
    await catchUp(id, wallet, "Syncing wallet");
    return { saved: wallet.save(), state: walletState() };
  },

  openWallet({ phrase: p, saved }) {
    wallet = tacit.Wallet.load(p, saved);
    phrase = p;
    return walletState();
  },

  closeWallet() {
    wallet = null;
    phrase = null;
    return true;
  },

  async sync(_, id) {
    if (!wallet) throw new Error("no wallet open");
    await catchUp(id, wallet, "Syncing wallet");
    return { saved: wallet.save(), state: walletState() };
  },

  state() {
    return wallet ? walletState() : null;
  },

  address({ index }) {
    return wallet.address(index);
  },

  async bid({ terms, amount, refundIndex, phrase: p, saved }, id) {
    if (saved) wallet = tacit.Wallet.load(p, saved);
    if (!wallet) throw new Error("no wallet open");
    await catchUp(id, wallet, "Syncing wallet");
    progress(id, { label: "Sealing your bid and proving the deposit", spin: true });
    const t = performance.now();
    const built = wallet.bid(JSON.stringify(terms), BigInt(amount), refundIndex);
    const proveMs = performance.now() - t;
    progress(id, { label: "Broadcasting", spin: true });
    const tx = JSON.parse(built);
    await api("/v1/chain/tx", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ data: tx.data }),
    });
    wallet.sent(built);
    return { txid: tx.txid, fee: tx.fee, proveMs, saved: wallet.save(), state: walletState() };
  },

  // The same bid in two steps, so proving can run while the bidder reads the
  // confirmation: prepareBid proves and holds the transaction, broadcastBid
  // sends it. Nothing leaves the tab until broadcastBid.
  async prepareBid({ terms, amount, refundIndex, phrase: p, saved }, id) {
    if (saved) wallet = tacit.Wallet.load(p, saved);
    if (!wallet) throw new Error("no wallet open");
    pendingBid = null;
    await catchUp(id, wallet, "Syncing wallet");
    progress(id, { label: "Sealing your bid and proving the deposit", spin: true });
    const t = performance.now();
    const built = wallet.bid(JSON.stringify(terms), BigInt(amount), refundIndex);
    pendingBid = { built, proveMs: performance.now() - t };
    return { proveMs: pendingBid.proveMs };
  },

  async broadcastBid(_, id) {
    if (!pendingBid) throw new Error("nothing to send; place the bid again");
    const { built, proveMs } = pendingBid;
    pendingBid = null;
    progress(id, { label: "Broadcasting", spin: true });
    const tx = JSON.parse(built);
    await api("/v1/chain/tx", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ data: tx.data }),
    });
    wallet.sent(built);
    return { txid: tx.txid, fee: tx.fee, proveMs, saved: wallet.save(), state: walletState() };
  },

  dropBid() {
    pendingBid = null;
    return true;
  },

  async send({ to, amount, memo, phrase: p, saved }, id) {
    if (saved) wallet = tacit.Wallet.load(p, saved);
    if (!wallet) throw new Error("no wallet open");
    await catchUp(id, wallet, "Syncing wallet");
    progress(id, { label: "Proving the payment", spin: true });
    const t = performance.now();
    const built = wallet.send(to, BigInt(amount), memo || "");
    const proveMs = performance.now() - t;
    const tx = JSON.parse(built);
    progress(id, { label: "Broadcasting", spin: true });
    await api("/v1/chain/tx", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ data: tx.data }),
    });
    wallet.sent(built);
    return { txid: tx.txid, fee: tx.fee, proveMs, saved: wallet.save(), state: walletState() };
  },

  clearPending() {
    wallet.clearPending();
    return { saved: wallet.save(), state: walletState() };
  },

  // A full audit from the public terms: scan the vault, read every deposit's
  // memo, and, once the beacon exists, recompute and check the settlement.
  async audit({ terms, beacon }, id) {
    const tree = await getText(`/v1/chain/tree/${terms.created_height}`);
    const audit = new tacit.Audit(JSON.stringify(terms), tree);
    try {
      await catchUp(id, audit, "Scanning the vault");
      const wanted = JSON.parse(audit.wanted());
      let n = 0;
      for (const txid of wanted) {
        progress(id, { label: "Reading deposits", done: n++, total: wanted.length });
        audit.addTx(txid, await getText(`/v1/chain/tx/${txid}`));
      }
      const report = JSON.parse(audit.report(beacon ? beacon.signature : ""));
      return report;
    } finally {
      audit.free();
    }
  },

  // ---------- vault seats ----------

  newSeatKey() {
    return tacit.newSeatKey();
  },

  seatPublic({ secret }) {
    return tacit.seatPublic(secret);
  },

  // Runs this seat's side of key generation in `room` to completion.
  async dkg({ network, secret, room, seats }, id) {
    const me = tacit.seatPublic(secret);
    const session = new tacit.Dkg(network, secret, room, JSON.stringify(seats));
    try {
      await sendAll(room, me, JSON.parse(session.start()));
      const inbox = [];
      let after = 0;
      const deadline = Date.now() + 15 * 60 * 1000;
      const labels = { 1: "Exchanging key commitments", 2: "Exchanging key shares", 3: "Agreeing on the viewing key", 4: "Confirming the vault" };
      for (;;) {
        const msgs = await api(`/v1/rooms/${room}/messages?to=${me}&after=${after}`);
        for (const m of msgs) {
          after = Math.max(after, m.seq);
          inbox.push(m);
        }
        const r = JSON.parse(session.step(JSON.stringify(inbox)));
        await sendAll(room, me, r.out);
        const round = Math.max(1, ...inbox.map((m) => m.round));
        progress(id, { label: labels[round] || "Generating the vault key", spin: true });
        if (r.done) return r.done;
        if (Date.now() > deadline) throw new Error("The other seats didn't show up in time");
        await sleep(700);
      }
    } finally {
      session.free();
    }
  },

  // Balance and state of a vault this browser holds a seat in.
  async vaultStatus({ network, share, createdHeight }, id) {
    const tree = await getText(`/v1/chain/tree/${createdHeight}`);
    const v = new tacit.VaultSeat(network, JSON.stringify(share), tree);
    try {
      await catchUp(id, v, "Scanning the vault");
      return { balance: Number(v.balance()), deposits: v.deposits(), spent: v.spent() || null, height: v.height() };
    } finally {
      v.free();
    }
  },

  // Proposes paying out the whole vault to `to`, coordinates the 2 of 3
  // signature with `cosigner`, and broadcasts the result.
  async vaultPayout({ network, share, createdHeight, secret, to, cosigner, purpose }, id) {
    const tree = await getText(`/v1/chain/tree/${createdHeight}`);
    const v = new tacit.VaultSeat(network, JSON.stringify(share), tree);
    const me = tacit.seatPublic(secret);
    try {
      await catchUp(id, v, "Scanning the vault");
      const room = await api("/v1/rooms", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ seats: share.seats, purpose }),
      });
      progress(id, { label: "Proving the payout", spin: true });
      const amount = Number(v.propose(secret, room.id, to, cosigner));
      await sendAll(room.id, me, JSON.parse(v.start()));
      progress(id, { label: "Waiting for the cosigner", spin: true });
      const inbox = [];
      let after = 0;
      const deadline = Date.now() + 10 * 60 * 1000;
      for (;;) {
        const msgs = await api(`/v1/rooms/${room.id}/messages?to=${me}&after=${after}`);
        for (const m of msgs) {
          after = Math.max(after, m.seq);
          inbox.push(m);
        }
        const r = JSON.parse(v.step(JSON.stringify(inbox)));
        await sendAll(room.id, me, r.out);
        if (r.done) {
          progress(id, { label: "Broadcasting", spin: true });
          await api("/v1/chain/tx", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({ data: r.done.data }),
          });
          return { txid: r.done.txid, amount };
        }
        if (Date.now() > deadline) throw new Error("The cosigner didn't sign. If this is a dispute, the arbiter decides by hand.");
        await sleep(800);
      }
    } finally {
      v.free();
    }
  },

  auctionId({ terms }) {
    return tacit.auctionId(JSON.stringify(terms));
  },

  escrowId({ terms }) {
    return tacit.escrowId(JSON.stringify(terms));
  },

  vaultAddress({ network, ufvk }) {
    return tacit.vaultAddress(network, ufvk);
  },

  verifyBeacon({ round, signature }) {
    return tacit.verifyBeacon(BigInt(round), signature);
  },

  roundTime({ round }) {
    return Number(tacit.roundTime(BigInt(round)));
  },

  roundAt({ unix }) {
    return Number(tacit.roundAt(BigInt(unix)));
  },
};

self.onmessage = async ({ data }) => {
  const { id, method, args } = data;
  try {
    const fn = methods[method];
    if (!fn) throw new Error(`unknown method ${method}`);
    const result = await fn(args || {}, id);
    self.postMessage({ id, result });
  } catch (e) {
    self.postMessage({ id, error: e && e.message ? e.message : String(e) });
  }
};
