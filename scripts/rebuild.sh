#!/usr/bin/env bash
# rebuild.sh — Build ethos-server and restart the systemd service
# Usage: ./scripts/rebuild.sh [--release]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_ROOT"

RELEASE_FLAG=""
PROFILE="dev"
if [[ "${1:-}" == "--release" ]]; then
  RELEASE_FLAG="--release"
  PROFILE="release"
fi

echo "🔨 Building ethos-server (profile: $PROFILE)..."
cargo build --bin ethos-server $RELEASE_FLAG

# Update service binary path if switching profiles
if [[ "$PROFILE" == "release" ]]; then
  BINARY="$PROJECT_ROOT/target/release/ethos-server"
  # Patch the service file to point at release binary
  sed -i "s|target/debug/ethos-server|target/release/ethos-server|g" \
    ~/.config/systemd/user/ethos-server.service
  systemctl --user daemon-reload
else
  BINARY="$PROJECT_ROOT/target/debug/ethos-server"
  # Ensure service file points at debug binary
  sed -i "s|target/release/ethos-server|target/debug/ethos-server|g" \
    ~/.config/systemd/user/ethos-server.service
  systemctl --user daemon-reload
fi

echo "♻️  Restarting ethos-server.service..."
systemctl --user restart ethos-server

sleep 1
systemctl --user status ethos-server --no-pager -l

echo ""
echo "✅ Done! ethos-server is live at /tmp/ethos.sock"
echo "   Logs: journalctl --user -u ethos-server -f"
