#!/usr/bin/env bash
# Build and (re)start Cortex Cloud on cortex.alvinsclub.ai (alvin@35.223.174.205), next to the
# other Studio services. The container uses the host network and listens on 127.0.0.1:8084
# only (so nginx's X-Forwarded-For is trusted); nginx terminates TLS and routes ONLY the
# Cortex paths (snippet: cortex-cloud.nginx.conf). Tenant data: ~/cortex-cloud/data;
# master key: ~/cortex-cloud/secrets (outside data, never backed up).
set -euo pipefail

HOST="${HOST:-alvin@35.223.174.205}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519_usa}"
BASE_URL="${BASE_URL:-https://cortex.alvinsclub.ai}"
LISTEN="${LISTEN:-127.0.0.1:8084}"
SITE="${SITE:-/etc/nginx/sites-available/cortex}"
REMOTE_DIR="cortex-cloud"
SSH_OPTS=(-o ConnectTimeout=30 -o ServerAliveInterval=5 -o IdentitiesOnly=yes -i "$SSH_KEY")
SSH=(ssh "${SSH_OPTS[@]}" "$HOST")

cd "$(dirname "$0")/../.."
sha="$(git rev-parse --short HEAD)"
if [[ -n "$(git status --porcelain -- cortex-core cortex-http cortex-mcp-server cortex-cloud Cargo.toml Cargo.lock deploy/cortex-cloud)" ]]; then
  echo "refusing: uncommitted changes in the service sources" >&2
  exit 1
fi

# Upload ONLY committed build inputs (never the working directory: it can hold ignored
# files such as logs, caches or local databases).
git archive --format=tar HEAD -- Cargo.toml Cargo.lock cortex-core cortex-http cortex-mcp-server cortex-cloud deploy/cortex-cloud \
  | "${SSH[@]}" "rm -rf ~/$REMOTE_DIR/src-tree && mkdir -p ~/$REMOTE_DIR/src-tree && tar -x -C ~/$REMOTE_DIR/src-tree"

"${SSH[@]}" bash -s -- "$sha" "$BASE_URL" "$LISTEN" "$SITE" <<'REMOTE'
set -euo pipefail
sha="$1"; base="$2"; listen="$3"; site="$4"
cd ~/cortex-cloud
install -d -m 700 data secrets
docker build -q -f src-tree/deploy/cortex-cloud/Dockerfile -t "cortex-cloud:$sha" -t cortex-cloud:latest src-tree
docker rm -f cortex-cloud >/dev/null 2>&1 || true
docker run -d --name cortex-cloud --restart unless-stopped --network host -e RUST_LOG=cortex_cloud=info,cortex_mcp_server=info \
  -v "$HOME/cortex-cloud/data:/data" -v "$HOME/cortex-cloud/secrets:/secrets" \
  "cortex-cloud:$sha" --base-url "$base" --listen "$listen" >/dev/null
for i in $(seq 1 60); do
  curl -fsS "http://$listen/healthz" >/dev/null 2>&1 && break
  sleep 2
done
curl -fsS "http://$listen/healthz" >/dev/null || { docker logs --tail 50 cortex-cloud; exit 1; }

# nginx: route only the Cortex paths to the SAME address the container listens on; idempotent.
sed "s#127.0.0.1:8084#$listen#g" src-tree/deploy/cortex-cloud/cortex-cloud.nginx.conf | sudo tee /etc/nginx/snippets/cortex-cloud.conf >/dev/null
if ! sudo grep -q "snippets/cortex-cloud.conf" "$site"; then
  anchor='include /etc/nginx/snippets/ecn-muse.conf;'
  sudo grep -qF "$anchor" "$site" || { echo "no '$anchor' in $site: add 'include /etc/nginx/snippets/cortex-cloud.conf;' to its 443 server block by hand" >&2; exit 1; }
  backup="$site.bak-cortex-$(date +%Y%m%d%H%M%S)"
  sudo cp "$site" "$backup"
  # Before the catch-all `location /` include, inside the 443 server block.
  sudo sed -i '0,/include \/etc\/nginx\/snippets\/ecn-muse.conf;/s//include \/etc\/nginx\/snippets\/cortex-cloud.conf;\n    include \/etc\/nginx\/snippets\/ecn-muse.conf;/' "$site"
  sudo grep -q "snippets/cortex-cloud.conf" "$site" || { sudo cp "$backup" "$site"; echo "include not inserted; site restored" >&2; exit 1; }
fi
sudo nginx -t
sudo systemctl reload nginx
# The public route must reach THIS service (an unknown tenant id answers an empty 404).
code=$(curl -s -o /dev/null -w '%{http_code}' "$base/t/AAAAAAAAAAAAAAAAAAAAAA/mcp")
[ "$code" = 404 ] || { echo "public route check failed ($code)" >&2; exit 1; }
echo "deployed cortex-cloud:$sha"
REMOTE
