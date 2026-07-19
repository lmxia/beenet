#!/bin/sh
set -eu
UPSTREAM="${REGISTRY_UPSTREAM:-http://beenet-registry:3030}"
UPSTREAM="${UPSTREAM%/}"
case "$UPSTREAM" in
  http://*) HOSTPORT="${UPSTREAM#http://}" ;;
  https://*) HOSTPORT="${UPSTREAM#https://}" ;;
  *) HOSTPORT="$UPSTREAM" ;;
esac

cat >/etc/nginx/conf.d/default.conf <<EOF
upstream registry_upstream {
    server ${HOSTPORT};
}

server {
    listen 80;
    server_name _;
    root /usr/share/nginx/html;
    index index.html;

    location / {
        try_files \$uri \$uri/ /index.html;
    }

    location /registry/ {
        proxy_http_version 1.1;
        proxy_set_header Host \$host;
        proxy_set_header X-Real-IP \$remote_addr;
        proxy_set_header X-Forwarded-For \$proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto \$scheme;
        proxy_pass http://registry_upstream/;
    }

    location = /health {
        access_log off;
        add_header Content-Type text/plain;
        return 200 'ok';
    }
}
EOF
exec nginx -g 'daemon off;'
