#!/usr/bin/env bash
# Regenerates flatpak/cargo-sources.json and flatpak/node-sources.json, the crates and packages the
# Flatpak build downloads beforehand (Flathub builds offline). Run after any change to Cargo.lock
# or pnpm-lock.yaml; release.sh runs it too. The generators (flatpak-builder-tools, pinned) and
# their Python packages live in target/flatpak-tools.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
tools="$repo/target/flatpak-tools"
commit=74697c75b630d7330e77250fc13cb5ea688d9479

if [[ "$(git -C "$tools/fbt" rev-parse HEAD 2>/dev/null)" != "$commit" ]]; then
  rm -rf "$tools"
  mkdir -p "$tools"
  git clone -q https://github.com/flatpak/flatpak-builder-tools.git "$tools/fbt"
  git -C "$tools/fbt" checkout -q "$commit"
  python3 -m venv "$tools/venv"
  "$tools/venv/bin/pip" install -q aiohttp tomlkit pyyaml "$tools/fbt/node"
fi

"$tools/venv/bin/python" -I "$tools/fbt/cargo/flatpak-cargo-generator.py" Cargo.lock -o flatpak/cargo-sources.json
# pnpm 11 (see the pnpm archive in the manifest): its store layout.
"$tools/venv/bin/flatpak-node-generator" pnpm pnpm-lock.yaml --pnpm-store-version v11 \
  -o flatpak/node-sources.json > /dev/null
echo "flatpak/cargo-sources.json and flatpak/node-sources.json regenerated"
