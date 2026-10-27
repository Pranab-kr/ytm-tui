#!/usr/bin/env bash
# Keep packaging/aur/PKGBUILD in step with a release and mirror it to the AUR.
#
#   ./scripts/aur-sync.sh                 # sync the version in Cargo.toml
#   ./scripts/aur-sync.sh --version 0.3.1 # sync a different release
#   ./scripts/aur-sync.sh --push          # also push to the AUR
#
# The checksum comes from dist/ when the release tarball is still there, and
# from GitHub otherwise, so this works on any machine, not just the one that
# built the release.
set -euo pipefail
cd "$(dirname "$0")/.."

AUR_CLONE="${AUR_CLONE:-$HOME/aur/ytm-tui-bin}"
AUR_REMOTE="ssh://aur@aur.archlinux.org/ytm-tui-bin.git"
PKGREL=1
VERSION=""
PUSH=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --push) PUSH=1; shift ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

[[ -n "$VERSION" ]] || VERSION=$(grep -m1 '^version' crates/ytm-app/Cargo.toml | cut -d'"' -f2)

TARBALL="ytm-tui-v${VERSION}-x86_64-linux.tar.gz"
URL="https://github.com/Pranab-kr/ytm-tui/releases/download/v${VERSION}/${TARBALL}"

echo "==> ytm-tui-bin ${VERSION}-${PKGREL}"

# 1. Checksum of the release tarball.
if [[ -f "dist/${TARBALL}" ]]; then
  echo "    checksum from dist/${TARBALL}"
  TARBALL_SHA=$(sha256sum "dist/${TARBALL}" | cut -d' ' -f1)
else
  echo "    checksum from ${URL}"
  TARBALL_SHA=$(curl -fsSL "$URL" | sha256sum | cut -d' ' -f1)
fi
LICENSE_SHA=$(sha256sum packaging/aur/LICENSE | cut -d' ' -f1)

# 2. Point the PKGBUILD at that release.
sed -i "s/^pkgver=.*/pkgver=${VERSION}/" packaging/aur/PKGBUILD
sed -i "s/^pkgrel=.*/pkgrel=${PKGREL}/" packaging/aur/PKGBUILD

# Both sums live in one array; rewrite them as a pair instead of patching lines,
# which would break the moment the source list changes.
python3 - "packaging/aur/PKGBUILD" "$TARBALL_SHA" "$LICENSE_SHA" <<'PY'
import re
import sys

path, tarball_sha, license_sha = sys.argv[1:4]
text = open(path).read()
new = "sha256sums=('%s'\n            '%s')" % (tarball_sha, license_sha)
patched = re.sub(r"sha256sums=\([^)]*\)", lambda _: new, text, count=1)
if patched == text and new not in text:
    sys.exit("could not find sha256sums in %s" % path)
open(path, "w").write(patched)
PY

# 3. Regenerate .SRCINFO — the AUR refuses a push without it, and a stale one
#    makes the web page show the wrong version.
( cd packaging/aur && makepkg --printsrcinfo > .SRCINFO )
echo "    wrote packaging/aur/{PKGBUILD,.SRCINFO}"

# 4. Mirror into the AUR clone.
if [[ ! -d "$AUR_CLONE/.git" ]]; then
  cat <<MSG

The AUR clone is missing. Once an AUR account exists and its SSH key is
registered, create it and run this script again:

  git clone ${AUR_REMOTE} "${AUR_CLONE}"

MSG
  exit 0
fi

cp packaging/aur/PKGBUILD packaging/aur/.SRCINFO packaging/aur/LICENSE "$AUR_CLONE/"
(
  cd "$AUR_CLONE"
  git add PKGBUILD .SRCINFO LICENSE
  if git diff --cached --quiet; then
    echo "    AUR clone already up to date"
  else
    git commit -q -m "upgpkg: ytm-tui-bin ${VERSION}-${PKGREL}"
    echo "    committed in ${AUR_CLONE}"
  fi
)

if [[ "$PUSH" == 1 ]]; then
  ( cd "$AUR_CLONE" && git push origin master )
  echo "    pushed to the AUR"
else
  echo "    not pushed; run with --push, or: git -C ${AUR_CLONE} push origin master"
fi
