#!/usr/bin/env bash
# Builds the public source tree from the committed HEAD into an empty directory, scans it for
# CJK text and sensitive data, and only if the scan is clean makes it a fresh git repository
# with one commit, "Initial public release".
#
# Usage: tools/release/export-public.sh TARGET_DIR
#
# Only the allowlisted tracked paths below are exported; internal/, local agent instruction
# files, packaging/out/, node_modules/ and target/ never are. Owner-specific patterns (names,
# handles, hostnames) come from internal/release/sensitive-patterns.txt, which stays local and
# must exist. Every hit is printed before the script fails.
#
# The commit is authored as PUBLIC_AUTHOR ("Name <email>"), by default the project-neutral
# "YTM Glass <noreply@ytm-glass.invalid>", never the local git identity.
set -euo pipefail
export LC_ALL=C.UTF-8

repo="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
patterns="$repo/internal/release/sensitive-patterns.txt"

# Tracked paths that make up the public tree.
ALLOW=(
  .gitignore Cargo.toml Cargo.lock package.json pnpm-lock.yaml pnpm-workspace.yaml
  rust-toolchain.toml tsconfig.base.json justfile README.md CHANGELOG.md LICENSE
  apps bridge config crates docs fixtures packages packaging tests tools ui
)
# Never exported, even if they appear under an allowlisted path.
DENY=(internal MCP AGENTS.md CLAUDE.md START_HERE.md packaging/out '**/node_modules' '**/target')

# Files that legitimately contain Chinese: the zh-CN UI translation, script detection for lyric
# and title language, multibyte lyric test data, localized page fixtures in the catalog tests,
# and the desktop entry's localized name.
CJK_ALLOWED=(
  # The Simplified Chinese translation, its language name and the tests of the Chinese UI.
  ui/src/lib/i18n/zh-CN.ts
  ui/src/lib/i18n/index.svelte.ts
  ui/src/app/__tests__/app.test.ts
  # Formatting and lyrics handling of CJK text.
  ui/src/lib/components/format.ts
  crates/core/src/control/lyrics.rs
  # Invented mixed-script titles and lyrics that exercise CJK rendering, search and input.
  ui/src/fixtures/synthetic.ts
  ui/src/lib/__tests__/lib.test.ts
  packages/protocol/src/fake/index.ts
  tools/probes/runtime/glass-test.html
  # Matching YouTube Music's own Chinese page labels, and the IME composition check.
  crates/ytm-data/src/probe/tests.rs
  tools/qa/t17-write.mjs
  tools/qa/gates/g1.mjs
  # The desktop entry's localized name.
  packaging/local.ytmglass.Desktop.desktop
)
# Third-party license texts carry their authors' e-mail addresses.
EMAIL_ALLOWED_PREFIX=(packaging/licenses/)

CJK='[\x{3000}-\x{303F}\x{3040}-\x{30FF}\x{3400}-\x{4DBF}\x{4E00}-\x{9FFF}\x{AC00}-\x{D7AF}\x{F900}-\x{FAFF}\x{FF00}-\x{FFEF}]'
HOME_PATH='/home/(?!(USER|user|you)\b)[A-Za-z0-9._-]+|/Users/(?!(USER|user|you)\b)[A-Za-z0-9._-]+'
# Not preceded by ':' or '/', so URL userinfo in tests (https://user:pass@host) is not taken for one.
EMAIL='(?<![:/A-Za-z0-9._%+-])[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}'
EMAIL_PLACEHOLDER='noreply|no-reply|@example\.(com|org|net)$|\.(invalid|test|example)$'

die() { echo "export-public: $*" >&2; exit 1; }

[[ $# -eq 1 ]] || die "usage: tools/release/export-public.sh TARGET_DIR"
target="$1"
if [[ -e "$target" ]]; then
  [[ -d "$target" ]] || die "$target exists and is not a directory"
  [[ -z "$(ls -A "$target")" ]] || die "refusing a non-empty target: $target"
fi
[[ -f "$patterns" ]] || die "missing $patterns (owner-specific patterns, kept local)"
mapfile -t owner < <(grep -vE '^\s*(#|$)' "$patterns")
[[ ${#owner[@]} -gt 0 ]] || die "$patterns lists no patterns"
if ! git -C "$repo" diff --quiet HEAD -- "${ALLOW[@]}"; then
  echo "export-public: note: uncommitted changes are not exported; HEAD is" >&2
fi

mkdir -p "$target"
target="$(cd "$target" && pwd)"
pathspec=("${ALLOW[@]}")
for path in "${DENY[@]}"; do pathspec+=(":(exclude,glob)$path"); done
git -C "$repo" archive --format=tar HEAD -- "${pathspec[@]}" | tar -x -C "$target"
commit="$(git -C "$repo" rev-parse HEAD)"
echo "exported $(find "$target" -type f | wc -l) files from $commit"

hits="$(mktemp)"
trap 'rm -f "$hits"' EXIT
cd "$target"

allowed() { # allowed PATH LIST...
  local path="$1" item; shift
  for item in "$@"; do [[ "$path" == "$item"* ]] && return 0; done
  return 1
}

# Contents: CJK, home paths, owner patterns, e-mail addresses. Binary files are scanned through
# their printable ASCII runs (names, paths and addresses are ASCII), without the CJK check.
text="$hits.text"
while IFS= read -r -d '' file; do
  path="${file#./}"
  source="$file"
  if ! grep -qI '' "$file"; then
    LC_ALL=C tr -c '[:print:]' '\n' < "$file" > "$text"
    source="$text"
  elif ! allowed "$path" "${CJK_ALLOWED[@]}"; then
    grep -nP -- "$CJK" "$source" | cut -c1-160 | sed "s|^|$path: CJK: |" >> "$hits" || true
  fi
  grep -naP -- "$HOME_PATH" "$source" | cut -c1-160 | sed "s|^|$path: home path: |" >> "$hits" || true
  for pattern in "${owner[@]}"; do
    grep -naP -- "$pattern" "$source" | cut -c1-160 | sed "s|^|$path: owner pattern: |" >> "$hits" || true
  done
  if ! allowed "$path" "${EMAIL_ALLOWED_PREFIX[@]}"; then
    grep -noaP -- "$EMAIL" "$source" | grep -vP -- "$EMAIL_PLACEHOLDER" | sed "s|^|$path: e-mail: |" >> "$hits" || true
  fi
done < <(find . -type f -print0 | sort -z)
rm -f "$text"

# Paths: CJK and owner patterns in file and directory names.
find . -mindepth 1 | sed 's|^\./||' | sort > "$hits.paths"
grep -P -- "$CJK" "$hits.paths" | sed 's|^|path: CJK: |' >> "$hits" || true
for pattern in "${owner[@]}"; do
  grep -P -- "$pattern" "$hits.paths" | sed 's|^|path: owner pattern: |' >> "$hits" || true
done
rm -f "$hits.paths"

# Markdown links must resolve inside the exported tree.
links="$(python3 -I "$repo/tools/release/check-links.py" "$target")" || [[ -n "$links" ]] \
  || die "the link check failed to run"
[[ -z "$links" ]] || sed 's|^|link: |' <<< "$links" >> "$hits"

if [[ -s "$hits" ]]; then
  cat "$hits"
  die "$(wc -l < "$hits") findings; nothing was committed in $target"
fi
echo "scan clean"

author="${PUBLIC_AUTHOR:-YTM Glass <noreply@ytm-glass.invalid>}"
name="${author% <*}"
email="${author##*<}"
email="${email%>}"
[[ -n "$name" && "$email" == *@* ]] || die "PUBLIC_AUTHOR must look like 'Name <email>'"
git init -q -b main
git add -A
GIT_COMMITTER_NAME="$name" GIT_COMMITTER_EMAIL="$email" \
  git -c user.name="$name" -c user.email="$email" commit -q -m "Initial public release"
echo "created $(git rev-parse --short HEAD) in $target"
