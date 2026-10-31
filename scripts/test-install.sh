#!/bin/sh
# Offline end-to-end test of install.sh against a fake release built from a given binary.
#
#   scripts/test-install.sh target/debug/sliplane
#
# Any executable that answers `--version` with "sliplane X.Y.Z" will do.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
binary=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
version=$("$binary" --version | awk '{ print $2 }')

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

passed=0
check() {
  printf 'ok   %s\n' "$1"
  passed=$((passed + 1))
}
die() {
  printf 'FAIL %s\n' "$*" >&2
  exit 1
}

# A release holds one archive per platform; the installer must pick the right one itself.
releases=$work/releases
release=$releases/download/v$version
mkdir -p "$release"
for target in aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  package=sliplane-v$version-$target
  mkdir "$work/$package"
  cp "$binary" "$work/$package/sliplane"
  tar -czf "$release/$package.tar.gz" -C "$work" "$package"
done
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$release" && sha256sum ./*.tar.gz | sed 's| \./| |' > checksums.txt)
else
  (cd "$release" && shasum -a 256 ./*.tar.gz | sed 's| \./| |' > checksums.txt)
fi

export SLIPLANE_RELEASES_URL="file://$releases"
export HOME="$work/home"
export SLIPLANE_INSTALL_DIR="$work/install/bin"
mkdir -p "$HOME"
install() { sh "$here/install.sh" "$@"; }

# Fresh install
out=$(install "$version" --non-interactive 2>&1) || die "fresh install failed: $out"
[ "$("$SLIPLANE_INSTALL_DIR/sliplane" --version)" = "sliplane $version" ] || die 'installed binary does not report the version'
case "$out" in *"Installed Sliplane $version"*) ;; *) die "no install message: $out" ;; esac
check 'fresh install'

# A leading "v" is accepted
install "v$version" --non-interactive >/dev/null 2>&1 || die 'v-prefixed version rejected'
check 'v-prefixed version'

# Running it again changes nothing and says so
out=$(install "$version" --non-interactive 2>&1) || die "re-install failed: $out"
case "$out" in *"Already up to date"*) ;; *) die "re-install not recognised as a no-op: $out" ;; esac
check 'idempotent re-install'

# A corrupted archive is rejected and the existing install survives untouched
printf 'previous install' > "$SLIPLANE_INSTALL_DIR/sliplane"
chmod 755 "$SLIPLANE_INSTALL_DIR/sliplane"
cp "$release/checksums.txt" "$work/checksums.good"
# "x" is not a hex digit, so unlike swapping in another digit this can never reproduce the real hash.
sed 's/^./x/' "$work/checksums.good" > "$release/checksums.txt"
if out=$(install "$version" --non-interactive 2>&1); then die 'tampered checksum was accepted'; fi
case "$out" in *"Checksum verification failed"*) ;; *) die "wrong error for bad checksum: $out" ;; esac
[ "$(cat "$SLIPLANE_INSTALL_DIR/sliplane")" = 'previous install' ] || die 'failed install changed the existing binary'
check 'bad checksum rejected, existing install kept'

# No checksums.txt means no install
rm "$release/checksums.txt"
if out=$(install "$version" --non-interactive 2>&1); then die 'install without checksums.txt was accepted'; fi
case "$out" in *"checksums.txt"*) ;; *) die "wrong error for missing checksums: $out" ;; esac
check 'missing checksums.txt rejected'
cp "$work/checksums.good" "$release/checksums.txt"

# Version typos fail before touching the network
if out=$(install 'not-a-version' --non-interactive 2>&1); then die 'garbage version accepted'; fi
case "$out" in *"Use a version such as"*) ;; *) die "wrong error for garbage version: $out" ;; esac
check 'invalid version rejected'

if out=$(install 9.9.9 --non-interactive 2>&1); then die 'nonexistent release accepted'; fi
case "$out" in *"Could not download"*) ;; *) die "wrong error for nonexistent release: $out" ;; esac
check 'nonexistent release rejected'

# --setup-path writes the profile once, however often it runs
rm -rf "$work/install"
SHELL=/bin/bash install "$version" --setup-path >/dev/null 2>&1 || die '--setup-path install failed'
install_line=$(grep -c "$SLIPLANE_INSTALL_DIR" "$HOME/.bashrc" 2>/dev/null || true)
[ "$install_line" = 1 ] || die "expected one PATH line in .bashrc, found $install_line"
SHELL=/bin/bash install "$version" --setup-path >/dev/null 2>&1 || die 'second --setup-path install failed'
install_line=$(grep -c "$SLIPLANE_INSTALL_DIR" "$HOME/.bashrc" 2>/dev/null || true)
[ "$install_line" = 1 ] || die "PATH line was written twice ($install_line)"
check '--setup-path is idempotent'

printf '\n%s checks passed\n' "$passed"
