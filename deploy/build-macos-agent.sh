#!/bin/sh
# Build the MonitorLab agent on a Mac and upload it to your hub, so Macs can install and self-update.
# Run from the repository root on the Mac:
#   HUB=https://monitorlab.example.com EMAIL=you@example.com ./deploy/build-macos-agent.sh
# (asks for the password; the account needs the "Manage servers" permission)
set -eu
: "${HUB:?set HUB to the hub URL}"
: "${EMAIL:?set EMAIL to your hub login}"
command -v cargo >/dev/null 2>&1 || { echo "Rust is required: https://rustup.rs" >&2; exit 1; }
printf "Hub password for %s: " "$EMAIL"; stty -echo; read -r PASSWORD; stty echo; echo

rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --locked -p monitorlab-agent --target aarch64-apple-darwin
cargo build --release --locked -p monitorlab-agent --target x86_64-apple-darwin

JAR=$(mktemp); trap 'rm -f "$JAR"' EXIT
curl -fsS -c "$JAR" -H 'content-type: application/json' -H 'x-monitorlab-csrf: 1' \
  -d "{\"email\":\"$EMAIL\",\"password\":\"$PASSWORD\"}" "$HUB/api/auth/login" >/dev/null
for pair in "aarch64-apple-darwin:monitorlab-agent-macos-arm64" "x86_64-apple-darwin:monitorlab-agent-macos-amd64"; do
  target=${pair%%:*}; name=${pair##*:}
  echo "Uploading $name ..."
  curl -fsS -b "$JAR" -X PUT -H 'x-monitorlab-csrf: 1' -H 'content-type: application/octet-stream' \
    --data-binary "@target/$target/release/monitorlab-agent" "$HUB/api/agent-binaries/$name"
  echo
done
echo "Done. Servers & agents -> Add server -> macOS now offers the installer."
