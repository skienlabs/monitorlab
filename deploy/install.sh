#!/bin/sh
# MonitorLab agent installer for Linux (served by the hub at /download/install.sh).
# Usage: curl -fsSL https://HUB/download/install.sh | sudo sh -s -- --hub https://HUB --token TOKEN [--fingerprint FP] [--pin sha256//...]
set -eu

HUB=""; TOKEN=""; FP=""; PIN=""
while [ $# -gt 0 ]; do
  case "$1" in
    --hub) HUB="$2"; shift 2 ;;
    --token) TOKEN="$2"; shift 2 ;;
    --fingerprint) FP="$2"; shift 2 ;;
    --pin) PIN="$2"; shift 2 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

if [ -z "$HUB" ] || [ -z "$TOKEN" ]; then
  echo "usage: install.sh --hub URL --token TOKEN [--fingerprint FP] [--pin sha256//...]" >&2
  exit 1
fi
if [ "$(id -u)" != "0" ]; then
  echo "please run as root (sudo)" >&2
  exit 1
fi
command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v systemctl >/dev/null 2>&1 || { echo "systemd is required" >&2; exit 1; }

case "$(uname -m)" in
  x86_64|amd64) ARCH=amd64 ;;
  aarch64|arm64) ARCH=arm64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

HUB="${HUB%/}"
DEST=/usr/local/bin/monitorlab-agent.download
echo "Downloading monitorlab-agent ($ARCH) from $HUB ..."
if [ -n "$PIN" ]; then
  # Self-signed hub: skip CA validation but require the exact public key (pinned).
  curl -fsSL --retry 3 -k --pinnedpubkey "$PIN" -o "$DEST" "$HUB/download/monitorlab-agent-linux-$ARCH"
else
  curl -fsSL --retry 3 -o "$DEST" "$HUB/download/monitorlab-agent-linux-$ARCH"
fi
chmod 0755 "$DEST"

if [ -n "$FP" ]; then
  "$DEST" install --hub "$HUB" --token "$TOKEN" --fingerprint "$FP"
else
  "$DEST" install --hub "$HUB" --token "$TOKEN"
fi
rm -f "$DEST"
echo "Done."
