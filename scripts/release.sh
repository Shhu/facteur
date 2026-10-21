#!/usr/bin/env bash
# Publishes a facteur release from WSL:
#
#   scripts/release.sh 0.2.0                     → Windows exe, signed for updates, and the
#                                                  Linux Flatpak bundle
#   scripts/release.sh 0.2.0 --android           → plus the Android APK, signed with
#                                                  ~/.tauri/facteur-android.properties
#   scripts/release.sh 0.2.0 --notes notes.md    → release notes (else a one-line default)
#
# The exe is signed with ~/.tauri/facteur.key (kept out of the repository). The website workflow
# copies the latest release's files to https://shu-app.com/facteur/download/, a stable address
# for the updater and the download page.
set -euo pipefail

start_dir="$PWD"
repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

usage() { echo "usage: scripts/release.sh X.Y.Z [--android] [--notes file]" >&2; exit 1; }
version="${1:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || usage
shift
android=0
notes=''
while [[ $# -gt 0 ]]; do
  case "$1" in
    --android) android=1 ;;
    --notes)
      [[ $# -ge 2 ]] || usage
      # Relative to where the script was started from, not to the repository.
      notes="$(cd "$start_dir" && realpath -e -- "$2" 2>/dev/null)" || { echo "notes file not found: $2" >&2; exit 1; }
      [[ -f "$notes" && -r "$notes" ]] || { echo "notes file not readable: $notes" >&2; exit 1; }
      shift ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
  shift
done

tag="v$version"
site='https://shu-app.com/facteur/download'
key="$HOME/.tauri/facteur.key"
[[ -f "$key" && -f "$key.password" ]] || { echo "missing update signing key: $key (and $key.password)" >&2; exit 1; }
[[ -z "$(git status --porcelain)" ]] || { echo "the working tree has changes: commit them first" >&2; exit 1; }
[[ "$(git branch --show-current)" == main ]] || { echo "not on main" >&2; exit 1; }
git rev-parse -q --verify "refs/tags/$tag" > /dev/null && { echo "tag $tag already exists" >&2; exit 1; }
# Origin must not be ahead (an edit pushed from elsewhere): the atomic push at the end would be
# refused after every build. Local commits not yet pushed are fine.
git fetch -q origin main || { echo "cannot fetch origin main" >&2; exit 1; }
git merge-base --is-ancestor origin/main HEAD \
  || { echo "origin/main has commits that local main lacks: pull them first" >&2; exit 1; }
git ls-remote -q --exit-code --tags origin "refs/tags/$tag" > /dev/null && { echo "tag $tag already exists on origin" >&2; exit 1; }

# Only forward: never the same version again, nor an older one.
newer() { [[ "$1" != "$2" && "$(printf '%s\n' "$1" "$2" | sort -V | tail -n 1)" == "$1" ]]; }
current="$(python3 -c "import json; print(json.load(open('src-tauri/tauri.conf.json'))['version'])")"
latest_tag="$(git tag -l 'v[0-9]*.[0-9]*.[0-9]*' | sed 's/^v//' | sort -V | tail -n 1)"
for previous in "$current" "$latest_tag"; do
  [[ -z "$previous" ]] || newer "$version" "$previous" \
    || { echo "version $version is not later than $previous (tauri.conf.json $current, latest tag ${latest_tag:-none})" >&2; exit 1; }
done

# The OAuth client overrides (FACTEUR_*, scripts/build-windows.sh) are for local builds: a
# release is built with the identifiers published in crates/core/src/oauth.rs.
if [[ -f .env ]] && grep -qE '^[[:space:]]*(export[[:space:]]+)?FACTEUR_[A-Z_]+=' .env; then
  echo ".env defines FACTEUR_* variables: move them away for a release" >&2; exit 1
fi
# (grep -q on a pipe could kill its writer early: pipefail would then hide the match.)
if grep -q '^FACTEUR_' <<< "$(compgen -e)"; then
  echo "FACTEUR_* variables are set in this shell: unset them for a release" >&2; exit 1
fi
if grep -q '^FACTEUR_' <<< "$(cmd.exe /c 'set FACTEUR_' 2>/dev/null | tr -d '\r' || true)"; then
  echo "FACTEUR_* variables are set in the Windows environment: remove them for a release" >&2; exit 1
fi

if [[ $android == 1 ]]; then
  export JAVA_HOME="$HOME/Android/jdk-dl/jdk-17.0.20.1+1" ANDROID_HOME="$HOME/Android/Sdk"
  export NDK_HOME="$ANDROID_HOME/ndk/27.3.13750724"
  signing="$HOME/.tauri/facteur-android.properties"
  [[ -f "$signing" ]] || { echo "missing Android signing key: $signing" >&2; exit 1; }
  for dir in "$JAVA_HOME" "$ANDROID_HOME" "$NDK_HOME"; do
    [[ -d "$dir" ]] || { echo "missing Android toolchain: $dir" >&2; exit 1; }
  done
fi
gh auth status > /dev/null

# ── Rollback ─────────────────────────────────────────────────────────────
# How far the release got, for the exit trap.
stage=checks
out="$repo/target/release-$version"
on_exit() {
  local status=$?
  [[ $status -ne 0 ]] || return 0
  case "$stage" in
    bumped)
      git checkout -q -- src-tauri/tauri.conf.json Cargo.toml Cargo.lock flatpak/com.shu_app.facteur.metainfo.xml \
        flatpak/cargo-sources.json flatpak/node-sources.json
      echo "release.sh failed: version change undone." >&2 ;;
    committed)
      cat >&2 <<EOF

release.sh failed after the release commit was made, before it was pushed.
  retry:  git tag -a $tag -m "facteur $version"   (if missing)
          git push --atomic origin main $tag
  undo:   git tag -d $tag; git reset --hard HEAD~1
EOF
      ;;
    pushed)
      local notes_arg="--notes \"facteur $version\""
      [[ -z "$notes" ]] || notes_arg="--notes-file \"$notes\""
      cat >&2 <<EOF

release.sh failed after $tag was pushed: the GitHub release is missing.
The files are in $out.
  retry:  gh release create $tag $out/* --title "facteur $version" $notes_arg
  undo:   git push --delete origin $tag && git tag -d $tag   (then revert the release commit)
EOF
      ;;
  esac
}
trap on_exit EXIT

# ── Version ──────────────────────────────────────────────────────────────
stage=bumped
python3 - "$version" <<'EOF'
import json, re, sys
version = sys.argv[1]
path = 'src-tauri/tauri.conf.json'
conf = json.load(open(path))
conf['version'] = version
open(path, 'w').write(json.dumps(conf, indent=2, ensure_ascii=False) + '\n')
path = 'Cargo.toml'
text = open(path).read()
text, n = re.subn(r'(\[workspace\.package\][^\[]*?\nversion = ")[^"]+(")', rf'\g<1>{version}\g<2>', text, count=1)
assert n == 1, 'workspace version not found in Cargo.toml'
open(path, 'w').write(text)
# Software centres (GNOME Software, Discover) show the installed version from here.
import datetime
path = 'flatpak/com.shu_app.facteur.metainfo.xml'
text = open(path).read()
entry = f'    <release version="{version}" date="{datetime.date.today().isoformat()}"/>\n'
text, n = re.subn(r'(  <releases>\n)', lambda m: m.group(1) + entry, text, count=1)
assert n == 1, 'no <releases> in the metainfo'
open(path, 'w').write(text)
EOF
cargo update --workspace --offline -q

# ── Windows exe, signed ─────────────────────────────────────────────────
# A running facteur.exe locks the build's output.
echo "Waiting for facteur to be closed…"
powershell.exe -NoProfile -Command "Wait-Process -Name facteur -ErrorAction SilentlyContinue" || true
scripts/build-windows.sh
win_home="$(wslpath "$(cmd.exe /c 'echo %USERPROFILE%' 2>/dev/null | tr -d '\r')")"
exe="$win_home/dev/facteur-build/target/release/facteur.exe"
[[ -f "$exe" ]] || { echo "facteur.exe missing: $exe" >&2; exit 1; }

rm -rf "$out" && mkdir -p "$out"
# The updater downloads the versioned name: a CDN cache cannot serve it with an older exe.
versioned="facteur-$version.exe"
cp "$exe" "$out/$versioned"
# The key file and its password never appear on a command line (nor in pnpm's echo of it).
# --app-version binds the signature to the version (its trusted comment, checked by the
# updater, crates/core/src/update.rs): an older exe cannot be replayed as a newer version.
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat "$key.password")" \
  node_modules/.bin/tauri signer sign -f "$key" --app-version "$version" "$out/$versioned" > /dev/null
[[ -f "$out/$versioned.sig" ]] || { echo "signing failed" >&2; exit 1; }
grep -qF "version:$version" <<< "$(base64 -d "$out/$versioned.sig")" \
  || { echo "the signature does not carry version:$version" >&2; exit 1; }
# The site's download link.
cp "$out/$versioned" "$out/facteur.exe"

# ── Android (optional) ──────────────────────────────────────────────────
if [[ $android == 1 ]]; then
  FACTEUR_ANDROID_SIGNING="$signing" npx -y pnpm@11.1.1 tauri android build --apk --target aarch64
  cp src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk "$out/facteur-$version.apk"
fi

# ── Linux Flatpak ───────────────────────────────────────────────────────
# A bundle installed by hand, the site links the stable name; Flathub builds its own from the tag
# (scripts/flathub-manifest.sh). Both build offline from the source lists, regenerated here so
# that the tag carries lists matching its lockfiles.
scripts/flatpak-sources.sh
scripts/build-flatpak.sh
cp "target/flatpak/facteur-$version.flatpak" "$out/"
cp "$out/facteur-$version.flatpak" "$out/facteur.flatpak"

# ── update.json, read by the updater of installed copies ────────────────
python3 - "$version" "$site/$versioned" "$out/$versioned.sig" "$out/update.json" "$notes" <<'EOF'
import datetime, json, sys
version, url, sig, out, notes = sys.argv[1:]
text = open(notes).read().strip() if notes else f'facteur {version}'
json.dump({
    'version': version,
    'notes': text,
    'pub_date': datetime.datetime.now(datetime.timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ'),
    'platforms': {'windows-x86_64': {'signature': open(sig).read().strip(), 'url': url}},
}, open(out, 'w'), indent=2)
EOF

rm "$out/$versioned.sig"

# ── Publish ──────────────────────────────────────────────────────────────
git commit -qam "Release $tag"
stage=committed
git tag -a "$tag" -m "facteur $version"
git push -q --atomic origin main "$tag"
stage=pushed
notes_args=(--notes "facteur $version")
[[ -n "$notes" ]] && notes_args=(--notes-file "$notes")
gh release create "$tag" "$out"/* --title "facteur $version" "${notes_args[@]}"
stage=published
# The shu-app.com Flatpak repository (facteur and Atelier), rebuilt from the latest releases.
shared="$HOME/dev/shu-app/flatpak/publish.sh"
if [[ -x "$shared" ]]; then
  "$shared" || echo "Flatpak repository not updated: run $shared again" >&2
else
  echo "Flatpak repository not updated: $shared missing (clone Shhu/shhu.github.io to ~/dev/shu-app)" >&2
fi
echo
echo "Release $tag published; the website copies its files within a few minutes:"
echo "  https://shu-app.com/facteur/"
