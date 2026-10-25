// Questions people actually ask before trusting an auction or an escrow
// with money. Used on the landing page and the FAQ page.

export const FAQ = [
  {
    q: "What is Tacit?",
    a: `<p>Tacit is a market for things that should be sold fairly and paid for privately. It runs sealed-bid auctions and shielded escrow on Zcash. Everyone can see the final price and check the payout, but nobody learns who bid, who bought or who paid whom.</p>`,
  },
  {
    q: "How can a bid be sealed if nobody holds the key?",
    a: `<p>Your browser encrypts the bid with drand timelock encryption to a specific future round of the drand randomness beacon. The key for that round doesn't exist until the drand network, a threshold group of independent organisations, publishes it. Until then nobody can open any bid: not the seller, not the operator, not us. Once it's published, everyone can open every bid at the same moment, and no bidder can refuse to reveal.</p>`,
  },
  {
    q: "Who holds the money while the auction runs?",
    a: `<p>Nobody alone. Every bond goes into a fresh 2-of-3 FROST vault on Zcash's Ironwood shielded pool. The seller, an operator and an arbiter each hold one share of the spending key, made together with distributed key generation, so the whole key never exists anywhere. Moving funds takes two of the three, and each cosigner recomputes the payout from the rules and refuses anything else.</p>`,
  },
  {
    q: "What can the seller, the operator or a chain observer see?",
    a: `<p>Before the bids unseal: how many deposits arrived, and that each is exactly the bond. After: every bid amount and the clearing price. Never: who sent a deposit, or which wallet owns a refund address. A chain observer sees only that shielded transactions happened.</p>`,
  },
  {
    q: "Why does every bid deposit the same amount?",
    a: `<p>So deposits can't give bids away. If you deposited exactly what you bid, anyone with the vault's viewing key could read the bids before they unseal. Instead everyone deposits the same bond, which is also the highest possible bid. The real bid travels inside the sealed memo, and whatever you don't pay comes back.</p>`,
  },
  {
    q: "How do I know the result is honest?",
    a: `<p>Press "Run audit in this tab" on any auction. Using nothing but the published terms, your browser finds every deposit with the vault's public viewing key, verifies the drand beacon, opens the bids, recomputes the settlement the rules require and compares it with what the vault actually paid. It takes a few seconds and needs no trust in us.</p>`,
  },
  {
    q: "How does escrow work?",
    a: `<p>You send the other side an invite link. Your browsers and an arbiter generate a shared 2-of-3 vault. The buyer pays in. When the deal is done the buyer releases the money to the seller; if it falls through, the seller refunds the buyer. In both cases the arbiter cosigns automatically, because a party is only giving up its own claim. The arbiter decides by hand only in a dispute.</p>`,
  },
  {
    q: "Can I fund my wallet from Bitcoin, Ethereum or Solana?",
    a: `<p>Yes, on mainnet. Tacit asks NEAR Intents 1Click for a quote that delivers ZEC straight to a fresh shielded address of your wallet. You pay the one-time deposit address from whatever wallet you like. The deposit is public on its own chain, but nothing on the Zcash side links the ZEC to your bids or payments.</p>`,
  },
  {
    q: "Can I bid with Noir, Zashi or another wallet I already use?",
    a: `<p>Yes. On any auction, the "Noir Wallet" tab connects a Zcash browser extension: your bid is sealed in this tab, and the extension pays the bond with the sealed memo after one approval. Refunds go back to that wallet's shielded address. The "Any wallet" tab makes a ZIP 321 payment request with a QR code for mobile wallets such as Zashi, Zingo or YWallet. Or use the Tacit wallet built into the app. All three settle the same way.</p>`,
  },
  {
    q: "How do I get testnet coins?",
    a: `<p>Open the Faucet page (it's in the top menu), paste a shielded testnet address or use your Tacit wallet's, and press send. Tacit's own faucet sends test coins straight to that address, once a day per address. The wallet's "Get free TAZ" button does the same in one tap. The faucet refills from donations to its transparent address, which it shields automatically.</p>`,
  },
  {
    q: "Can I install Tacit like an app?",
    a: `<p>Yes. In Chrome, Edge or Brave press "Install" in the header (or "Add to Home screen" on your phone). Tacit then opens in its own window, with shortcuts to the market, wallet and escrow. Your keys stay on the device either way.</p>`,
  },
  {
    q: "Where do my keys live?",
    a: `<p>In this browser tab. Your recovery phrase, seat key and vault shares are encrypted with your passcode and kept in the browser's storage. Syncing works by downloading whole ranges of blocks and decrypting them locally, so the coordinator never learns which notes are yours. Write the words down: they are the only way back in.</p>`,
  },
  {
    q: "What does it cost?",
    a: `<p>Only Zcash network fees, which follow ZIP 317: usually 0.0001 ZEC for a bid or a payment. Settlement fees come out of the vault. Cross-chain funding through 1Click adds the solvers' spread and a small 1Click fee. Tacit takes nothing.</p>`,
  },
  {
    q: "Is it live?",
    a: `<p>The full flow runs on Zcash testnet and on a local regtest chain: browser wallets, vault key generation, sealed bids, automatic settlement, audits and escrow. Use test coins (TAZ) from the testnet faucet. Mainnet use is possible, but this is new software, so start with amounts you can afford to lose.</p>`,
  },
  {
    q: "Is the code open?",
    a: `<p>Yes. Everything, from the Rust protocol code to the browser app, is on GitHub under MIT or Apache 2.0, along with the exact protocol rules and a plain threat model that says what is and isn't private.</p>`,
  },
];

export function faqList(items = FAQ) {
  return `<div class="faq-list">${items
    .map((f, i) => `<details ${i === 0 ? "open" : ""}><summary>${f.q}</summary><div class="ans">${f.a}</div></details>`)
    .join("")}</div>`;
}

// Only one answer open at a time, like Penumbra's accordion.
export function wireFaq(root) {
  root.querySelectorAll(".faq-list").forEach((list) => {
    list.addEventListener(
      "toggle",
      (e) => {
        if (e.target.open) list.querySelectorAll("details[open]").forEach((d) => d !== e.target && (d.open = false));
      },
      true,
    );
  });
}
