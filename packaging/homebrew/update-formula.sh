#!/bin/sh
# Rewrite sinter.rb for a release: set the version and fill each sha256
# from the .sha256 assets published with the GitHub release.
#
#   usage: update-formula.sh <version>          (e.g. 0.37.0, no leading v)
#
# Run after a release exists; then copy sinter.rb into the
# shellfu/homebrew-tap repo as Formula/sinter.rb.
set -eu

VERSION="${1:?usage: update-formula.sh <version>}"
DIR="$(cd "$(dirname "$0")" && pwd)"
FORMULA="$DIR/sinter.rb"
BASE="https://github.com/shellfu/sinter/releases/download/v$VERSION"

sed -i.bak "s/^  version \".*\"/  version \"$VERSION\"/" "$FORMULA"

for target in aarch64-apple-darwin x86_64-apple-darwin \
              aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
  sum="$(curl -fsSL "$BASE/sinter-$target.tar.gz.sha256" | awk '{print $1}')"
  [ -n "$sum" ] || { echo "error: empty sha256 for $target" >&2; exit 1; }
  # Replace whichever sha256 line follows this target's url line.
  awk -v url="sinter-$target.tar.gz" -v sum="$sum" '
    /url .*/ { pending = index($0, url) > 0 }
    pending && /sha256 / { sub(/sha256 ".*"/, "sha256 \"" sum "\""); pending = 0 }
    { print }
  ' "$FORMULA" > "$FORMULA.tmp" && mv "$FORMULA.tmp" "$FORMULA"
done

rm -f "$FORMULA.bak"
echo "updated $FORMULA for v$VERSION"
