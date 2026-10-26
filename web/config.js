// Where the app finds its coordinator and chain gateway. When the
// coordinator serves the app itself (a deployment), that's the same origin;
// the local dev server on :8770 talks to a coordinator on :8650. Anyone can
// point their copy elsewhere from the chain button in the header, or with
// ?gateway=https://... in the URL.
window.TACIT_GATEWAY = location.port === "8770" ? "http://127.0.0.1:8650" : location.origin;

// The public deployments, one per network. Each keeps its own wallets, since
// browser storage belongs to the origin.
window.TACIT_SITES = {
  main: "https://tacit-mainnet.up.railway.app",
  test: "https://tacit-production-6a96.up.railway.app",
};
