#!/usr/bin/env bash
# Builds target/flatpak/facteur-X.Y.Z.flatpak locally (no CI). Install it with
# `flatpak install --user facteur-X.Y.Z.flatpak`; there is no update channel yet.
# Needs once: flatpak install --user flathub org.gnome.Sdk//51 org.freedesktop.Sdk.Extension.rust-stable//26.08
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
pnpm='npx -y pnpm@11.1.1'
out="$repo/target/flatpak"
version="$(python3 -c "import json; print(json.load(open('src-tauri/tauri.conf.json'))['version'])")"
mkdir -p "$out"

# The UI is generated here: the sandbox only compiles Rust. A failed generate must not leave the
# previous UI to be bundled.
$pnpm install --frozen-lockfile > /dev/null
rm -rf ui/.output
$pnpm --dir ui generate > "$out.log" 2>&1 || { cat "$out.log" >&2; echo "nuxt generate failed" >&2; exit 1; }
rm -f "$out.log"
[[ -f ui/.output/public/index.html ]] || { echo "nuxt generate wrote no ui/.output/public/index.html" >&2; exit 1; }

# rofiles-fuse is not always usable (WSL): plain copies instead.
flatpak-builder --user --force-clean --disable-rofiles-fuse \
  --state-dir "$out/state" --repo "$out/repo" "$out/build" flatpak/dev.facteur.mail.yml \
  2>&1 | grep -vE '^\s+(Compiling|Downloaded|Downloading) ' || exit "${PIPESTATUS[0]}"
# The runtime repo lets `flatpak install --user` fetch GNOME 51 from Flathub (offering to add it as
# a remote) when the user installation does not have it yet.
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
  "$out/repo" "$out/facteur-$version.flatpak" dev.facteur.mail

echo
echo "Flatpak: $out/facteur-$version.flatpak"
