#!/bin/sh
# Deploys the docs site to GitHub Pages via the `gh-pages` branch:
# builds the site (verifying every example — a broken example aborts
# the deploy), then force-pushes the output as the branch tip (the
# branch carries the _site git history between deploys; the force-
# push replaces whatever was there).
#
# One-time setup on GitHub: Settings → Pages → Source: "Deploy from a
# branch" → branch `gh-pages`, folder `/ (root)`.
#
# Usage: docs/deploy.sh [--release]

set -eu
cd "$(dirname "$0")/.."

PROFILE=debug
CARGO_PROFILE_FLAG=""
if [ "${1:-}" = "--release" ]; then
    PROFILE=release
    CARGO_PROFILE_FLAG="--release"
fi

echo "==> building the surform binary"
cargo build --bin surform $CARGO_PROFILE_FLAG

echo "==> rebuilding the playground's wasm bundle (docs/editor → assets/editor)"
# A deploy ships whatever is committed — so the committed bundle is
# refreshed from the current sources first (idempotent; needs the
# once-run `npm install` in docs/editor). The remap keeps the building
# machine's home path out of the wasm's embedded panic locations.
(cd docs/editor && RUSTFLAGS="--remap-path-prefix=$HOME=/build" node build.mjs)

echo "==> building the site (every example is verified on the way)"
cargo run -q -p surform-docs $CARGO_PROFILE_FLAG -- \
    --out docs/_site --cli "target/$PROFILE/surform"

echo "==> carrying the third-party notices into the site"
# The site serves the SurrealDB parser compiled into the wasm bundle;
# BSL 1.1: "You must conspicuously display this License on each
# original or modified copy of the Licensed Work."
cp THIRD-PARTY-NOTICES.md docs/_site/

echo "==> publishing docs/_site to the gh-pages branch"
cd docs/_site
git init -q 2>/dev/null || true
git checkout -q -B gh-pages 2>/dev/null || true
git add -A
git -c user.name="${SURFORM_DEPLOY_NAME:-surform-deploy}" \
    -c user.email="${SURFORM_DEPLOY_EMAIL:-deploy@invalid}" \
    commit -q -m "docs: site built from $(cd ../.. && git rev-parse --short HEAD)" \
    --allow-empty-message 2>/dev/null || true
git remote remove origin 2>/dev/null || true
git remote add origin "$(cd ../.. && git config --get remote.origin.url)"
git push -q --force origin gh-pages

echo "==> done — Pages serves the site in a minute or two."
