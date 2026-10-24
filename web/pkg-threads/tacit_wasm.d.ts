/* tslint:disable */
/* eslint-disable */

/**
 * An audit of one auction, from its public terms.
 */
export class Audit {
    free(): void;
    [Symbol.dispose](): void;
    addTx(txid: string, raw: string): void;
    height(): number;
    /**
     * `birthday` is the tree state at the terms' `created_height`.
     */
    constructor(terms: string, birthday: string);
    /**
     * The audit report as JSON. Pass the closing round's beacon signature
     * (hex) once it exists, or an empty string before that.
     */
    report(beacon_signature: string): string;
    scan(blocks: string): void;
    vaultAddress(): string;
    /**
     * JSON list of txids whose full transactions the audit still needs.
     */
    wanted(): string;
}

/**
 * This seat's side of a 2 of 3 key generation.
 */
export class Dkg {
    free(): void;
    [Symbol.dispose](): void;
    constructor(network: string, secret: string, room: string, seat_list: string);
    /**
     * Messages to send before anything has been received.
     */
    start(): string;
    /**
     * Feeds every message received so far (JSON from the coordinator's
     * inbox). Returns `{out, done}`; `done` is the share once finished.
     */
    step(received: string): string;
}

/**
 * Holds the Ironwood proving key. Building it is a one-off cost per page
 * load, so the app builds it once in a worker and keeps it.
 */
export class IronwoodProver {
    free(): void;
    [Symbol.dispose](): void;
    constructor();
    /**
     * Builds and proves an Ironwood bundle with `outputs` real outputs
     * (padded to at least two actions, like every wallet transaction).
     * Returns the proof size in bytes.
     */
    prove(outputs: number): number;
}

/**
 * A vault seen through one seat's share: its balance, and payouts that this
 * seat proposes and coordinates.
 */
export class VaultSeat {
    free(): void;
    [Symbol.dispose](): void;
    balance(): bigint;
    deposits(): number;
    height(): number;
    /**
     * `birthday` is the tree state at the vault's creation height.
     */
    constructor(network: string, share: string, birthday: string);
    /**
     * Builds and proves a payout of the whole vault to `to`, and starts
     * coordinating its signature with `cosigner` (a seat number). Returns
     * the amount paid.
     */
    propose(secret: string, room: string, to: string, cosigner: number): bigint;
    scan(blocks: string): void;
    spent(): string | undefined;
    start(): string;
    /**
     * Feeds the signing room's messages. `done` is the finished transaction
     * (`{txid, data}`) once both signatures are in.
     */
    step(received: string): string;
}

/**
 * A bidder's wallet.
 */
export class Wallet {
    free(): void;
    [Symbol.dispose](): void;
    address(index: number): string;
    balance(): bigint;
    /**
     * Seals a bid and builds the bond deposit. Returns the transaction as
     * JSON (`txid`, hex `data`, `fee`, `spent`). Call [`Wallet::sent`] once
     * it has been broadcast.
     */
    bid(terms: string, amount: bigint, refund_index: number): string;
    /**
     * Releases notes held by transactions that never got mined.
     */
    clearPending(): void;
    height(): number;
    /**
     * Restores a wallet saved with [`Wallet::save`].
     */
    static load(phrase: string, saved: string): Wallet;
    network(): string;
    /**
     * A new wallet that scans from right after `birthday` (tree state JSON
     * from the chain gateway).
     */
    constructor(network: string, phrase: string, birthday: string);
    notes(): string;
    /**
     * Everything but the keys, as JSON. Safe to keep in browser storage.
     */
    save(): string;
    /**
     * Scans a batch of consecutive blocks (JSON from the chain gateway).
     */
    scan(blocks: string): void;
    /**
     * A plain shielded payment, with an optional text memo.
     */
    send(to: string, value: bigint, memo: string): string;
    /**
     * Marks the notes a broadcast transaction spends, so they aren't used
     * twice before it is mined.
     */
    sent(built: string): void;
    /**
     * Spendable balance in zatoshi (not counting notes a pending
     * transaction spends).
     */
    spendable(): bigint;
}

/**
 * The auction id the terms commit to (hex).
 */
export function auctionId(terms: string): string;

/**
 * Whether a recovery phrase is a valid BIP 39 English phrase.
 */
export function checkPhrase(phrase: string): boolean;

/**
 * The escrow id the terms commit to (hex).
 */
export function escrowId(terms: string): string;

export function initThreadPool(num_threads: number): Promise<any>;

/**
 * A fresh 24 word recovery phrase.
 */
export function newPhrase(): string;

/**
 * A fresh seat key: the secret, hex. Keep it with the wallet.
 */
export function newSeatKey(): string;

/**
 * The quicknet round published at or just before a unix time.
 */
export function roundAt(unix: bigint): bigint;

/**
 * Unix time (seconds) at which a drand quicknet round is published.
 */
export function roundTime(round: bigint): bigint;

export function seatPublic(secret: string): string;

export function start(): void;

/**
 * The vault deposit address encoded in a unified viewing key.
 */
export function vaultAddress(network: string, ufvk: string): string;

/**
 * Checks a beacon signature against the quicknet group key.
 */
export function verifyBeacon(round: bigint, signature: string): boolean;

/**
 * Builds the Ironwood proving key if it isn't built yet. Takes several
 * seconds; the app calls it in the background right after loading.
 */
export function warmProver(): void;

export class wbg_rayon_PoolBuilder {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    build(): void;
    numThreads(): number;
    receiver(): number;
}

export function wbg_rayon_start_worker(receiver: number): void;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly __wbg_audit_free: (a: number, b: number) => void;
    readonly __wbg_dkg_free: (a: number, b: number) => void;
    readonly __wbg_ironwoodprover_free: (a: number, b: number) => void;
    readonly __wbg_vaultseat_free: (a: number, b: number) => void;
    readonly __wbg_wallet_free: (a: number, b: number) => void;
    readonly __wbg_wbg_rayon_poolbuilder_free: (a: number, b: number) => void;
    readonly auctionId: (a: number, b: number) => [number, number, number, number];
    readonly audit_addTx: (a: number, b: number, c: number, d: number, e: number) => [number, number];
    readonly audit_height: (a: number) => number;
    readonly audit_new: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly audit_report: (a: number, b: number, c: number) => [number, number, number, number];
    readonly audit_scan: (a: number, b: number, c: number) => [number, number];
    readonly audit_vaultAddress: (a: number) => [number, number];
    readonly audit_wanted: (a: number) => [number, number];
    readonly checkPhrase: (a: number, b: number) => number;
    readonly dkg_new: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number];
    readonly dkg_start: (a: number) => [number, number];
    readonly dkg_step: (a: number, b: number, c: number) => [number, number, number, number];
    readonly escrowId: (a: number, b: number) => [number, number, number, number];
    readonly initThreadPool: (a: number) => any;
    readonly ironwoodprover_new: () => number;
    readonly ironwoodprover_prove: (a: number, b: number) => [number, number, number];
    readonly newPhrase: () => [number, number, number, number];
    readonly newSeatKey: () => [number, number];
    readonly roundAt: (a: bigint) => bigint;
    readonly roundTime: (a: bigint) => bigint;
    readonly seatPublic: (a: number, b: number) => [number, number, number, number];
    readonly start: () => void;
    readonly vaultAddress: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly vaultseat_balance: (a: number) => bigint;
    readonly vaultseat_deposits: (a: number) => number;
    readonly vaultseat_height: (a: number) => number;
    readonly vaultseat_new: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly vaultseat_propose: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [bigint, number, number];
    readonly vaultseat_scan: (a: number, b: number, c: number) => [number, number];
    readonly vaultseat_spent: (a: number) => [number, number];
    readonly vaultseat_start: (a: number) => [number, number];
    readonly vaultseat_step: (a: number, b: number, c: number) => [number, number, number, number];
    readonly verifyBeacon: (a: bigint, b: number, c: number) => number;
    readonly wallet_address: (a: number, b: number) => [number, number];
    readonly wallet_balance: (a: number) => bigint;
    readonly wallet_bid: (a: number, b: number, c: number, d: bigint, e: number) => [number, number, number, number];
    readonly wallet_clearPending: (a: number) => void;
    readonly wallet_height: (a: number) => number;
    readonly wallet_load: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly wallet_network: (a: number) => [number, number];
    readonly wallet_new: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly wallet_notes: (a: number) => [number, number];
    readonly wallet_save: (a: number) => [number, number];
    readonly wallet_scan: (a: number, b: number, c: number) => [number, number];
    readonly wallet_send: (a: number, b: number, c: number, d: bigint, e: number, f: number) => [number, number, number, number];
    readonly wallet_sent: (a: number, b: number, c: number) => [number, number];
    readonly wallet_spendable: (a: number) => bigint;
    readonly warmProver: () => void;
    readonly wbg_rayon_poolbuilder_build: (a: number) => void;
    readonly wbg_rayon_poolbuilder_numThreads: (a: number) => number;
    readonly wbg_rayon_poolbuilder_receiver: (a: number) => number;
    readonly wbg_rayon_start_worker: (a: number) => void;
    readonly rustsecp256k1_v0_10_0_context_create: (a: number) => number;
    readonly rustsecp256k1_v0_10_0_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_10_0_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_10_0_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly memory: WebAssembly.Memory;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_thread_destroy: (a?: number, b?: number, c?: number) => void;
    readonly __wbindgen_start: (a: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput, memory?: WebAssembly.Memory, thread_stack_size?: number }} module - Passing `SyncInitInput` directly is deprecated.
 * @param {WebAssembly.Memory} memory - Deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput, memory?: WebAssembly.Memory, thread_stack_size?: number } | SyncInitInput, memory?: WebAssembly.Memory): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput>, memory?: WebAssembly.Memory, thread_stack_size?: number }} module_or_path - Passing `InitInput` directly is deprecated.
 * @param {WebAssembly.Memory} memory - Deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput>, memory?: WebAssembly.Memory, thread_stack_size?: number } | InitInput | Promise<InitInput>, memory?: WebAssembly.Memory): Promise<InitOutput>;
