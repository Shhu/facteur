#!/usr/bin/env bash
# Builds facteur.exe with the Windows toolchain, from WSL.
#
#   scripts/build-windows.sh            → target\release\facteur.exe (standalone)
#   scripts/build-windows.sh --setup    → plus an NSIS installer
#   scripts/build-windows.sh --perf     → with the performance tools (Ctrl+Shift+P), local only
#
# The working tree is copied to the Windows disk: building through \\wsl.localhost is slow and
# unreliable.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
win_home="$(wslpath "$(cmd.exe /c 'echo %USERPROFILE%' 2>/dev/null | tr -d '\r')")"
dest="$win_home/dev/facteur-build"
pnpm='npx -y pnpm@11.1.1'

bundle='--no-bundle'
features='vulkan'
for arg in "$@"; do
  case "$arg" in
    --setup) bundle='--bundles nsis' ;;
    # Local builds only: scripts/release.sh never passes it.
    --perf) features='vulkan,perf' ;;
  esac
done

mkdir -p "$dest"
# Tracked and untracked non-ignored files that still exist. Files gone from the repository are
# removed from the copy's source folders, so a stale file (an old page after a move) is not built.
files="$(mktemp)"
(cd "$repo" && git ls-files -z --cached --others --exclude-standard \
  | while IFS= read -r -d '' f; do [[ -e "$f" ]] && printf '%s\0' "$f"; done) > "$files"
(cd "$repo" && tar --null -T "$files" -cf -) | tar -xf - -C "$dest"
tr '\0' '\n' < "$files" > "$files.lines"
# grep usually finds nothing, which is not an error.
(cd "$dest" && find ui/app ui/i18n crates src-tauri/src src-tauri/capabilities site scripts -type f 2>/dev/null) \
  | { grep -vxFf "$files.lines" || true; } | (cd "$dest" && xargs -r -d '\n' rm -f --)
rm -f "$files" "$files.lines"
# pnpm is not installed on Windows: the UI is generated below, not by beforeBuildCommand.
echo '{ "build": { "beforeBuildCommand": "" } }' > "$dest/src-tauri/tauri.exe.conf.json"

cd "$dest"
# Optional FACTEUR_* overrides of the OAuth2 client ids in crates/core/src/oauth.rs (fork, test
# project), read from .env. Passed through WSLENV so cmd never parses the values.
if [[ -f "$repo/.env" ]]; then
  while IFS='=' read -r key value; do
    [[ "$key" =~ ^FACTEUR_[A-Z_]+$ ]] || continue
    export "$key=$value"
    export WSLENV="${WSLENV:+$WSLENV:}$key"
  done < "$repo/.env"
fi
# The local AI's llama.cpp is built with CMake (Visual Studio's Build Tools have one) and needs
# libclang (LLVM: winget install LLVM.LLVM).
llvm='/mnt/c/Program Files/LLVM/bin'
[[ -f "$llvm/libclang.dll" ]] || { echo "LLVM missing (libclang.dll): winget install LLVM.LLVM" >&2; exit 1; }
export LIBCLANG_PATH="$(wslpath -w "$llvm")"
export WSLENV="${WSLENV:+$WSLENV:}LIBCLANG_PATH"
# The local AI on the graphics card: llama.cpp's Vulkan backend is built with the Vulkan SDK
# (winget install KhronosGroup.VulkanSDK); the exe itself needs only the graphics driver.
vulkan="$(ls -d /mnt/c/VulkanSDK/*/ 2>/dev/null | sort -V | tail -1)"
[[ -n "$vulkan" && -f "$vulkan/Lib/vulkan-1.lib" ]] || { echo "Vulkan SDK missing: winget install KhronosGroup.VulkanSDK" >&2; exit 1; }
export VULKAN_SDK="$(wslpath -w "${vulkan%/}")"
export WSLENV="$WSLENV:VULKAN_SDK"
vs_cmake="$(ls -d /mnt/c/Program\ Files*/Microsoft\ Visual\ Studio/*/*/Common7/IDE/CommonExtensions/Microsoft/CMake/CMake/bin/cmake.exe 2>/dev/null | head -1)"
if [[ -n "$vs_cmake" ]]; then
  export CMAKE="$(wslpath -w "$vs_cmake")"
  export WSLENV="$WSLENV:CMAKE"
fi
win() { cmd.exe /c "$*" 2>&1 | tr -d '\r'; }
win "$pnpm install --frozen-lockfile"
# A failed generate must not leave the previous UI to be bundled into the new exe.
rm -rf "$dest/ui/.output"
log="$(mktemp)"
if ! win "$pnpm --dir ui generate" > "$log"; then
  cat "$log" >&2
  rm -f "$log"
  echo "nuxt generate failed" >&2
  exit 1
fi
grep -E '✔|ERROR' "$log" || true
rm -f "$log"
[[ -f "$dest/ui/.output/public/index.html" ]] || { echo "nuxt generate wrote no ui/.output/public/index.html" >&2; exit 1; }
win "$pnpm tauri build $bundle --features $features --config src-tauri\\tauri.exe.conf.json" | grep -vE '^\s+Compiling '

echo
echo "Executable: $(wslpath -w "$dest/target/release/facteur.exe")"
if [[ "$bundle" != '--no-bundle' ]]; then
  echo "Installer: $(wslpath -w "$dest/target/release/bundle/nsis")"
fi
