#!/bin/sh
# Builds the WebAssembly the web app loads into web/pkg (single threaded,
# for browsers without cross-origin isolation) and web/pkg-threads
# (multithreaded proving).
#
# Needs clang for the one C dependency (secp256k1, pulled in by
# zcash_primitives), the wasm32 target on stable and on nightly-2026-01-01
# with rust-src, and wasm-bindgen-cli 0.2.129. On Windows, run it in WSL.
set -e
cd "$(dirname "$0")/.."
TARGET_DIR="${TACIT_WASM_TARGET:-target/web}"
export CC_wasm32_unknown_unknown="${CC_wasm32_unknown_unknown:-clang}"
export AR_wasm32_unknown_unknown="${AR_wasm32_unknown_unknown:-llvm-ar}"
RAND='--cfg getrandom_backend="wasm_js" --cfg zcash_unstable="nu7"'

if [ "$1" != "threads-only" ]; then
  RUSTFLAGS="$RAND" cargo build --release -p tacit-wasm --target wasm32-unknown-unknown --target-dir "$TARGET_DIR/single"
  wasm-bindgen "$TARGET_DIR/single/wasm32-unknown-unknown/release/tacit_wasm.wasm" --target web --out-dir web/pkg
fi

# Threads need the standard library rebuilt with atomics (nightly), shared
# imported memory, and the TLS exports wasm-bindgen uses to start workers.
LINK="-C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=2147483648"
LINK="$LINK -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size"
LINK="$LINK -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base"
RUSTFLAGS="$RAND -C target-feature=+atomics,+bulk-memory $LINK" \
  cargo +nightly-2026-01-01 build --release -p tacit-wasm --features threads \
  --target wasm32-unknown-unknown -Z build-std=panic_abort,std --target-dir "$TARGET_DIR/threads"
wasm-bindgen "$TARGET_DIR/threads/wasm32-unknown-unknown/release/tacit_wasm.wasm" --target web --out-dir web/pkg-threads
ls -la web/pkg web/pkg-threads 2>/dev/null | grep wasm
