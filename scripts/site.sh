#!/usr/bin/env bash
# Builds the GitHub Pages site into target/site and, with --publish, force-pushes it
# to the gh-pages branch. The site is site/ plus a live demo that runs the real WASM runtime.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo web
out=target/site
rm -rf "$out"
mkdir -p "$out/demo"
cp site/index.html "$out/"
cp site/demo/index.html "$out/demo/"
cp hosts/web/tinyjump.js "$out/demo/"
cp target/wasm32-unknown-unknown/small/tj_wasm.wasm "$out/demo/tinyjump.wasm"
cp examples/todo/app.tj "$out/demo/app.tj"
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
