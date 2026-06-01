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
