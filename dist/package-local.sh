#!/bin/sh
set -eu

target="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
version="${TSCODE_VERSION:-v0.1.0-dev}"
exe=""

case "$target" in
    *windows*) exe=".exe" ;;
esac

cargo build --release --target "$target"

asset="tscode-${version}-${target}"
stage="dist/${asset}"
mkdir -p "$stage"
cp "target/${target}/release/tscode${exe}" "$stage/"
cp README.md LICENSE install.sh "$stage/"

case "$target" in
    *windows*) (cd dist && zip -qr "${asset}.zip" "$asset") ;;
    *) tar -czf "dist/${asset}.tar.gz" -C dist "$asset" ;;
esac

echo "created dist/${asset}"
