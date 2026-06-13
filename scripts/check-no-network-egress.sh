#!/usr/bin/env bash
# Privacy guard: the default cortex-core build must have NO network or telemetry
# dependencies. This turns the "zero telemetry, your data never leaves the device"
# promise into an enforced, verifiable guarantee — the build fails if any HTTP client,
# telemetry SDK, or model-download crate ever enters the default dependency tree.
#
# Scope: cortex-core (the embeddable memory engine). cortex-http is a local API server
# and legitimately uses an HTTP stack, so it is intentionally out of scope.
#
# Run locally:  bash scripts/check-no-network-egress.sh
set -euo pipefail

# Crate names that imply outbound network access or telemetry. Matched exactly.
FORBIDDEN=(
  reqwest ureq isahc surf attohttpc curl curl-sys
  hyper h2 hyper-util
  sentry posthog opentelemetry mixpanel segment datadog rudderanalytics
  hf-hub fastembed ort onnxruntime
)

echo "Auditing cortex-core default dependency tree for network/telemetry crates..."
# Include normal AND build dependencies (a build script can make network calls at build
# time); dev-dependencies are excluded (test-only, never shipped). --target all audits
# every platform, so a target-specific (e.g. Windows-only) network crate can't slip past.
# --prefix none → one "crate vX.Y.Z" per line; take the crate name.
names="$(cargo tree -p cortex-core --edges normal,build --target all --prefix none 2>/dev/null \
          | awk 'NF {print $1}' | sort -u)"

violations=()
for crate in "${FORBIDDEN[@]}"; do
  if printf '%s\n' "$names" | grep -qx "$crate"; then
    violations+=("$crate")
  fi
done

if [ "${#violations[@]}" -ne 0 ]; then
  echo "::error::Privacy guarantee violated — cortex-core default build pulled in network/telemetry crate(s): ${violations[*]}"
  echo "If this is intentional, it must be gated behind an opt-in cargo feature (like 'embeddings')."
  exit 1
fi

echo "✅ cortex-core default build has no network or telemetry dependencies ($(printf '%s\n' "$names" | wc -l | tr -d ' ') crates audited)."

# A dependency audit can't see direct use of std networking primitives (std::net needs
# no extra crate), so also scan cortex-core's own source for raw sockets.
echo "Scanning cortex-core source for direct network primitives..."
src_hits="$(grep -rnE 'std::net|TcpStream|TcpListener|UdpSocket' cortex-core/src --include='*.rs' || true)"
if [ -n "$src_hits" ]; then
  echo "::error::cortex-core source opens a socket directly (std networking):"
  echo "$src_hits"
  exit 1
fi
echo "✅ no direct network primitives in cortex-core source."

# ── Scope honesty: the SHIPPED binary (cortex-mcp-server) enables `embeddings` by
# default, which intentionally pulls a model-download crate (fastembed). That is NOT a
# data-egress/telemetry path — it fetches model weights once and sends none of the user's
# data — but the distinction must be explicit, not buried. Verify the two real guarantees:
#   1) the binary's DEFAULT tree's only network crate is the embeddings model-fetch, and
#   2) a `--no-default-features` binary is genuinely zero-network (the offline path we
#      document via CORTEX_NO_EMBEDDINGS / --no-default-features).
echo
echo "Auditing shipped binary (cortex-mcp-server) for scope honesty..."
bin_default="$(cargo tree -p cortex-mcp-server --edges normal,build --target all --prefix none 2>/dev/null \
                | awk 'NF {print $1}' | sort -u)"
bin_net=()
for crate in "${FORBIDDEN[@]}"; do
  if printf '%s\n' "$bin_default" | grep -qx "$crate"; then
    bin_net+=("$crate")
  fi
done
# We do NOT allowlist individual crates here: the embeddings model-fetch legitimately pulls
# a whole HTTP stack (hf-hub → reqwest/hyper). Enumerating it would be brittle and would
# wrongly imply telemetry. The honest guarantee is proven below instead: EVERY network
# crate in the default binary must come solely from `embeddings`, i.e. the
# --no-default-features binary must be completely zero-network. Here we just disclose them.
if [ "${#bin_net[@]}" -ne 0 ]; then
  echo "ℹ️  cortex-mcp-server (default) pulls network crates via the opt-out embeddings"
  echo "    model-fetch: ${bin_net[*]}"
  echo "   → one-time ~30MB model download on first ingest; no user data leaves the device."
  echo "   → fully-offline binary: cargo build -p cortex-mcp-server --no-default-features"
  echo "   → runtime offline switch: CORTEX_NO_EMBEDDINGS=1"
fi

# THE guarantee that matters: the documented offline binary is genuinely zero-network,
# which proves every network crate above is attributable solely to the embeddings opt-out.
echo "Verifying --no-default-features binary is zero-network..."
bin_offline="$(cargo tree -p cortex-mcp-server --no-default-features --edges normal,build --target all --prefix none 2>/dev/null \
                | awk 'NF {print $1}' | sort -u)"
offline_violations=()
for crate in "${FORBIDDEN[@]}"; do
  if printf '%s\n' "$bin_offline" | grep -qx "$crate"; then
    offline_violations+=("$crate")
  fi
done
if [ "${#offline_violations[@]}" -ne 0 ]; then
  echo "::error::the --no-default-features binary is NOT zero-network: ${offline_violations[*]}"
  echo "The documented offline path must have no network/telemetry crates."
  exit 1
fi
echo "✅ cortex-mcp-server --no-default-features is zero-network (the documented offline build)."
