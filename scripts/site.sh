#!/usr/bin/env bash
# Builds the GitHub Pages site into target/site and, with --publish, force-pushes it
# to the gh-pages branch. The site is site/ plus a live demo that runs the real WASM runtime.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo web
out=target/site
rm -rf "$out"
mkdir -p "$out/demo"
# The platform table comes from docs/vision.md, so the site never drifts from it.
ROWS=$(awk '
  /^## Platforms/ { inside = 1; next }
  /^## / { inside = 0 }
  inside && /^\|/ && !/^\|---/ && !/^\| Target/ {
    split($0, c, "|"); t = c[2]; s = c[3]
    gsub(/^ +| +$/, "", t); gsub(/^ +| +$/, "", s)
    printf "<tr><td>%s</td><td class=\"%s\">%s</td></tr>\n", t, (s ~ /^working/ ? "ok" : "next"), s
  }' docs/vision.md)
[[ -n "$ROWS" ]] || { echo "no platform table found in docs/vision.md" >&2; exit 1; }
ROWS="$ROWS" awk '{ if (index($0, "<!-- PLATFORMS -->")) { sub(/<!-- PLATFORMS -->/, ENVIRON["ROWS"]) } print }' site/index.html > "$out/index.html"
cp site/demo/index.html "$out/demo/"
cp hosts/web/tinyjump.js "$out/demo/"
cp target/wasm32-unknown-unknown/small/tj_wasm.wasm "$out/demo/tinyjump.wasm"
cp examples/todo/app.tj "$out/demo/app.tj"
if [[ -d site/guide ]]; then
  cp -R site/guide "$out/guide"
fi
if [[ -d site/skills ]]; then
  cp -R site/skills "$out/skills"
fi
touch "$out/.nojekyll"
echo "site built in $out"

if [[ "${1:-}" == "--publish" ]]; then
  remote=$(git remote get-url origin)
  cd "$out"
  git init -q -b gh-pages
  git add -A
  git -c user.name="$(git -C ../.. config user.name)" -c user.email="$(git -C ../.. config user.email)" commit -q -m "Publish site"
  git push -q -f "$remote" gh-pages
  rm -rf .git
  echo "published to gh-pages"
fi
