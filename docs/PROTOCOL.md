# Tacit Protocol

This is the exact set of rules Tacit's code implements. Everything here can be
checked by anyone with the auction terms, a Zcash node and the drand beacon.

## Pieces

**Vault.** An Ironwood address whose spend authority is split 2 of 3 with FROST
(RedPallas, rerandomized). The three seats are the seller, the operator and an
arbiter. The key is made with a distributed key generation, so no one ever
holds the whole spending key. One participant picks the viewing key's nullifier
and address randomness and shares it; the others check it matches the group key.

**Sealed bid.** A bid is encrypted with drand timelock encryption to the
auction's closing round. Only the beacon for that round can open it, and the
beacon doesn't exist until the drand network publishes it.

**Audit key.** The vault's unified full viewing key is published in the terms.
It reveals every deposit's amount and memo and every payment the vault makes,
but never who sent a deposit.

## Auction terms

| Field | Meaning |
|---|---|
| `network` | `main`, `test` or `regtest` |
| `vault_ufvk` | the vault's unified full viewing key |
| `seller` | unified address the seller is paid at |
| `item` | description, at most 256 bytes |
| `created_height` | chain height the vault was created at |
| `reserve` | lowest acceptable price, zatoshi |
| `bond` | the exact deposit every bid makes; a ZIP 318 canonical denomination (1, 2 or 5 times a power of ten) and the highest possible bid |
| `bids_until_height` | deposits mined above this height don't count |
| `close_round` | drand quicknet round the bids are sealed to |
| `rule` | first price or second price |

The auction id is `BLAKE2b-256` with personalization `TacitAuction__v1` over
the fields in the order above. Strings are prefixed with their byte length as
a little endian u32, integers are little endian, and the rule is one byte
(1 first price, 2 second price). Changing any term changes the id, and every
sealed bid names the id it was made for.

## Memo format

Every Tacit memo is 512 bytes:

```
0xF5 | "TACIT" | version = 1 | kind | body | zero padding
```

`0xF5` is ZIP 302's marker for arbitrary binary data.

**Sealed bid (kind 1).** Body is the auction id (32 bytes), then a u16 big
endian length, then the sealed box. The sealed box is

```
close round (u64 BE) | tlock ciphertext (128) | ChaCha20-Poly1305 ciphertext
```

The tlock ciphertext locks a random 16 byte file key to the round. The payload
key is `HKDF-SHA256(salt = "tacit/seal/v1", ikm = file key, info = round)`,
the nonce is zero (each key is used once), and the associated data is the
round and the tlock ciphertext. The plaintext is

```
version = 1 | auction id (32) | bid in zatoshi (u64 BE) | refund receiver (43)
```

The refund receiver is the raw Orchard-protocol receiver of a fresh diversified
address, which is how Ironwood receivers are encoded. Repeating the auction id
inside the box stops a sealed bid from being moved into another auction.

## Bidding

A bidder sends exactly `bond` zatoshi to the vault address in a transaction
whose only shielded component is Ironwood, with the sealed bid as the memo.

## Settling

Once the beacon for `close_round` is published, anyone can compute the outcome.
The beacon is verified against the quicknet group key first.

For each deposit to the vault (identified by txid and action index):

1. If the memo isn't a sealed bid for this auction, or doesn't open, the deposit
   is **unreadable**. There is nowhere to refund it, so it goes to the seller.
2. Otherwise it is refunded in full if it was mined above `bids_until_height`,
   if its value isn't exactly `bond`, if the bid is above `bond`, or if the bid
   is below `reserve`.
3. Everything left is a valid bid.

Valid bids are ranked highest first. Ties go to the lower value of
`BLAKE2b-256(personal "TacitTiebreak_v1", beacon randomness || deposit id)`,
which nobody can predict before the round.

The price is the winning bid under first price. Under second price it's the
larger of the runner up's bid and the reserve (so a lone bidder pays the
reserve).

Payouts: the winner gets `bond - price` back, every other valid bid and every
refunded deposit gets its full value back, and the seller gets the price plus
any unreadable deposits, minus the fee.

The fee follows ZIP 317: 5,000 zatoshi times the larger of 2, the number of
deposits and the number of payouts plus one. The extra one is a zero-valued
change output back to the vault that every settlement carries, so that a
transaction spending a vault's whole balance looks like any other transaction. If the seller's share can't cover it (no
valid bids), the seller gets no output and the fee is taken evenly from the
refunds, with any remainder from the first.

The vault pays everything in a single transaction, signed by any two seats.
Its change output is always zero.

## Escrow

An escrow is a 2 of 3 vault whose seats are, in order, the buyer, the seller
and an arbiter. Its terms are:

| Field | Meaning |
|---|---|
| `network` | `main`, `test` or `regtest` |
| `vault_ufvk` | the vault's unified full viewing key |
| `seats` | hex X25519 seat keys of buyer, seller and arbiter |
| `buyer` | unified address refunds go to |
| `seller` | unified address payment goes to |
| `item` | description, at most 256 bytes |
| `amount` | what the buyer pays in, zatoshi |
| `created_height` | chain height the vault was created at |

The escrow id is `BLAKE2b-256` with personalization `TacitEscrow___v1` over
the fields in the order above, with the same encoding as auction terms (each
seat key is a length-prefixed string).

The vault pays out exactly once, everything it holds, either to `seller` or
to `buyer`, minus the ZIP 317 fee: 5,000 zatoshi times the larger of 2 and
the number of notes spent. Like an auction settlement, the payout carries one
zero-valued change output back to the vault.

Any two seats can sign the payout. An arbiter running unattended cosigns only
when a party gives up its own claim: the buyer asking to pay the seller, or
the seller asking to refund the buyer. Every other request is a dispute and
waits for the arbiter's own decision (`tacit decide <room>`). Before signing,
any seat checks the transaction pays exactly the amount above to exactly that
address and spends every note the vault holds.

## Rooms and agents

Seats talk through the coordinator's rooms in sealed envelopes. A room has a
public `purpose` so that seats running unattended (agents) know what is being
asked of them:

| Purpose | Meaning |
|---|---|
| `dkg` | key generation for a new vault |
| `settle:<auction id>:<seat>` | seat `<seat>` coordinates the auction's settlement |
| `escrow:<escrow id>:<seller or buyer>:<seat>` | seat `<seat>` asks to pay the escrow out to that party |

An operator agent settles every auction on its vaults once the closing
beacon is out and bidding has ended, asking the arbiter seat to cosign. Every
agent recomputes what the rules require before signing anything and refuses
any other transaction.

## Chain gateway

Browsers can't speak gRPC, so a coordinator can also relay a light wallet
server's public data as JSON: chain info, tree states, compact blocks trimmed
to their Ironwood actions, raw transactions, and broadcasting. Browsers
trial-decrypt blocks themselves; the gateway never receives a key, an address
or a note.

## Verifying

`verify` takes nothing but the terms and the beacon. It imports the audit key
into a fresh wallet, scans from `created_height`, rebuilds the deposit list,
recomputes the outcome with the rules above, finds the one transaction that
spent from the vault, and compares its payments with the expected ones. Any
missing or extra payment is reported.
