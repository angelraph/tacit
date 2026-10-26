# Conformance vectors

Real auctions, frozen after they settled. Each file holds an auction's
published terms, every deposit its vault received (memo included, read through
the published viewing key), the drand beacon for its closing round, and the
payments the settlement transaction actually made on chain.

| File | Network | Bids | Price | Settlement |
|---|---|---|---|---|
| `mainnet-lot-2.json` | mainnet | 2 | 0.001 ZEC (second price) | `8d9f0e7040eda2052a8cde555e4c2595ae8decff9c17be47f080da7525bef49c` |
| `testnet-lot-1.json` | testnet | 2 | 0.012 TAZ (second price) | `179c77a2…` |
| `testnet-lone-bid.json` | testnet | 1 | 0.001 TAZ (a lone bidder pays the reserve) | `3e1b0e39…` |

`cargo test -p tacit-core --test conformance` replays them with no network
access: the beacon must verify against drand's quicknet key, the sealed bids
must open, and the settlement rules must reproduce the winner, the price, the
fee and every on-chain payment exactly. Any implementation of the format in
[docs/ZIP-DRAFT.md](../docs/ZIP-DRAFT.md) can check itself the same way.

Make a new one from any honestly settled auction:

```bash
tacit-dev vector <coordinator url> <auction id> > test-vectors/<name>.json
```
