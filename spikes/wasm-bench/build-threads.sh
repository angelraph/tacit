#!/bin/sh
# Builds the multithreaded browser prover. Threads in WebAssembly need the
# standard library rebuilt with atomics, which only nightly can do.
#
# Current rustc no longer makes memory shared just because atomics are on,
# so ask the linker for shared, imported memory (capped at 2 GiB) and export
# the thread-local storage symbols wasm-bindgen uses to start each thread.
set -e
cd "$(dirname "$0")/../.."
LINK="-C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=2147483648"
LINK="$LINK -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size"
LINK="$LINK -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base"
RUSTFLAGS="-C target-feature=+atomics,+bulk-memory $LINK" \
  cargo +nightly build --release -p tacit-wasm --features threads \
  --target wasm32-unknown-unknown -Z build-std=panic_abort,std \
  --target-dir target/wasm-threads
wasm-bindgen target/wasm-threads/wasm32-unknown-unknown/release/tacit_wasm.wasm \
  --target web --out-dir spikes/wasm-bench/pkg-threads
