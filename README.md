# Tacit

Sealed-bid auctions and shielded escrow on Zcash. The price is public; the participants aren't.

- **Sealed bids.** A bid is timelock-encrypted to a future drand round and rides in a shielded memo. Nobody can read a bid before that round, the seller and the operator included, and no bidder can refuse to reveal.
- **2 of 3 FROST vaults.** Bonds go into an Ironwood address whose spending key is split between the seller, the operator and an arbiter by distributed key generation. No one ever holds the whole key, and any two seats can settle.
- **Public audit.** The vault's viewing key is part of the published auction terms. Anyone can rebuild the settlement the rules require and check the vault paid exactly that. They still can't see who bid.

Every bid deposits the same bond, so deposits look identical until the bids unseal. Settlement is one fully shielded transaction that pays the seller, refunds the losers and returns the winner's change.

See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the exact rules and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for what is and isn't private.

## Status

Working on a Zebra regtest chain with Zcash NU6.3 (Ironwood) active:

- 2 of 3 FROST key generation and spend authorization, run by three separate `tacit` processes that only talk through the coordinator in sealed envelopes
- full sealed-bid auctions: bids, unsealing, second-price settlement, cosigning and an outside audit
- Ironwood proofs in the browser: about 20 s per bid on a 4 core laptop ([demo](spikes/wasm-bench))

Next: testnet, then the browser app.

## Layout

| Crate | What it does |
|---|---|
| `tacit-core` | auction rules, sealed-bid memos, drand beacon verification, seat envelopes |
| `tacit-vault` | FROST vaults, and key generation and signing as message-passing state machines |
| `tacit-wallet` | shielded wallet on librustzcash, Ironwood proving, auction helpers, coordinator client |
| `tacit-coordinator` | auction registry and mailboxes for vault seats (axum and SQLite); it never sees a key or a readable message |
| `tacit-cli` | the `tacit` command |
| `tacit-wasm` | the prover compiled to WebAssembly |
| `tacit-regtest` | helpers for a local Zebra regtest chain |

## Try it on regtest

You need Rust and a Zebra 6.4+ node in regtest mode (`tacit-dev init` writes its config). Then:

```bash
cargo run -p tacit-wallet --bin tacit-dev -- auction-e2e
```

Or by hand, with one home directory per person:

```bash
cargo run -p tacit-coordinator
tacit --home ./alice init
tacit-dev fund $(tacit --home ./alice address) 101000000
tacit --home ./seller vault new lot7 --seats me,<operator seat>,<arbiter seat>
tacit --home ./operator vault join lot7 <room>
tacit --home ./seller auction create --vault lot7 --item "lot 7" --reserve 0.1 --bond 1 --close-in 300
tacit --home ./alice bid <auction> 0.35
tacit --home ./operator auction settle <auction> --vault lot7 --with 1
tacit --home ./seller auction cosign <auction> --vault lot7 --room <room> --from 2
tacit --home ./anyone auction verify <auction>
```

Before cosigning, a seat recomputes the settlement from its own view of the chain. It refuses to sign any transaction whose payments differ.

## License

MIT or Apache 2.0, at your option.
