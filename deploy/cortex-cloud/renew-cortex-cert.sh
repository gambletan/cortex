#!/usr/bin/env bash
# Certbot deploy hook: load a renewed Cortex certificate after checking nginx.
set -euo pipefail

case " ${RENEWED_DOMAINS:-} " in
    *" cortex.alvinsclub.ai "*)
        /usr/sbin/nginx -t
        /usr/bin/systemctl reload nginx
        ;;
esac
