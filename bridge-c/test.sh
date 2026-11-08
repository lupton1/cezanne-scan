#!/bin/bash
# Tests the C bridge's protocol handling on any Mac, without the scanner.
#
#   make check
#
# The test build (make relay) forwards every command to the Rust cezanne-bridge running
# the simulator, so the chain is: cezanne-cli -> C bridge -> Rust bridge -> simulator.
# The checks:
#  1. probe and a scan through the C bridge give the same TIFF as a scan straight from
#     the simulator
#  2. a wrong token is refused
#  3. the C bridge itself refuses firmware and EEPROM writes, so they never reach the
#     Rust bridge behind it
# The IOKit part (the real SCSI card) can only be tested on the Mac with the scanner.
set -euo pipefail

cd "$(dirname "$0")"
ROOT=..
BIN="${CEZANNE_BIN:-}"
if [ -z "$BIN" ]; then
  (cd "$ROOT" && cargo build --release -p cezanne-bridge -p cezanne-cli >/dev/null 2>&1)
  BIN="$ROOT/target/release"
fi
[ -x ./cezanne-bridge-relay ] || make relay >/dev/null

TMP=$(mktemp -d)
RUST_PORT=17810
C_PORT=17811
PIDS=()
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT

"$BIN/cezanne-bridge" --sim --listen "127.0.0.1:$RUST_PORT" >"$TMP/rust.log" 2>&1 &
PIDS+=($!)
./cezanne-bridge-relay --relay "127.0.0.1:$RUST_PORT" --listen "127.0.0.1:$C_PORT" --token secret >"$TMP/c.log" 2>&1 &
PIDS+=($!)
sleep 1

fail() {
  echo "FAIL: $*"
  [ -f "$TMP/probe.txt" ] && cat "$TMP/probe.txt"
  echo "--- C bridge log"
  cat "$TMP/c.log"
  exit 1
}

echo "== probe through the C bridge"
"$BIN/cezanne-cli" probe --bridge "127.0.0.1:$C_PORT" --token secret >"$TMP/probe.txt" 2>&1 || fail "probe"
grep -q "FT-S5000" "$TMP/probe.txt" || fail "probe did not identify the scanner"
echo "ok"

echo "== scan through the C bridge and straight from the simulator"
ARGS=(scan --dpi 300 --film color-negative --auto)
"$BIN/cezanne-cli" "${ARGS[@]}" --bridge "127.0.0.1:$C_PORT" --token secret -o "$TMP/bridge.tif" >/dev/null 2>&1 || fail "scan through the bridge"
"$BIN/cezanne-cli" "${ARGS[@]}" --sim -o "$TMP/direct.tif" >/dev/null 2>&1 || fail "direct scan"
cmp -s "$TMP/bridge.tif" "$TMP/direct.tif" || fail "the TIFF through the bridge differs from the direct scan"
echo "ok, identical ($(wc -c <"$TMP/bridge.tif" | tr -d ' ') bytes)"

echo "== wrong token and blocked commands"
python3 -I - "$C_PORT" <<'EOF' || fail "raw protocol checks"
import socket, struct, sys
port = int(sys.argv[1])

def connect(token):
    s = socket.create_connection(("127.0.0.1", port), timeout=10)
    s.sendall(b"CZB1" + struct.pack(">H", len(token)) + token)
    def read(n):
        b = b""
        while len(b) < n:
            c = s.recv(n - len(b))
            if not c:
                raise EOFError
            b += c
        return b
    assert read(4) == b"CZB1"
    ok = read(1)[0]
    desc = read(struct.unpack(">H", read(2))[0]).decode()
    return s, read, ok, desc

s, read, ok, desc = connect(b"wrong")
assert ok == 1 and desc == "wrong token", (ok, desc)
s.close()

s, read, ok, desc = connect(b"secret")
assert ok == 0, desc
def command(cdb, out=b""):
    s.sendall(bytes([1, len(cdb)]) + cdb + bytes([2 if out else 0]) + struct.pack(">II", len(out), 5000) + out)
    status, transferred, sense_len = struct.unpack(">BIB", read(6))
    sense = read(sense_len)
    return status, sense

blocked = [
    bytes([0x3B, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    bytes([0xC9, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    bytes([0x2A, 0, 0x8A, 0, 0, 1, 0, 0, 16, 0]),
    bytes([0x2A, 0, 0x8B, 0, 0, 1, 0, 0, 16, 0]),
    bytes([0x2A, 0, 0x8C, 0, 0, 1, 0, 0, 16, 0]),
    bytes([0x2A, 0, 0x03, 0, 1, 1, 0, 5, 0xFA, 0]),
]
for cdb in blocked:
    status, sense = command(cdb, b"\0" * 16)
    assert status == 2 and sense[2] == 5 and sense[12] == 0x26, (cdb.hex(), status, sense.hex())
# INQUIRY with a data in phase still works on the same connection
s.sendall(bytes([1, 6, 0x12, 0, 0, 0, 36, 0, 1]) + struct.pack(">II", 36, 5000))
status, transferred, sense_len = struct.unpack(">BIB", read(6))
read(sense_len)
data = read(transferred)
assert status == 0 and transferred == 36 and data[8:14] == b"SCREEN", (status, transferred, data)
s.close()
print("ok")
EOF

N=$(grep -c "refused command from client" "$TMP/c.log" || true)
[ "$N" = 6 ] || fail "expected the C bridge to log 6 refused commands, it logged $N"
if grep -q "refused command" "$TMP/rust.log"; then
  fail "a blocked command reached the Rust bridge"
fi
echo "ok, all 6 refused by the C bridge itself"
echo
echo "All checks passed."
