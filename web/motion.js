// Small motion toolkit: scroll reveals, a glyph scramble for headlines, a
// moving blur across hero words, count-ups, and a cursor glow on cards.
// Everything respects prefers-reduced-motion.

const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;

// ---------- reveal on scroll ----------

let observer = null;
export function reveals(root = document) {
  if (reduced) {
    root.querySelectorAll(".reveal").forEach((el) => el.classList.add("in"));
    return;
  }
  observer ||= new IntersectionObserver(
    (entries) => {
      for (const e of entries) {
        if (e.isIntersecting) {
          e.target.classList.add("in");
          observer.unobserve(e.target);
        }
      }
    },
    { rootMargin: "0px 0px -8% 0px", threshold: 0.08 },
  );
  root.querySelectorAll(".reveal:not(.in)").forEach((el) => observer.observe(el));
}

// ---------- glyph scramble (Namada) ----------

const GLYPHS = "ЖБЪДЯИЛ#%&$@ΣΩΞ01<>/\\";

export function scramble(el, { duration = 900, final } = {}) {
  const target = final ?? el.dataset.text ?? el.textContent;
  el.dataset.text = target;
  if (reduced) {
    el.textContent = target;
    return Promise.resolve();
  }
  const start = performance.now();
  return new Promise((resolve) => {
    const tick = (now) => {
      const t = Math.min(1, (now - start) / duration);
      let out = "";
      for (let i = 0; i < target.length; i++) {
        const c = target[i];
        if (c === " " || c === "\n") {
          out += c;
          continue;
        }
        // Letters settle left to right.
        const settleAt = (i / target.length) * 0.75 + 0.2;
        out += t >= settleAt ? c : GLYPHS[(Math.random() * GLYPHS.length) | 0];
      }
      el.textContent = out;
      if (t < 1) requestAnimationFrame(tick);
      else {
        el.textContent = target;
        resolve();
      }
    };
    requestAnimationFrame(tick);
  });
}

// Every few seconds a couple of letters glitch, like Namada's headline.
export function idleGlitch(el, every = 3800) {
  if (reduced) return () => {};
  const text = el.dataset.text || el.textContent;
  el.dataset.text = text;
  const id = setInterval(() => {
    if (!document.body.contains(el)) return clearInterval(id);
    const chars = [...text];
    const picks = new Set();
    while (picks.size < 2) {
      const i = (Math.random() * chars.length) | 0;
      if (chars[i] !== " ") picks.add(i);
    }
    picks.forEach((i) => (chars[i] = GLYPHS[(Math.random() * GLYPHS.length) | 0]));
    el.textContent = chars.join("");
    setTimeout(() => (el.textContent = text), 160);
  }, every);
  return () => clearInterval(id);
}

// ---------- penumbra blur sweep across hero words ----------

export function blurSweep(container, every = 2600) {
  const words = [...container.querySelectorAll(".word")];
  if (!words.length || reduced) return () => {};
  // Like Penumbra: only an edge of the headline sits in the shadow, and the
  // shadow drifts from one edge to the other.
  const edges = [0, words.length - 1, 1, words.length - 2];
  let k = 0;
  const step = () => {
    words.forEach((w, i) => w.classList.toggle("blur", i === edges[k % edges.length]));
    k++;
  };
  step();
  const id = setInterval(() => {
    if (!document.body.contains(container)) return clearInterval(id);
    step();
  }, every);
  return () => clearInterval(id);
}

// ---------- count up ----------

export function countUp(el, to, { decimals = 0, prefix = "", suffix = "", duration = 1200 } = {}) {
  const fmt = (v) => prefix + v.toLocaleString(undefined, { minimumFractionDigits: decimals, maximumFractionDigits: decimals }) + suffix;
  if (reduced || !Number.isFinite(to)) {
    el.textContent = Number.isFinite(to) ? fmt(to) : "–";
    return;
  }
  const from = Number(el.dataset.v || 0);
  el.dataset.v = to;
  const start = performance.now();
  const tick = (now) => {
    const t = Math.min(1, (now - start) / duration);
    const e = 1 - Math.pow(1 - t, 3);
    el.textContent = fmt(from + (to - from) * e);
    if (t < 1) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

// ---------- cursor glow on cards ----------

export function glowCards(root = document) {
  root.querySelectorAll(".card, .go, .glow").forEach((el) => {
    if (el.dataset.glow) return;
    el.dataset.glow = "1";
    el.addEventListener("pointermove", (e) => {
      const r = el.getBoundingClientRect();
      el.style.setProperty("--mx", `${e.clientX - r.left}px`);
      el.style.setProperty("--my", `${e.clientY - r.top}px`);
    });
  });
}

// ---------- scroll progress of an element (0 at its top, 1 at its end) ----------

export function onScrollProgress(el, fn) {
  if (!el) return () => {};
  let raf = 0;
  const update = () => {
    raf = 0;
    if (!document.body.contains(el)) return off();
    const r = el.getBoundingClientRect();
    const total = r.height - innerHeight;
    fn(Math.min(1, Math.max(0, -r.top / Math.max(1, total))));
  };
  const on = () => (raf ||= requestAnimationFrame(update));
  const off = () => {
    removeEventListener("scroll", on);
    removeEventListener("resize", on);
  };
  addEventListener("scroll", on, { passive: true });
  addEventListener("resize", on);
  update();
  return off;
}

export { reduced };
