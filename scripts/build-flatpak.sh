#!/usr/bin/env bash
# Builds target/flatpak/facteur-X.Y.Z.flatpak locally (no CI), offline as Flathub does. Install it
# with `flatpak install --user facteur-X.Y.Z.flatpak`.
# Needs once: flatpak install --user flathub org.gnome.Sdk//51 \
#   org.freedesktop.Sdk.Extension.rust-stable//26.08 org.freedesktop.Sdk.Extension.node24//26.08
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
out="$repo/target/flatpak"
version="$(python3 -c "import json; print(json.load(open('src-tauri/tauri.conf.json'))['version'])")"
mkdir -p "$out"
# A fresh repository each time: only this build in it (what the Flathub linter checks).
rm -rf "$out/repo"

# rofiles-fuse is not always usable (WSL): plain copies instead.
flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --state-dir "$out/state" --repo "$out/repo" "$out/build" flatpak/com.shu_app.facteur.yml \
  2>&1 | grep -vE '^\s+(Compiling|Downloaded|Downloading) ' || exit "${PIPESTATUS[0]}"
# The runtime repo lets `flatpak install --user` fetch GNOME 51 from Flathub (offering to add it as
# a remote) when the user installation does not have it yet.
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$out/repo" "$out/facteur-$version.flatpak" com.shu_app.facteur

echo
echo "Flatpak: $out/facteur-$version.flatpak"
