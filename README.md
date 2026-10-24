# Tacit

Sealed-bid auctions and shielded escrow on Zcash. The price is public; the participants aren't.

Online sealed-bid auctions have always needed an auctioneer who can read every bid, and online escrow has always needed a custodian who holds the money. Tacit replaces both with things anyone can check: drand timelock encryption, a 2-of-3 FROST vault on Zcash's Ironwood shielded pool, and a published viewing key.

- **Sealed bids.** A bid is timelock-encrypted to a future drand round and rides in a shielded memo. Nobody can read a bid before that round, the seller and the operator included, and no bidder can refuse to reveal.
- **No single custodian.** Bonds and escrow payments sit in an Ironwood address whose spending key is split 2 of 3 by distributed key generation. No one ever holds the whole key, and every cosigner recomputes the payout and refuses any other.
- **Anyone can audit.** The vault's viewing key is part of the published terms. Any browser can rebuild the settlement the rules require and check that the vault paid exactly that, without learning who bid.
- **Everything in the browser.** The web app is a full shielded wallet: it syncs by trial-decrypting blocks locally, seals bids, proves Ironwood transactions (about 20 s on a laptop) and runs vault seats. Keys never leave the tab.
- **Fund from any chain.** NEAR Intents 1Click swaps USDC, ETH, SOL or BTC straight into a fresh shielded address.

See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the exact rules and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for what is and isn't private.

## What you can do

| | |
|---|---|
| **Bid** | Pick an auction, slide to your bid, and press seal. Your browser encrypts the bid to the closing drand round and deposits the fixed bond to the vault. Every deposit looks the same on chain. |
| **Sell** | Describe the item, set a reserve and a bond. Your browser runs key generation with an operator agent and an arbiter agent, then publishes the terms. When the bids unseal, the agents settle automatically. |
| **Escrow** | Send the other side an invite link. Your browsers and an arbiter generate a shared vault. The buyer pays in. The buyer releases the money to the seller, or the seller refunds the buyer. The arbiter steps in only for disputes. |
| **Audit** | On any auction, press "Run audit in this tab". It finds every deposit with the public viewing key, opens the bids with the drand beacon, recomputes the settlement and compares it with what the vault paid. |

## How it fits together

```text
 browser (web/)                          coordinator (tacit-coordinator)
 ┌──────────────────────────────┐        ┌──────────────────────────────┐
 │ tacit-wasm in a Web Worker   │  JSON  │ auction + escrow registry    │
 │  · wallet: scan, witnesses   │◄──────►│ sealed seat mailboxes        │
 │  · seal bid, prove, sign     │        │ chain gateway (public data)  │──► Zebra (lightwalletd gRPC)
 │  · FROST seat (DKG, signing) │        └──────────────────────────────┘
 │  · audit from terms          │                     ▲
 └──────────────────────────────┘                     │ sealed envelopes
                                         ┌────────────┴─────────────────┐
              drand quicknet ◄────────── │ tacit agent (operator/arbiter)│
                                         └──────────────────────────────┘
```

The coordinator never holds a key, never sees a readable seat message, and only relays public chain data and finished, encrypted transactions. The worst it can do is drop messages.

## Status

Working end to end on a Zebra regtest chain with Zcash NU6.3 (Ironwood) active:

- 2 of 3 FROST key generation and spend authorization across separate processes and browsers, only talking through the coordinator in sealed envelopes
- sealed-bid auctions: bids, unsealing, second-price settlement by agents, and an outside audit
- shielded escrow: release, refund, and arbiter decisions
- the browser wallet's code paths, checked natively against the chain gateway (`tacit-dev light-e2e`)

## Layout

| Path | What it does |
|---|---|
| `crates/tacit-core` | auction and escrow rules, sealed-bid memos, drand beacon verification, seat envelopes |
| `crates/tacit-vault` | FROST vaults; key generation and signing as message-passing state machines |
| `crates/tacit-light` | the browser wallet: compact block scanning, witnesses, building and proving payments, audits |
| `crates/tacit-wasm` | `tacit-light` and vault seats compiled to WebAssembly for the web app |
| `crates/tacit-wallet` | the native wallet on librustzcash, auction helpers, coordinator client, `tacit-dev` |
| `crates/tacit-coordinator` | registry, seat mailboxes and the chain gateway (axum and SQLite) |
| `crates/tacit-cli` | the `tacit` command, including `tacit agent` and `tacit decide` |
| `web/` | the web app (plain ES modules, no build step besides the WebAssembly) |

## Run it locally on regtest

You need Rust, Python 3 and a Zebra 6.4+ node in regtest mode (`tacit-dev init` writes its config). Building the WebAssembly needs clang and wasm-bindgen-cli 0.2.129 (on Windows, run `web/build.sh` in WSL).

```bash
# chain, block clock and coordinator with its chain gateway
cargo run -p tacit-wallet --bin tacit-dev -- mine-every 20
cargo run -p tacit-coordinator -- 127.0.0.1:8650 coordinator.sqlite http://127.0.0.1:9067

# an operator and an arbiter agent, each with its own home
cargo run -p tacit-cli -- --home ./operator init
cargo run -p tacit-cli -- --home ./operator agent --role operator --name "Tacit operator"
cargo run -p tacit-cli -- --home ./arbiter init
cargo run -p tacit-cli -- --home ./arbiter agent --role arbiter --name "Tacit arbiter"

# the web app
sh web/build.sh
python web/serve.py 8770        # then open http://127.0.0.1:8770
```

Fund a browser wallet with `cargo run -p tacit-wallet --bin tacit-dev -- fund <address> <zatoshi>`.

The end-to-end checks:

```bash
cargo run -p tacit-wallet --bin tacit-dev -- auction-e2e
cargo run -p tacit-wallet --bin tacit-dev -- light-e2e
```

## License

MIT or Apache 2.0, at your option.
