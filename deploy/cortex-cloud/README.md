# Deploying Cortex Cloud for Muse

Design and threat model: [`docs/design/muse-cloud.md`](../../docs/design/muse-cloud.md).

The service holds only what users chose to share with Muse, one encrypted directory per
user. It needs one small Linux server with a public IPv4, ports 80 and 443 open, and a
DNS name pointing at it.

**studio.alvinsclub.ai (current):** one command from a clean checkout:

```bash
deploy/cortex-cloud/deploy.sh   # build on the server (docker), run on 127.0.0.1:8084, route the Cortex paths in nginx
curl -s -o /dev/null -w '%{http_code}\n' https://studio.alvinsclub.ai/t/AAAAAAAAAAAAAAAAAAAAAA/mcp   # 404 = service answering
```

It mirrors the other Studio services: docker container (host network, so nginx's
X-Forwarded-For is trusted), data in `~/cortex-cloud/data`, master key in
`~/cortex-cloud/secrets` (created on first start, 0600, keep it out of backups), and
`cortex-cloud.nginx.conf` included in the site's 443 server block before its catch-all.

**Any other host:** `docker build -f deploy/cortex-cloud/Dockerfile .`, or the systemd unit
(`cortex-cloud.service`) plus `Caddyfile` for a dedicated host name.

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
