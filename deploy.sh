#!/usr/bin/env bash
# Sync the landing page into dist/ and deploy it to Cloudflare Pages.
set -euo pipefail
cd "$(dirname "$0")"

# --branch master matches the project's production branch, so this always
# publishes to production rather than a preview alias.
cp index.html dist/index.html
mkdir -p dist/assets
cp assets/styles.css assets/site.js dist/assets/
wrangler pages deploy dist --project-name deeptui --branch master --commit-dirty=true "$@"
