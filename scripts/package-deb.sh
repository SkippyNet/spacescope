#!/usr/bin/env bash
# Builds dist/spacescope-amd64.deb.
# For a package that also installs on older releases, run this on the oldest
# distribution you want to support (CI uses Ubuntu 22.04).
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo-deb >/dev/null || cargo install cargo-deb --locked
cargo build --release
mkdir -p dist
rm -f dist/*.deb
cargo deb -p spacescope --no-build --output dist/spacescope-amd64.deb
ls -lh dist/*.deb
