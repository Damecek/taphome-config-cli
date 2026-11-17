#!/bin/sh
# Installs the taphome release binary for this platform into ~/.local/bin (override with TAPHOME_INSTALL_DIR).
# Pin a version with TAPHOME_VERSION=v0.1.0. The download is checked against the release's SHA256SUMS.
set -eu

repo="Damecek/taphome-config-cli"
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
  *) echo "unsupported platform: $(uname -s) $(uname -m). Download a release manually from https://github.com/$repo/releases" >&2; exit 1 ;;
esac

if [ -n "${TAPHOME_VERSION:-}" ]; then
  base="https://github.com/$repo/releases/download/$TAPHOME_VERSION"
else
  base="https://github.com/$repo/releases/latest/download"
fi
asset="taphome-$target.tar.gz"
dir="${TAPHOME_INSTALL_DIR:-$HOME/.local/bin}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL -o "$tmp/$asset" "$base/$asset"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"

want=$(awk -v f="$asset" '$2 == f {print $1}' "$tmp/SHA256SUMS")
if command -v sha256sum >/dev/null 2>&1; then
  got=$(sha256sum "$tmp/$asset" | awk '{print $1}')
else
  got=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
fi
if [ -z "$want" ] || [ "$want" != "$got" ]; then
  echo "checksum mismatch for $asset" >&2
  exit 1
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
install -m 755 "$tmp/taphome" "$dir/taphome"
echo "installed $dir/taphome"
