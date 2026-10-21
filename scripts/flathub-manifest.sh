#!/usr/bin/env bash
# Writes target/flathub/: the manifest Flathub builds (the working tree source replaced by the
# release's git tag and commit) with its two source lists, ready to copy into the
# github.com/flathub/com.shu_app.facteur repository.
#
#   scripts/flathub-manifest.sh v0.8.2
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
tag="${1:?usage: scripts/flathub-manifest.sh vX.Y.Z}"
commit="$(git rev-list -n 1 "$tag")" || { echo "unknown tag: $tag" >&2; exit 1; }
out="$repo/target/flathub"
mkdir -p "$out"

python3 - "$tag" "$commit" "$out" <<'EOF'
import re, sys
tag, commit, out = sys.argv[1:]
text = open('flatpak/com.shu_app.facteur.yml').read()
source = (
    '      - type: git\n'
    '        url: https://github.com/Shhu/facteur.git\n'
    f'        tag: {tag}\n'
    f'        commit: {commit}\n'
)
text, n = re.subn(r'      # SOURCE:.*\n      - type: dir\n(        .*\n)+?(?=      - type: archive)', source, text, count=1)
assert n == 1, 'working tree source not found in the manifest'
open(f'{out}/com.shu_app.facteur.yml', 'w').write(text)
EOF
cp flatpak/cargo-sources.json flatpak/node-sources.json "$out/"
echo "Flathub manifest for $tag ($commit): $out"
