// Records the demo's scenes from the real app, one video per scene.
//
//   node record.mjs intro problem landing    # these scenes, in order
//   node record.mjs all                      # every scene
//
// Each scene runs in a fresh page of one persistent Chrome profile (so the
// wallet made in "wallet" is there later), with overlay.js drawing the cursor,
// ripples and titles. A scene lasts as long as its narration (out/timing.json);
// stretches marked with cut() are removed when the video is assembled.

import { chromium } from "playwright";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const OUT = path.join(HERE, "out");
const RAW = path.join(OUT, "raw");
const BASE = process.env.TACIT_URL || "https://tacit-production-6a96.up.railway.app";
const MAINNET = process.env.TACIT_MAINNET || "http://localhost:8770/?gateway=http://127.0.0.1:8651";
const PASS = "demo-passcode-7";
const timing = JSON.parse(fs.readFileSync(path.join(OUT, "timing.json"), "utf8"));
const state = fs.existsSync(path.join(OUT, "state.json")) ? JSON.parse(fs.readFileSync(path.join(OUT, "state.json"), "utf8")) : {};
const saveState = () => fs.writeFileSync(path.join(OUT, "state.json"), JSON.stringify(state, null, 2));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const page_ = (p) => pathToFile(path.join(HERE, "pages", p));
function pathToFile(p) {
  return "file:///" + p.replace(/\\/g, "/");
}

fs.mkdirSync(RAW, { recursive: true });

const ctx = await chromium.launchPersistentContext(path.join(OUT, "profile"), {
  headless: true,
  viewport: { width: 1920, height: 1080 },
  deviceScaleFactor: 1,
  recordVideo: { dir: path.join(OUT, "tmp"), size: { width: 1920, height: 1080 } },
  args: ["--autoplay-policy=no-user-gesture-required", "--hide-scrollbars"],
});
await ctx.addInitScript({ path: path.join(HERE, "overlay.js") });
// The one-time risk notice belongs to onboarding, not to every scene.
await ctx.addInitScript(() => {
  try {
    localStorage.setItem("tacit.accepted.v1", String(Date.now()));
  } catch {}
});

// ---------- helpers ----------

function scene(id) {
  let page, t0, cutAt = null, cutTotal = 0;
  const cuts = [];
  const s = {
    async open(url) {
      page = await ctx.newPage();
      t0 = Date.now();
      // Loading is cut from the video: each scene starts on a ready page.
      s.cut();
      await page.goto(url, { waitUntil: "domcontentloaded" });
      if (url.startsWith("http")) {
        await page.waitForFunction(
          () => !/connecting/.test(document.getElementById("chain-label")?.textContent || "connecting") && document.querySelector("#view")?.children.length,
          null,
          { timeout: 120000 },
        );
        await page.waitForTimeout(1500);
      } else {
        await page.waitForTimeout(300);
      }
      s.uncut();
      return page;
    },
    get page() {
      return page;
    },
    t: () => (Date.now() - t0) / 1000,
    async center(sel) {
      const loc = page.locator(sel).first();
      await loc.waitFor({ state: "visible", timeout: 60000 });
      await loc.scrollIntoViewIfNeeded();
      const b = await loc.boundingBox();
      return { x: b.x + b.width / 2, y: b.y + b.height / 2, box: b, loc };
    },
    async point(sel, ms = 800) {
      const c = await s.center(sel);
      await page.evaluate(([x, y, ms]) => window.__demo.move(x, y, ms), [c.x, c.y, ms]);
      return c;
    },
    async click(sel, opts = {}) {
      const c = await s.point(sel, opts.ms || 800);
      await page.evaluate(() => window.__demo.ripple());
      await page.waitForTimeout(120);
      await c.loc.click(opts);
      await page.waitForTimeout(opts.after ?? 500);
    },
    async type(sel, text, delay = 45) {
      await s.click(sel, { after: 150 });
      await c_clear(sel);
      await page.locator(sel).first().pressSequentially(text, { delay });
      await page.waitForTimeout(300);
    },
    async ring(sel, ms = 1800) {
      const c = await s.center(sel);
      await page.evaluate(([b, ms]) => window.__demo.ring(b, ms), [c.box, ms]);
    },
    async title(t, sub) {
      await page.evaluate(([t, sub]) => window.__demo.title(t, sub), [t, sub]);
    },
    async untitle() {
      await page.evaluate(() => window.__demo.untitle());
    },
    async scroll(y, ms = 1400) {
      await page.evaluate((y) => window.scrollTo({ top: y, behavior: "smooth" }), y);
      await page.waitForTimeout(ms);
    },
    cut() {
      cutAt = s.t();
    },
    uncut() {
      if (cutAt == null) return;
      const end = s.t();
      cuts.push([cutAt, end]);
      cutTotal += end - cutAt;
      cutAt = null;
    },
    // Hold until the narration is done (time inside cuts doesn't count).
    async finish(extra = 0.8) {
      // Always let the last action land on camera.
      await sleep(2500);
      const need = (timing[id] || 8) + extra;
      while (s.t() - cutTotal < need) await sleep(100);
      const video = page.video();
      await page.close();
      const src = await video.path();
      fs.copyFileSync(src, path.join(RAW, `${id}.webm`));
      fs.writeFileSync(path.join(RAW, `${id}.json`), JSON.stringify({ length: s.t(), cuts }, null, 2));
      console.log(`${id}: ${s.t().toFixed(1)} s recorded, ${cutTotal.toFixed(1)} s cut`);
    },
  };
  async function c_clear(sel) {
    await page.locator(sel).first().fill("");
  }
  return s;
}

// Unlocks the demo wallet off camera (inside a cut).
async function unlock(s) {
  // Wait for the wallet page proper: the passcode box, or an open wallet.
  await s.page.locator('#view input[type="password"], #addr').first().waitFor({ timeout: 120000 });
  if (!(await s.page.locator('#view input[type="password"]').count())) return;
  s.cut();
  await s.page.locator('#view input[type="password"]').fill(PASS);
  await s.page.locator("#view button", { hasText: "Unlock" }).click();
  await s.page.locator("#addr, #bid-panel, #item, #view h1").first().waitFor({ timeout: 300000 });
  await s.page.waitForFunction(() => !document.querySelector("#modal-root .modal-back"), null, { timeout: 300000 });
  s.uncut();
}

async function openApp(s, hash) {
  await s.open(`${BASE}/#/wallet`);
  await unlock(s);
  if (hash !== "/wallet") {
    await s.page.evaluate((h) => (location.hash = h), hash);
    await s.page.waitForTimeout(1500);
  }
}

// ---------- scenes ----------

const SCENES = {
  async intro() {
    const s = scene("intro");
    await s.open(page_(`intro.html?d=${timing.intro + 0.8}`));
    await s.finish();
  },

  async problem() {
    const s = scene("problem");
    await s.open(page_(`problem.html?d=${timing.problem + 0.8}`));
    await s.finish();
  },

  async landing() {
    const s = scene("landing");
    await s.open(`${BASE}/#/`);
    await s.title("Tacit", "Live on Zcash mainnet and testnet");
    await s.point("#ticker", 800);
    await s.page.waitForTimeout(1500);
    await s.untitle();
    for (const y of [760, 1700, 2600]) {
      await s.scroll(y, 1900);
    }
    await s.scroll(0, 1300);
    await s.point(".launch", 700);
    await s.finish();
  },

  async wallet() {
    const s = scene("wallet");
    await s.open(`${BASE}/#/wallet`);
    await s.title("A wallet in seconds", "Keys and proofs never leave the tab");
    await s.click("#view button >> text=Create a new wallet", { after: 900 });
    await s.click("#modal-root button >> text=12 words", { after: 700 });
    await s.ring("#modal-root .modal", 2200);
    await s.page.waitForTimeout(2200);
    await s.type("#modal-root #p1", PASS, 70);
    await s.click("#modal-root #ok");
    await s.click("#modal-root #go", { after: 300 });
    await s.page.locator("#addr").waitFor({ timeout: 120000 });
    await s.untitle();
    await s.page.waitForTimeout(800);
    await s.ring("#addr-qr", 2600);
    await s.point("#addr", 900);
    state.address = (await s.page.locator("#addr").textContent()).trim();
    saveState();
    await s.finish();
  },

  async faucet() {
    const s = scene("faucet");
    await openApp(s, "/faucet");
    await s.title("Free testnet coins", "Works on NU7 today");
    await s.page.waitForTimeout(1200);
    await s.ring("#view .split > .panel:last-child", 2200);
    await s.page.waitForTimeout(1500);
    await s.click("#fgo", { after: 400 });
    s.cut();
    await s.page.locator("#fout .notice").waitFor({ timeout: 180000 });
    s.uncut();
    await s.untitle();
    await s.ring("#fout", 3000);
    await s.finish();
  },

  async sell() {
    const s = scene("sell");
    await openApp(s, "/sell");
    await s.title("List something", "A fresh 2 of 3 vault per auction");
    await s.type("#item", "Vintage film camera, boxed", 40);
    await s.type("#reserve", "0.001", 80);
    await s.click("#bond");
    await s.page.locator("#bond").selectOption("200000");
    await s.page.waitForTimeout(500);
    await s.ring("#custody", 3200);
    await s.page.waitForTimeout(2600);
    await s.click("#go", { after: 300 });
    s.cut();
    await s.page.waitForFunction(() => location.hash.startsWith("#/auction"), null, { timeout: 300000 });
    await s.page.waitForTimeout(1500);
    s.uncut();
    state.sold = s.page.url().split("#/auction/")[1];
    saveState();
    await s.untitle();
    await s.ring(".auction-head, #phase-badge", 2400);
    await s.finish();
  },

  async bid() {
    const s = scene("bid");
    await openApp(s, `/auction/${state.lot}`);
    await s.title("Seal a bid", "Encrypted to a future drand round");
    await s.ring("#bid-panel", 2200);
    await s.type("#amt", "0.0015", 90);
    await s.click("#bid", { after: 900 });
    await s.ring("#modal-root .modal", 2600);
    await s.point("#pv", 900);
    await s.page.waitForTimeout(3500);
    await s.click("#modal-root #go", { after: 600 });
    s.cut();
    await s.page.locator("#modal-root >> text=Bid sealed and sent").waitFor({ timeout: 600000 });
    await s.page.waitForTimeout(600);
    s.uncut();
    await s.untitle();
    await s.ring("#modal-root .modal", 3000);
    await s.finish();
  },

  async ways() {
    const s = scene("ways");
    await openApp(s, `/auction/${state.lot}`);
    await s.title("Bid from any wallet", "Tacit wallet, Noir extension, or a QR code");
    await s.click('#bid-panel .tabs button[data-t="plugin"]', { after: 1000 });
    await s.click('#bid-panel .tabs button[data-t="ext"]', { after: 500 });
    await s.page.locator("#refund").fill(state.address);
    await s.click("#ext-go", { after: 200 });
    s.cut();
    await s.page.locator("#ext-out .qr").waitFor({ timeout: 60000 });
    await s.page.locator("#ext-out .qr").scrollIntoViewIfNeeded();
    await s.page.waitForTimeout(400);
    s.uncut();
    await s.untitle();
    await s.ring("#ext-out .qr", 3000);
    await s.finish();
  },

  async settle() {
    const s = scene("settle");
    await openApp(s, `/auction/${state.lot}`);
    await s.title("Unseal, settle, audit", "Anyone can check the result");
    await s.ring("#timeline", 2600);
    await s.page.waitForTimeout(2200);
    await s.click("#run-audit", { after: 300 });
    s.cut();
    await s.page.locator("#audit-out >> text=Settled honestly").waitFor({ timeout: 300000 });
    s.uncut();
    await s.untitle();
    await s.ring("#audit-out .notice", 3000);
    await s.page.waitForTimeout(2600);
    await s.point("#audit-out table", 900);
    await s.ring("#audit-out table", 3000);
    await s.finish();
  },

  async proof() {
    const s = scene("proof");
    await s.open(`${BASE}/#/proof`);
    await s.title("Proof, in public", "Every auction, audited");
    await s.page.locator(".big-stats").waitFor({ timeout: 60000 });
    await s.ring(".big-stats", 2400);
    await s.page.waitForTimeout(2600);
    await s.page.goto(`${MAINNET}#/proof`, { waitUntil: "domcontentloaded" });
    await s.page.locator(".big-stats").waitFor({ timeout: 60000 });
    await s.title("Mainnet", "Real ZEC, settled honestly");
    await s.ring(".big-stats", 2400);
    await s.page.waitForTimeout(1800);
    await s.ring("tbody tr:first-child", 2600);
    await s.finish();
  },

  async escrow() {
    const s = scene("escrow");
    await openApp(s, "/escrow");
    await s.title("Shielded escrow", "Pay a stranger without trusting them");
    await s.page.locator("#item").waitFor({ timeout: 60000 });
    await s.page.waitForTimeout(900);
    await s.type("#item", "Logo design, 3 concepts", 40);
    await s.type("#amount", "0.05", 90);
    await s.point("#arb", 800);
    await s.ring("#arb", 2200);
    await s.page.waitForTimeout(2000);
    await s.untitle();
    await s.point("#go", 900);
    await s.ring("#view .split > .panel:first-child", 2800);
    await s.finish();
  },

  async agents() {
    const s = scene("agents");
    await s.open(`${BASE}/#/agents`);
    await s.title("Anyone can be an arbiter", "Two commands, no funds needed");
    await s.page.locator(".agent-row").first().waitFor({ timeout: 60000 });
    await s.ring("#agent-list", 2400);
    await s.page.waitForTimeout(2600);
    await s.ring("pre.cmd", 3600);
    await s.point("pre.cmd", 900);
    await s.finish();
  },

  async outro() {
    const s = scene("outro");
    await s.open(page_(`outro.html?d=${timing.outro + 0.8}`));
    await s.finish(1.6);
  },
};

const want = process.argv.slice(2);
const order = want[0] === "all" ? Object.keys(SCENES) : want;
for (const id of order) {
  if (!SCENES[id]) throw new Error(`no scene ${id}`);
  await SCENES[id]();
}
await ctx.close();
