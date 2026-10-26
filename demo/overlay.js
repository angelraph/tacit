// Motion graphics drawn over the real app while the demo records: a cursor
// that glides to what gets clicked, a ripple on each click, a highlight ring,
// and a lower-third title card. The recorder drives it through window.__demo.
(() => {
  const css = `
    #demo-cursor { position: fixed; left: 0; top: 0; z-index: 2147483647; pointer-events: none; width: 30px; height: 30px;
      transform: translate(960px, 620px); transition: transform 0.9s cubic-bezier(.22,.9,.24,1); filter: drop-shadow(0 4px 10px rgba(0,0,0,.5)); opacity: 0; }
    #demo-cursor.on { opacity: 1; }
    #demo-cursor svg { width: 30px; height: 30px; }
    .demo-ripple { position: fixed; z-index: 2147483646; pointer-events: none; width: 20px; height: 20px; margin: -10px 0 0 -10px; border-radius: 50%;
      border: 2px solid #b6a8ff; box-shadow: 0 0 24px #8e7bff; animation: demo-ripple .7s ease-out forwards; }
    @keyframes demo-ripple { from { transform: scale(.4); opacity: 1; } to { transform: scale(3.4); opacity: 0; } }
    .demo-ring { position: fixed; z-index: 2147483645; pointer-events: none; border-radius: 16px; border: 2px solid rgba(182,168,255,.95);
      box-shadow: 0 0 0 6px rgba(109,92,255,.18), 0 0 40px rgba(109,92,255,.45); animation: demo-ring-in .5s cubic-bezier(.2,.8,.2,1) both; }
    @keyframes demo-ring-in { from { opacity: 0; transform: scale(1.08); } }
    #demo-third { position: fixed; left: 56px; bottom: 56px; z-index: 2147483644; pointer-events: none; padding: 22px 30px 22px 26px; border-radius: 22px;
      background: linear-gradient(135deg, rgba(20,18,36,.88), rgba(12,11,22,.82)); border: 1px solid rgba(182,168,255,.22);
      backdrop-filter: blur(14px); box-shadow: 0 20px 60px rgba(0,0,0,.45); display: flex; gap: 18px; align-items: center;
      opacity: 0; transform: translateY(26px); transition: opacity .6s ease, transform .7s cubic-bezier(.2,.8,.2,1); }
    #demo-third.on { opacity: 1; transform: none; }
    #demo-third .bar { width: 5px; align-self: stretch; border-radius: 4px; background: linear-gradient(#b6a8ff, #4a43f0); }
    #demo-third b { display: block; font: 600 34px/1.1 "Space Grotesk", Inter, sans-serif; color: #f4f3ff; letter-spacing: -.02em; }
    #demo-third span { display: block; margin-top: 6px; font: 400 21px/1.3 Inter, sans-serif; color: #b6b3d4; }
    #demo-ff { position: fixed; right: 56px; top: 110px; z-index: 2147483644; pointer-events: none; padding: 12px 20px; border-radius: 999px;
      background: rgba(12,11,22,.85); border: 1px solid rgba(182,168,255,.3); color: #d9d2ff; font: 500 20px Inter, sans-serif;
      opacity: 0; transition: opacity .4s; }
    #demo-ff.on { opacity: 1; }
    html { scroll-behavior: smooth !important; }
  `;
  const arrow = `<svg viewBox="0 0 24 24"><path d="M4 2l15 9.5-6.6 1.4L16.6 21l-3 1.4-4.2-8.2L4 18.6z" fill="#fff" stroke="#1b1830" stroke-width="1.4" stroke-linejoin="round"/></svg>`;
  let cursor, third, ff;
  let pos = { x: 960, y: 620 };
  const ready = () => {
    if (cursor) return;
    const st = document.createElement("style");
    st.textContent = css;
    document.documentElement.appendChild(st);
    cursor = document.createElement("div");
    cursor.id = "demo-cursor";
    cursor.innerHTML = arrow;
    third = document.createElement("div");
    third.id = "demo-third";
    third.innerHTML = `<i class="bar"></i><div><b></b><span></span></div>`;
    ff = document.createElement("div");
    ff.id = "demo-ff";
    document.documentElement.append(cursor, third, ff);
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  window.__demo = {
    async move(x, y, ms = 900) {
      ready();
      cursor.classList.add("on");
      cursor.style.transition = `transform ${ms}ms cubic-bezier(.22,.9,.24,1)`;
      cursor.style.transform = `translate(${x - 4}px, ${y - 2}px)`;
      pos = { x, y };
      await sleep(ms);
    },
    ripple() {
      ready();
      const r = document.createElement("div");
      r.className = "demo-ripple";
      r.style.left = pos.x + "px";
      r.style.top = pos.y + "px";
      document.documentElement.appendChild(r);
      setTimeout(() => r.remove(), 800);
    },
    ring(rect, ms = 1800) {
      ready();
      const r = document.createElement("div");
      r.className = "demo-ring";
      Object.assign(r.style, { left: rect.x - 10 + "px", top: rect.y - 10 + "px", width: rect.width + 20 + "px", height: rect.height + 20 + "px" });
      document.documentElement.appendChild(r);
      setTimeout(() => r.remove(), ms);
    },
    title(t, s) {
      ready();
      third.querySelector("b").textContent = t;
      third.querySelector("span").textContent = s || "";
      third.classList.add("on");
    },
    untitle() {
      ready();
      third.classList.remove("on");
    },
    ff(text) {
      ready();
      ff.textContent = text || "";
      ff.classList.toggle("on", !!text);
    },
  };
  document.addEventListener("DOMContentLoaded", ready);
})();
