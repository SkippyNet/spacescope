#!/usr/bin/env bash
# Builds dist/spacescope-amd64.deb.
#   $1  optional Rust target triple the binaries were built for
# For a package that also installs on older releases, run this on the oldest
# distribution you want to support (CI uses Ubuntu 22.04).
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo-deb >/dev/null || cargo install cargo-deb --locked
target="${1:-}"
mkdir -p dist
rm -f dist/*.deb
if [ -n "$target" ]; then
  cargo build --release --target "$target"
  cargo deb -p spacescope --no-build --target "$target" --output dist/spacescope-amd64.deb
else
  cargo build --release
  cargo deb -p spacescope --no-build --output dist/spacescope-amd64.deb
fi
ls -lh dist/*.deb
