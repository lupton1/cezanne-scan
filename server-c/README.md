# cezanne-server for old Macs (Mac OS X 10.6 and later)

This folder holds `cezanne-server` written in plain C, for the old Mac that has the SCSI
card for the FT-S5000, the one that ran ColorGeniusEX on Mac OS X 10.6 Snow Leopard. That
Mac becomes the scan server:

```text
browser on any computer, tablet or phone  ->  network  ->  old Mac (cezanne-server)  ->  SCSI card  ->  scanner
```

It talks to the scanner on the Mac's own SCSI card and serves the Cezanne Scan web page to
every device on the network. Finished scans download to the device you scan from, as 16
bit TIFF files tagged with the scanner's colour space (SCREEN's 5000stdRGB), the way
ColorGeniusEX saved them. No other computer and no bridge are needed.

The Rust `cezanne-server` that comes with Cezanne Scan needs macOS 10.15 or later (Rust
itself cannot build for anything older than 10.12). This is a line by line port of it and
of the parts of the Cezanne Scan core it uses:

| C file | Rust original |
|---|---|
| `server.c` | `crates/cezanne-server` (web server, API, scanner thread) |
| `scanner.c` | `protocol/window.rs`, `protocol/cdb.rs`, `scanner.rs` |
| `scsi.c` | `scsi/mod.rs`, `scsi/macos_iokit.rs`, `scsi/tcp.rs` |
| `image.c` | `image/pipeline.rs`, `image/icc.rs`, `image/tiff.rs`, `image/png.rs` |
| `json.c`, `util.c` | serde_json and the standard library |

The web page is the same one (`crates/cezanne-server/web`), built into the program, so
everything in the manual's web page chapter applies. The arithmetic is done in the same
order and precision as the Rust code, and the two give byte for byte identical files (see
Testing below).

It refuses WRITE BUFFER (0x3B), vendor command 0xC9 and any SEND that saves to the
scanner's EEPROM, like every other part of Cezanne Scan.

**Status:** tested against the simulator through a bridge, on Windows. Every preview and
every saved TIFF matched the Rust server byte for byte. The SCSI side uses Apple's
SCSITaskDeviceInterface like the Rust macOS transport and the C bridge. It has not yet run
on Mac OS X 10.6 or with the real scanner.

## What is different from the Rust server

* **No built in simulator.** Choosing Simulator on the page explains this. To try the page
  without the scanner, run `cezanne-bridge --sim` from Cezanne Scan on another computer
  and connect to it as a Bridge.
* `--connect sim` is not available for the same reason.
* `cezanne-server --list` lists the SCSI devices the Mac can see, like `cezanne-bridge --list`.
* Some numbers in the page's settings come back in shortest form (0.15 rather than
  0.15000000596046448). They are the same values.

## Building

On a current Mac with the Xcode command line tools, from this folder:

```bash
make
```

This writes `cezanne-server`, a 64 bit Intel program for Mac OS X 10.6 and later, with the
web page inside it. Like the C bridge it uses Apple's classic linker (`-ld_classic`),
because only that one still writes the program entry that 10.6 understands.

On the 10.6 Mac itself, with Xcode 3.2 installed:

```bash
make native
```

This builds for 32 and 64 bit Intel. Use the 64 bit Macs for large scans, a 32 bit
program cannot hold a big scan in memory.

## Installing on the old Mac

Copy `cezanne-server` and `install.sh` into one folder on the old Mac, switch the scanner on
before the Mac, quit ColorGeniusEX, and in Terminal (Applications, Utilities) in that
folder:

```bash
./cezanne-server --list
```

The scanner should appear as `SCREEN FT-S5000` marked "supported scanner". If the list is
empty, check the card, its driver, the cable and termination (see
[docs/hardware.md](../docs/hardware.md)). Then install the service:

```bash
sudo bash install.sh --token choose-a-secret
```

It starts at once and whenever the Mac starts, runs as your user, keeps scans in
`~/Pictures/Cezanne Scans` and logs to `/Library/Logs/cezanne-server.log`. It prints the
address to open, for example `http://old-mac.local:7820`. Open that in a browser on any
other computer, tablet or phone on the same network and enter the token once.

The browser must be a current one (Safari, Chrome, Firefox or Edge from the last few
years). Safari on the 10.6 Mac itself is too old for the page.

To run it by hand instead, for a first test:

```bash
./cezanne-server --connect auto --token choose-a-secret
```

To remove the service: `sudo bash install.sh --uninstall`. Scans already made are kept.

### Options

| Option | Meaning |
|---|---|
| `--listen HOST:PORT` | address to listen on, default `0.0.0.0:7820` (every network interface) |
| `--token SECRET` | access token browsers must enter once, also read from `CEZANNE_SERVER_TOKEN` |
| `--output-dir DIR` | folder for finished scans, default `~/Pictures/Cezanne Scans` |
| `--connect WHAT` | connect at start up: `auto`, a device from `--list`, or `bridge:HOST` |
| `--bridge-token SECRET` | token for `--connect bridge:HOST`, also read from `CEZANNE_BRIDGE_TOKEN` |
| `--list` | list the SCSI devices the Mac can reach and exit |

## Testing

`compare.py` runs this server and the Rust one side by side, each against its own
`cezanne-bridge --sim`, and checks that they answer every API call the same and save byte
identical files. It covers the access token, the page files, the film defaults and
presets, error answers, the prescan image, the densitometer, 12 frame previews (every film
type, auto levels, curves, sharpening, rotation, mirroring, greyscale modes), scans in all
four colour modes with 16 and 8 bit output and one 26 MB scan at 1800 dpi, downloads,
removing files, finding earlier scans after a restart and saving a preset.

On a Mac with the Rust tools, from this folder:

```bash
make host
python3 compare.py --c ./cezanne-server-host --rust ../target/release/cezanne-server \
    --bridge ../target/release/cezanne-bridge
```

(build the Rust programs first with `cargo build --release -p cezanne-server -p
cezanne-bridge`). 164 checks passed on Windows on 2026-11-08.
