# Deploying Cortex Cloud for Muse

Design and threat model: [`docs/design/muse-cloud.md`](../../docs/design/muse-cloud.md).

The service holds only what users chose to share with Muse, one encrypted directory per
user. It needs one small Linux server with a public IPv4, ports 80 and 443 open, and a
DNS name pointing at it.

**cortex.alvinsclub.ai:** one command from a clean checkout after configuring its TLS site:

```bash
deploy/cortex-cloud/deploy.sh   # build on the server (docker), run on 127.0.0.1:8084, route the Cortex paths in nginx
curl -s -o /dev/null -w '%{http_code}\n' https://cortex.alvinsclub.ai/t/AAAAAAAAAAAAAAAAAAAAAA/mcp   # 404 = service answering
```

It mirrors the other Studio services: docker container (host network, so nginx's
X-Forwarded-For is trusted), data in `~/cortex-cloud/data`, master key in
`~/cortex-cloud/secrets` (created on first start, 0600, keep it out of backups), and
`cortex-cloud.nginx.conf` included in the site's 443 server block before its catch-all.

**Any other host:** `docker build -f deploy/cortex-cloud/Dockerfile .`, or the systemd unit
(`cortex-cloud.service`) plus `Caddyfile` for a dedicated host name.

New devices use `https://cortex.alvinsclub.ai` by default; set `CORTEX_CLOUD_URL` on the device
to point at another deployment.

**Dedicated hostname: `cortex.alvinsclub.ai`.** DNS resolving is not sufficient: nginx
must serve a certificate whose subject alternative names include this exact hostname.
The checked-in [`cortex-domain.nginx.conf`](cortex-domain.nginx.conf) is a dedicated site
template, using the existing `127.0.0.1:8084` container and Cortex routing snippet. Obtain
the hostname's certificate first, then install/enable the site at
`/etc/nginx/sites-available/cortex`. The deployment command installs the snippet and
checks nginx before reload:

```bash
BASE_URL=https://cortex.alvinsclub.ai SITE=/etc/nginx/sites-available/cortex deploy/cortex-cloud/deploy.sh
curl --fail https://cortex.alvinsclub.ai/healthz
```

Install the certificate reload hook on the nginx host and test renewal:

```bash
sudo install -m 755 deploy/cortex-cloud/renew-cortex-cert.sh /etc/letsencrypt/renewal-hooks/deploy/cortex-nginx
sudo certbot renew --cert-name cortex.alvinsclub.ai --dry-run --run-deploy-hooks
```

The hook reloads nginx only when this domain renews, after checking its configuration.
See [Certbot's renewal-hook documentation](https://eff-certbot.readthedocs.io/en/stable/using.html#renewing-certificates).

Do not bypass certificate checks. Switching the service's base URL changes its OAuth
issuer/resource: existing Muse connectors must reconnect. Existing local device state
retains its previous base URL, so set `CORTEX_CLOUD_URL=https://cortex.alvinsclub.ai` for
those devices. Keep the legacy hostname's routes available for device management during
migration; do not redirect signed device requests, whose paths are signature-bound.
Existing connection state may retain the old Studio management URL; its signed API
routes remain available during migration. New connections use the dedicated hostname.

**Validation.** Run the entire workspace suite:

```bash
cargo test --workspace --exclude cortex-python --exclude cortex-wasm
```

The test that writes to a real Google Drive is explicitly ignored in ordinary runs.
To exercise it on a machine with a working Google Drive mount, run:

```bash
cargo test -p cortex-core --test test_gdrive_real test_real_gdrive_encrypted_sync -- --ignored
```

It uses a unique owned temporary folder; failed cloud mounts remain test failures when
this live test is explicitly selected. The normal suite still exercises encrypted sync
against local temporary folders.

Operations:
- Losing `/var/lib/cortex-cloud` loses nothing important: each device pushes its shared
  list again on the next `muse_share` / `muse_connect`. Do not back it up.
- Losing `master.key` makes every tenant unreadable; users reconnect (`muse_connect`).
- Tenants whose device hasn't called in 90 days are deleted automatically, and so are
  tenants that never pushed anything within a day of registering.
- Registration is limited per client network (/64 for IPv6) and globally; behind a CDN,
  make sure the proxy passes the real client address as the last X-Forwarded-For entry.
