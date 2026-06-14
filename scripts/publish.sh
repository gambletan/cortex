#!/usr/bin/env bash
# Publish the official Cortex packages — PyPI (cortex-ai-memory) and npm
# (@cortex-ai-memory/cortex-memory). One source of truth for the version, version-sync
# guard, dry-run by default, and per-target flags. Usable locally and from CI.
#
# Usage:
#   scripts/publish.sh [--pypi] [--npm] [--all] [--execute] [--version X.Y.Z]
#
#   (no target)     → defaults to --all
#   --execute       → actually publish (default is a DRY RUN that builds but doesn't upload)
#   --version X.Y.Z → override; default is read from cortex-mcp-server/Cargo.toml
#
# Credentials (only needed with --execute):
#   PyPI : MATURIN_PYPI_TOKEN   (or a configured ~/.pypirc / trusted publishing in CI)
#   npm  : NPM_TOKEN            (or a prior `npm login`)
#
# The canonical version is the workspace binary's Cargo.toml. This script refuses to
# publish if cortex-python/pyproject.toml or openclaw-plugin/package.json disagree —
# version drift is exactly how "we forgot to bump the package" ships a wrong release.
set -euo pipefail
cd "$(dirname "$0")/.."

DO_PYPI=0 DO_NPM=0 EXECUTE=0 VERSION=""
while [ $# -gt 0 ]; do
  case "$1" in
    --pypi) DO_PYPI=1 ;;
    --npm) DO_NPM=1 ;;
    --all) DO_PYPI=1; DO_NPM=1 ;;
    --execute) EXECUTE=1 ;;
    --version) VERSION="$2"; shift ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
  shift
done
[ $DO_PYPI -eq 0 ] && [ $DO_NPM -eq 0 ] && { DO_PYPI=1; DO_NPM=1; }

CARGO_VER=$(grep -m1 '^version = ' cortex-mcp-server/Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')
VERSION="${VERSION:-$CARGO_VER}"
echo "▶ target version: $VERSION  (dry-run=$([ $EXECUTE -eq 1 ] && echo no || echo yes))"

# ── Version-sync guard ───────────────────────────────────────────────────────
PY_VER=$(grep -m1 '^version = ' cortex-python/pyproject.toml | sed -E 's/.*"([^"]+)".*/\1/')
NPM_VER=$(node -p "require('./openclaw-plugin/package.json').version" 2>/dev/null || echo "?")
fail=0
[ "$PY_VER" != "$VERSION" ] && { echo "✗ cortex-python/pyproject.toml is $PY_VER, expected $VERSION" >&2; fail=1; }
[ "$NPM_VER" != "$VERSION" ] && { echo "✗ openclaw-plugin/package.json is $NPM_VER, expected $VERSION" >&2; fail=1; }
[ $fail -eq 1 ] && { echo "version drift — bump the manifests to $VERSION first." >&2; exit 1; }
echo "✓ versions in sync ($VERSION)"

# ── PyPI: cortex-ai-memory (maturin) ─────────────────────────────────────────
if [ $DO_PYPI -eq 1 ]; then
  echo "── PyPI: cortex-ai-memory ──"
  command -v maturin >/dev/null 2>&1 || { echo "installing maturin..."; pip install --quiet maturin || pipx install maturin; }
  if [ $EXECUTE -eq 1 ]; then
    : "${MATURIN_PYPI_TOKEN:?set MATURIN_PYPI_TOKEN to publish to PyPI}"
    maturin publish -m cortex-python/Cargo.toml --skip-existing
  else
    maturin build --release -m cortex-python/Cargo.toml --out dist
    echo "  (dry run) built wheel into dist/ — not uploaded"
  fi
fi

# ── npm: @cortex-ai-memory/cortex-memory (OpenClaw plugin) ───────────────────
if [ $DO_NPM -eq 1 ]; then
  echo "── npm: @cortex-ai-memory/cortex-memory ──"
  ( cd openclaw-plugin
    npm install --silent
    npm run build
    if [ $EXECUTE -eq 1 ]; then
      [ -n "${NPM_TOKEN:-}" ] && echo "//registry.npmjs.org/:_authToken=${NPM_TOKEN}" > ~/.npmrc
      npm publish --access public
    else
      npm publish --dry-run --access public
    fi
  )
fi

echo "✔ done${EXECUTE:+}"
[ $EXECUTE -eq 0 ] && echo "  this was a DRY RUN — re-run with --execute (and tokens) to publish."
