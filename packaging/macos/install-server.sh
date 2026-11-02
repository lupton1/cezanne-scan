#!/bin/bash
# Installs cezanne-server as a background service on a Mac (for example a Mac mini next to
# the scanner). It starts when the Mac boots, without anyone logging in, and serves the
# Cezanne Scan web page to the network.
#
#   sudo ./install-server.sh [--token SECRET] [--port 7820] [--connect auto]
#                            [--bridge-token SECRET] [--output-dir DIR] [--binary PATH]
#   sudo ./install-server.sh --uninstall
#
# --token       access token browsers must enter once (recommended)
# --port        web port, 7820 unless given
# --connect     scanner to open at start: auto (default), sim, a device address,
#               or bridge:HOST
# --bridge-token  token of the cezanne-bridge when using --connect bridge:HOST
# --output-dir  where scans are kept on the Mac, ~/Pictures/Cezanne Scans unless given
# --binary      cezanne-server to install, found automatically unless given
#
# Run it again after updating Cezanne Scan to install the new server.
set -euo pipefail

LABEL=com.lupton1.cezanne-server
PLIST=/Library/LaunchDaemons/$LABEL.plist
DEST_DIR=/usr/local/libexec/cezanne-scan
LOG=/Library/Logs/cezanne-server.log
PORT=7820
TOKEN=""
BRIDGE_TOKEN=""
CONNECT=auto
OUTPUT=""
BINARY=""
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --token) TOKEN="$2"; shift 2 ;;
    --port) PORT="$2"; shift 2 ;;
    --connect) CONNECT="$2"; shift 2 ;;
    --bridge-token) BRIDGE_TOKEN="$2"; shift 2 ;;
    --output-dir) OUTPUT="$2"; shift 2 ;;
    --binary) BINARY="$2"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) sed -n '2,21p' "$0"; exit 0 ;;
    *) echo "Unknown option $1 (see --help)"; exit 1 ;;
  esac
done

if [ "$(uname)" != Darwin ]; then
  echo "This script is for macOS. docs/server.md covers Linux and Windows."
  exit 1
fi
if [ "$(id -u)" -ne 0 ]; then
  echo "Run it with sudo: sudo $0 $*"
  exit 1
fi

if [ "$UNINSTALL" -eq 1 ]; then
  launchctl bootout "system/$LABEL" 2>/dev/null || true
  rm -f "$PLIST" "$DEST_DIR/cezanne-server"
  echo "cezanne-server removed. Scans already made were kept."
  exit 0
fi

# the service runs as the person who ran sudo, so the scans belong to them
RUN_USER="${SUDO_USER:-$(stat -f %Su /dev/console)}"
if [ -z "$RUN_USER" ] || [ "$RUN_USER" = root ]; then
  echo "Run this with sudo from your own account, not as root."
  exit 1
fi
RUN_HOME=$(dscl . -read "/Users/$RUN_USER" NFSHomeDirectory | awk '{print $2}')
[ -n "$OUTPUT" ] || OUTPUT="$RUN_HOME/Pictures/Cezanne Scans"

if [ -z "$BINARY" ]; then
  here=$(cd "$(dirname "$0")" && pwd)
  for c in \
    "/Applications/Cezanne Scan.app/Contents/MacOS/cezanne-server" \
    "$RUN_HOME/Applications/Cezanne Scan.app/Contents/MacOS/cezanne-server" \
    "$here/cezanne-server" \
    "$here/../../target/release/cezanne-server" \
    "$RUN_HOME/.cargo/bin/cezanne-server"; do
    if [ -x "$c" ]; then BINARY="$c"; break; fi
  done
fi
if [ -z "$BINARY" ] || [ ! -x "$BINARY" ]; then
  echo "cezanne-server was not found. Either install Cezanne Scan 0.4.0 or later into"
  echo "Applications, or build it from the source folder with"
  echo "    cargo build --release -p cezanne-server"
  echo "or pass its location with --binary."
  exit 1
fi
echo "Installing $BINARY"

mkdir -p "$DEST_DIR"
cp "$BINARY" "$DEST_DIR/cezanne-server"
chmod 755 "$DEST_DIR/cezanne-server"
xattr -d com.apple.quarantine "$DEST_DIR/cezanne-server" 2>/dev/null || true
# Apple Silicon only runs signed code, an ad hoc signature is enough
codesign --force --sign - "$DEST_DIR/cezanne-server" >/dev/null 2>&1 || true

mkdir -p "$OUTPUT"
chown "$RUN_USER" "$OUTPUT"
touch "$LOG"
chown "$RUN_USER" "$LOG"

esc() {
  printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g'
}

TOKEN_XML=""
if [ -n "$TOKEN" ]; then
  TOKEN_XML="    <key>CEZANNE_SERVER_TOKEN</key><string>$(esc "$TOKEN")</string>"
fi
if [ -n "$BRIDGE_TOKEN" ]; then
  TOKEN_XML="$TOKEN_XML
    <key>CEZANNE_BRIDGE_TOKEN</key><string>$(esc "$BRIDGE_TOKEN")</string>"
fi

launchctl bootout "system/$LABEL" 2>/dev/null || true
cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$DEST_DIR/cezanne-server</string>
    <string>--listen</string><string>0.0.0.0:$(esc "$PORT")</string>
    <string>--connect</string><string>$(esc "$CONNECT")</string>
    <string>--output-dir</string><string>$(esc "$OUTPUT")</string>
  </array>
  <key>UserName</key><string>$(esc "$RUN_USER")</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>HOME</key><string>$(esc "$RUN_HOME")</string>
$TOKEN_XML
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$LOG</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict>
</plist>
EOF
# the tokens are in this file, so only root may read it
chown root:wheel "$PLIST"
chmod 600 "$PLIST"
plutil -lint "$PLIST" >/dev/null
launchctl bootstrap system "$PLIST"
launchctl enable "system/$LABEL"

FW=/usr/libexec/ApplicationFirewall/socketfilterfw
if [ -x "$FW" ] && "$FW" --getglobalstate | grep -qi enabled; then
  "$FW" --add "$DEST_DIR/cezanne-server" >/dev/null || true
  "$FW" --unblockapp "$DEST_DIR/cezanne-server" >/dev/null || true
  echo "Allowed cezanne-server through the macOS firewall."
fi

if pmset -g | awk '$1 == "sleep" { exit ($2 == 0) ? 1 : 0 }'; then
  echo
  echo "Note: this Mac is set to sleep, which stops the web page. To keep it awake:"
  echo "    sudo pmset -a sleep 0"
  echo "or System Settings > Energy > Prevent automatic sleeping."
fi

NAME=$(scutil --get LocalHostName 2>/dev/null || hostname -s)
IP=$(ipconfig getifaddr en0 2>/dev/null || ipconfig getifaddr en1 2>/dev/null || true)
echo
echo "cezanne-server is running and starts with the Mac."
echo "Open one of these in a browser on the same network:"
echo "    http://$NAME.local:$PORT"
[ -n "$IP" ] && echo "    http://$IP:$PORT"
echo "Scans are kept in $OUTPUT"
echo "Log: $LOG"
