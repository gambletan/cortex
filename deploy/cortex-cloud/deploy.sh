#!/usr/bin/env bash
# Build and (re)start Cortex Cloud on studio.alvinsclub.ai (alvin@35.223.174.205), next to the
# other Studio services. The container uses the host network and listens on 127.0.0.1:8084
# only (so nginx's X-Forwarded-For is trusted); nginx terminates TLS and routes ONLY the
# Cortex paths (snippet: cortex-cloud.nginx.conf). Tenant data: ~/cortex-cloud/data;
# master key: ~/cortex-cloud/secrets (outside data, never backed up).
set -euo pipefail

HOST="${HOST:-alvin@35.223.174.205}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/id_ed25519_usa}"
BASE_URL="${BASE_URL:-https://studio.alvinsclub.ai}"
LISTEN="${LISTEN:-127.0.0.1:8084}"
SITE="${SITE:-/etc/nginx/sites-available/studio}"
REMOTE_DIR="cortex-cloud"
SSH_OPTS=(-o ConnectTimeout=30 -o ServerAliveInterval=5 -o IdentitiesOnly=yes -i "$SSH_KEY")
SSH=(ssh "${SSH_OPTS[@]}" "$HOST")

cd "$(dirname "$0")/../.."
sha="$(git rev-parse --short HEAD)"
if [[ -n "$(git status --porcelain -- cortex-core cortex-mcp-server cortex-cloud Cargo.toml Cargo.lock deploy/cortex-cloud)" ]]; then
  echo "refusing: uncommitted changes in the service sources" >&2
  exit 1
fi

rsync -az --delete -e "ssh ${SSH_OPTS[*]}" \
  --exclude target --exclude .git --exclude cortex-python --exclude cortex-wasm \
  --exclude node_modules --exclude '*.db' --exclude '*.db-wal' --exclude '*.db-shm' \
  ./ "$HOST:$REMOTE_DIR/src-tree/"

"${SSH[@]}" bash -s -- "$sha" "$BASE_URL" "$LISTEN" "$SITE" <<'REMOTE'
set -euo pipefail
sha="$1"; base="$2"; listen="$3"; site="$4"
cd ~/cortex-cloud
install -d -m 700 data secrets
docker build -q -f src-tree/deploy/cortex-cloud/Dockerfile -t "cortex-cloud:$sha" -t cortex-cloud:latest src-tree
docker rm -f cortex-cloud >/dev/null 2>&1 || true
docker run -d --name cortex-cloud --restart unless-stopped --network host \
  -v "$HOME/cortex-cloud/data:/data" -v "$HOME/cortex-cloud/secrets:/secrets" \
  "cortex-cloud:$sha" --base-url "$base" --listen "$listen" >/dev/null
for i in $(seq 1 60); do
  curl -fsS "http://$listen/healthz" >/dev/null 2>&1 && break
  sleep 2
done
curl -fsS "http://$listen/healthz" >/dev/null || { docker logs --tail 50 cortex-cloud; exit 1; }

# nginx: route only the Cortex paths; idempotent.
sudo install -m 644 src-tree/deploy/cortex-cloud/cortex-cloud.nginx.conf /etc/nginx/snippets/cortex-cloud.conf
if ! sudo grep -q "snippets/cortex-cloud.conf" "$site"; then
  sudo cp "$site" "$site.bak-cortex-$(date +%Y%m%d%H%M%S)"
  # Before the catch-all `location /` include, inside the 443 server block.
  sudo sed -i '0,/include \/etc\/nginx\/snippets\/ecn-muse.conf;/s//include \/etc\/nginx\/snippets\/cortex-cloud.conf;\n    include \/etc\/nginx\/snippets\/ecn-muse.conf;/' "$site"
fi
sudo nginx -t
sudo systemctl reload nginx
echo "deployed cortex-cloud:$sha"
REMOTE
