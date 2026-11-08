# cezanne-bridge for old Macs (Mac OS X 10.6 and later)

> If the old Mac should be the scan server itself, serving the web page to the other
> devices on the network, use [server-c](../server-c/README.md) instead. This bridge is
> for when Cezanne Scan or `cezanne-server` runs on another, newer computer.

Cezanne Scan, `cezanne-server` and the Rust `cezanne-bridge` need macOS 10.15 or later
(Rust itself cannot build for anything older than 10.12). Many Macs that still have a
working SCSI card for the FT-S5000 run Mac OS X 10.6 Snow Leopard, often the Mac that ran
ColorGeniusEX under Rosetta.

This folder holds a small `cezanne-bridge` written in plain C that runs on Mac OS X 10.6
and later. It does one job: it opens the scanner on the Mac's SCSI card and shares it over
the network with the same protocol as the Rust bridge. Everything else runs on a newer
computer:

```text
browser on any device  ->  cezanne-server (newer Mac, PC or Linux)  ->  network  ->  cezanne-bridge (10.6 Mac)  ->  SCSI card  ->  scanner
```

Cezanne Scan on a newer computer can use the bridge directly as well (choose **Bridge**).

Like the Rust bridge it serves one client at a time and refuses WRITE BUFFER (0x3B),
vendor command 0xC9 and any SEND that saves to the scanner's EEPROM, whatever the client
sends.

**Status:** the network side has been tested against the simulator (see Testing below).
The SCSI side follows Apple's SCSITaskDeviceInterface like the Rust macOS transport, but
it has not yet run on Mac OS X 10.6 or with the real scanner.

## Building

On a current Mac with the Xcode command line tools, from this folder:

```bash
make
```

This writes `cezanne-bridge`, a 64 bit Intel program for Mac OS X 10.6 and later. It uses
Apple's classic linker (`-ld_classic`), because only that one still writes the program
entry that 10.6 understands. If a future Xcode drops it, build on the old Mac instead.

On the 10.6 Mac itself, with Xcode 3.2 installed:

```bash
make native
```

This builds for 32 and 64 bit Intel. The 32 bit part matters only for the first Intel
Macs with a Core Duo or Core Solo processor.

Copy `cezanne-bridge` to the old Mac, for example into `/usr/local/bin`. It needs no
installer and no other files.

## Running

Switch the scanner on before the Mac, then in Terminal on the old Mac:

```bash
cezanne-bridge --list
```

The scanner should appear as `SCREEN FT-S5000` marked "supported scanner". If the list is
empty, check the card, its driver, the cable and termination (see
[docs/hardware.md](../docs/hardware.md)). Then start the bridge:

```bash
cezanne-bridge --listen 0.0.0.0:7810 --token choose-a-secret
```

Quit ColorGeniusEX or any other scanning software first. The bridge needs the scanner to
itself while a client is connected and releases it when the client disconnects.

| Option | Meaning |
|---|---|
| `--listen HOST:PORT` | address to listen on, default `127.0.0.1:7810`. Use `0.0.0.0:7810` for the network |
| `--token SECRET` | shared secret, also read from `CEZANNE_BRIDGE_TOKEN`. Set one on any shared network |
| `--device iokit:ID` | the scanner to use, from `--list`. Default: the first SCREEN scanner found |
| `--list` | list the SCSI devices the Mac can reach and exit |

## Connecting to it

The scan server on a newer computer:

```bash
cezanne-server --connect bridge:old-mac.local --bridge-token choose-a-secret
```

or, for the Mac mini service, `sudo bash packaging/macos/install-server.sh --connect
bridge:old-mac.local --bridge-token choose-a-secret --token another-secret`.

Cezanne Scan: choose **Bridge**, enter the old Mac's name or address and the token, and
press **Connect**.

Use the old Mac's address (System Preferences, Sharing shows its name) and a wired network
for high resolution scans.

## Starting it when the Mac starts

Save this as `/Library/LaunchDaemons/com.lupton1.cezanne-bridge.plist`, owned by root,
with the path and token changed to yours:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.lupton1.cezanne-bridge</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/cezanne-bridge</string>
        <string>--listen</string>
        <string>0.0.0.0:7810</string>
        <string>--token</string>
        <string>choose-a-secret</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardErrorPath</key>
    <string>/var/log/cezanne-bridge.log</string>
</dict>
</plist>
```

Then load it once:

```bash
sudo launchctl load -w /Library/LaunchDaemons/com.lupton1.cezanne-bridge.plist
```

The log is in `/var/log/cezanne-bridge.log`.

## Testing

```bash
make check
```

builds a test version that forwards to the Rust `cezanne-bridge --sim` instead of a SCSI
card and checks, on any Mac with the Rust tools:

* a probe and a scan through this bridge give a TIFF identical to a scan straight from the
  simulator
* a wrong token is refused
* the six blocked command forms are refused by this bridge itself and never reach the one
  behind it

The test version (`cezanne-bridge-relay`) is for testing only and must not be copied to
the old Mac.
