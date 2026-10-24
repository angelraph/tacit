#!/bin/sh
# Gathers prebuilt binaries and the web app into deploy/out.
set -e
cd "$(dirname "$0")/.."
TARGET="${TACIT_LINUX_TARGET:-target}"
OUT=deploy/out
rm -rf "$OUT" && mkdir -p "$OUT/bin" "$OUT/web"
cp "$TARGET/release/tacit" "$TARGET/release/tacit-coordinator" "$OUT/bin/"
cp -r web/* "$OUT/web/"
rm -f "$OUT/web/serve.py" "$OUT/web/build.sh" "$OUT/web/vercel.json"
cp deploy/Dockerfile deploy/start.sh "$OUT/"
du -sh "$OUT"
