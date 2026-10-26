# Tacit demo transcript

The narration of the two minute demo video, scene by scene. Times are where each scene's narration starts in the video. Captions for upload are in [media/tacit-demo-2min.srt](../media/tacit-demo-2min.srt), built (with the captioned video) by `python demo/build.py --spec scenes-2min.json`.

**0:00  Intro**

This is Tacit. Sealed bid auctions and shielded escrow on Zcash. The price is public. The bidders are not.

**0:10  The problem**

Every auction on a public chain leaks. Bids get sniped and buyers get tracked. And Zcash has no smart contracts, so fair markets needed a middleman you had to trust. Tacit fixes both.

**0:23  The app**

Tacit runs in your browser, on mainnet and testnet, built on drand timelock encryption and two of three FROST vaults on Ironwood.

**0:32  A wallet in seconds**

Make a wallet with a recovery phrase and a passcode. It syncs by decrypting blocks locally, so the server never learns which coins are yours.

**0:42  List something**

A seller sets a reserve, a bond and a bidding window. Their browser, an operator and an arbiter create a fresh vault, one key share each, and Tacit shows exactly who could move the money.

**0:56  Seal a bid**

Every bidder deposits the same bond, so the chain shows nothing about amounts. The bid is encrypted to a future drand round, so nobody can open it early, not even us.

**1:08  Unseal, settle, audit**

When the round is published, every bid opens at once. The agents recompute the winner and cosign one shielded payout, and your browser audits every step. Settled honestly.

**1:20  Proof, in public**

The proof page audits every auction in public. And our first auction with real ZEC has settled on mainnet, and passed the same audit.

**1:30  Shielded escrow**

The same vaults power escrow. The buyer pays in and releases when the deal is done, and nobody on chain can tell it was an escrow.

**1:41  Anyone can be an arbiter**

Anyone can be an arbiter. One binary, two commands. The more independent agents, the stronger every market gets.

**1:50  Close**

Tacit. Sealed markets on Zcash. Open source, live today. Audit it yourself.

Total length 1:57.
