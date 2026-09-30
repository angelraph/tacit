self.onmessage = async ({ data }) => {
  const pkg = data.threads ? "./pkg-threads/tacit_wasm.js" : "./pkg/tacit_wasm.js";
  const mod = await import(pkg);
  const t0 = performance.now();
  await mod.default();
  if (data.threads) {
    await mod.initThreadPool(navigator.hardwareConcurrency);
  }
  const t1 = performance.now();
  const prover = new mod.IronwoodProver();
  const t2 = performance.now();
  self.postMessage({ step: "key", initMs: t1 - t0, keyMs: t2 - t1 });

  for (const outputs of data.runs) {
    const s = performance.now();
    const bytes = prover.prove(outputs);
    self.postMessage({ step: "proof", outputs, ms: performance.now() - s, bytes });
  }
  self.postMessage({ step: "done" });
};
