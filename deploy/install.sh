#!/usr/bin/env bash
# Installs iloader-server on Ubuntu Server. Run from the repository root after
# building:  cargo build --release -p iloader-server && bun run build
set -euo pipefail

PREFIX=/opt/iloader-server
CONF=/etc/iloader-server

if [[ $EUID -ne 0 ]]; then
  echo "run as root (sudo $0)" >&2
  exit 1
fi
[[ -x target/release/iloader-server ]] || { echo "missing target/release/iloader-server (cargo build --release -p iloader-server)"; exit 1; }
[[ -f dist/index.html ]] || { echo "missing dist/ (bun run build)"; exit 1; }

apt-get install -y usbmuxd libimobiledevice-utils >/dev/null || \
  echo "warning: could not install usbmuxd; install it manually" >&2

id iloader >/dev/null 2>&1 || useradd --system --home-dir /var/lib/iloader-server --shell /usr/sbin/nologin iloader

install -d -m 0755 "$PREFIX"
install -m 0755 target/release/iloader-server "$PREFIX/iloader-server"
rm -rf "$PREFIX/dist"
cp -r dist "$PREFIX/dist"
chmod -R a+rX "$PREFIX/dist"
install -m 0644 README-SERVER.md "$PREFIX/README-SERVER.md"

install -d -m 0755 "$CONF"
if [[ ! -f "$CONF/env" ]]; then
  install -m 0600 deploy/env.example "$CONF/env"
  echo "created $CONF/env (review it)"
fi

install -m 0644 deploy/iloader-server.service /etc/systemd/system/iloader-server.service
systemctl daemon-reload
systemctl enable --now iloader-server
systemctl restart iloader-server
sleep 2
systemctl --no-pager --lines=5 status iloader-server || true

echo
echo "Access token: sudo cat /var/lib/iloader-server/access_token"
