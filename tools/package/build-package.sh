#!/usr/bin/env bash
# Builds packaging/PKGBUILD from the current HEAD in a fresh build directory.
# Refuses a dirty tree so the package always corresponds to one commit.
# Usage: tools/package/build-package.sh [extra makepkg args]
set -euo pipefail

repo="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo"
if [[ -n "$(git status --porcelain)" ]]; then
  echo "refusing to package: working tree is dirty" >&2
  exit 1
fi

commit="$(git rev-parse HEAD)"
out="${YTM_GLASS_PKG_OUT:-$repo/packaging/out}"
build="$(mktemp -d "${TMPDIR:-/tmp}/ytm-glass-pkg.XXXXXX")"
trap 'rm -rf "$build"' EXIT
mkdir -p "$out"
cp packaging/PKGBUILD "$build/"

# Reuse the Electron zip from the npm cache when its checksum matches the PKGBUILD.
zip_name="electron-v45.4.1-linux-x64.zip"
for cached in "$HOME"/.cache/electron/*/"$zip_name"; do
  [[ -f "$cached" ]] || continue
  if grep -q "$(sha256sum "$cached" | cut -d' ' -f1)" packaging/PKGBUILD; then
    ln -s "$cached" "$build/$zip_name"
    break
  fi
done

makepkg_cmd=(makepkg)
if cpus="$("$HOME/.local/bin/workload-cpus" 2>/dev/null)" && [[ -n "$cpus" ]]; then
  makepkg_cmd=(taskset -c "$cpus" makepkg)
fi
cd "$build"
YTM_GLASS_REPO="$repo" YTM_GLASS_COMMIT="$commit" PKGDEST="$out" \
  "${makepkg_cmd[@]}" --cleanbuild --force "$@"
echo "commit: $commit"
ls -la "$out"
