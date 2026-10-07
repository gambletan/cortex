# Deploying Cortex Cloud for Muse

Design and threat model: [`docs/design/muse-cloud.md`](../../docs/design/muse-cloud.md).

The service holds only what users chose to share with Muse, one encrypted directory per
user. It needs one small Linux server with a public IPv4, ports 80 and 443 open, and a
DNS name pointing at it.

```bash
# 1. Build (on the server or in CI) and install
cargo build --release -p cortex-cloud
sudo install -m 0755 target/release/cortex-cloud /usr/local/bin/

# 2. User, directories (the master key must NOT be in backups)
sudo useradd --system --home /var/lib/cortex-cloud --shell /usr/sbin/nologin cortex-cloud
sudo install -d -o cortex-cloud -g cortex-cloud -m 0700 /var/lib/cortex-cloud /etc/cortex-cloud

# 3. Service (edit the host name first)
sudo cp deploy/cortex-cloud/cortex-cloud.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now cortex-cloud
# first start creates /etc/cortex-cloud/master.key (0600)

# 4a. TLS with Caddy on a dedicated host name (edit the host name first)
sudo cp deploy/cortex-cloud/Caddyfile /etc/caddy/Caddyfile && sudo systemctl reload caddy

# 4b. …or behind an existing nginx site: route only Cortex paths
sudo cp deploy/cortex-cloud/cortex-cloud-proxy.conf /etc/nginx/
#   then add `include /path/to/deploy/cortex-cloud/nginx-locations.conf;` inside the
#   site's 443 server block, and: sudo nginx -t && sudo systemctl reload nginx

curl -s https://<host>/t/AAAAAAAAAAAAAAAAAAAAAA/mcp -o /dev/null -w '%{http_code}\n'   # → 404 (service answers)
```

Devices use `https://studio.alvinsclub.ai` by default; set `CORTEX_CLOUD_URL` on the device
to point at another deployment.

Operations:
- Losing `/var/lib/cortex-cloud` loses nothing important: each device pushes its shared
  list again on the next `muse_share` / `muse_connect`. Do not back it up.
- Losing `master.key` makes every tenant unreadable; users reconnect (`muse_connect`).
- Tenants whose device hasn't called in 90 days are deleted automatically, and so are
  tenants that never pushed anything within a day of registering.
- Registration is limited per client network (/64 for IPv6) and globally; behind a CDN,
  make sure the proxy passes the real client address as the last X-Forwarded-For entry.
