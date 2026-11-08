# Server mode: scanning from any browser

`cezanne-server` runs on the computer the scanner is plugged into and serves the whole
Cezanne Scan interface as a web page. Any computer, tablet or phone on the same network
opens the page, prescans, sets up frames and scans. The finished TIFF files download to
the computer that is using the page, with the same 16 bit data and embedded ICC colour
profile the desktop app writes.

The intended setup is a Mac mini next to the scanner, running as a headless scan server:

```
 Browser (any computer)  ── network ──>  Mac mini running cezanne-server  ── SCSI ──>  FT-S5000
   page, frames, download                 owns the scanner, prescans,
                                          scans, writes the TIFF
```

It works the same on Linux and Windows, and it can also reach the scanner through a
`cezanne-bridge` on another computer.

## What the page does

Everything the desktop app does:

* connect (This computer, Bridge or Simulator), find scanners, probe
* film type and presets, including saving new presets on the server
* whole bed prescan, drawing frames with the mouse or a finger, editing their position
* per frame HD and SD, highlight and shadow droppers, Auto, mid tone, MIN and MAX, the
  curve editor, sharpening, channel pick, rotate and mirror, with a live processed preview
* densitometer readout under the pointer
* scan one frame or all frames, with progress and Cancel
* a **Scans** list of finished files on the server, to download again or remove

Frames and settings are kept in the browser, so reloading the page or coming back later
carries on where you left off. Only one scan runs at a time. If a second person opens the
page while a scan is running they see its progress and cannot start another.

When **Download to this computer when each scan finishes** is ticked, each file downloads
as soon as it is written. The browser may ask once whether the page may download several
files. A copy also stays on the server (in `~/Pictures/Cezanne Scans` unless changed) until
you press **Remove** in the Scans list, so a download that went wrong can be repeated.

## Setting up a Mac mini

1. **Connect the scanner.** The Mac needs a SCSI card with a macOS driver, see
   [hardware.md](hardware.md). Alternatively the scanner stays on a Linux or Windows
   computer running `cezanne-bridge` and the Mac connects to that (use
   `--connect bridge:HOST` below). An older Mac with the SCSI card, down to Mac OS X 10.6,
   can be that computer with the C bridge from [bridge-c](../bridge-c/README.md).
2. **Install Cezanne Scan** 0.4.0 or later into Applications. `cezanne-server` is inside
   the app. Or build it on the Mac from the source folder:

   ```bash
   cargo build --release -p cezanne-server
   ```

3. **Install the service** from the source folder (or from wherever
   `install-server.sh` was copied):

   ```bash
   sudo bash packaging/macos/install-server.sh --token choose-a-secret
   ```

   The script copies `cezanne-server` to `/usr/local/libexec/cezanne-scan`, installs a
   launch daemon that starts it when the Mac boots (no login needed) and restarts it if it
   stops, lets it through the macOS firewall if the firewall is on, and prints the address
   to open. The service runs as your user, so the scans belong to you.

   Options: `--port` (7820 unless given), `--connect` (`auto` unless given, or `sim`, a
   device address, or `bridge:HOST`), `--bridge-token`, `--output-dir`, `--binary`. Run it again after
   updating Cezanne Scan. `sudo bash packaging/macos/install-server.sh --uninstall` removes the
   service and keeps the scans.

4. **Keep the Mac awake.** A sleeping Mac stops serving the page. Use
   `sudo pmset -a sleep 0`, or System Settings, Energy, Prevent automatic sleeping. The
   script reminds you if sleep is on.

5. **Open the page** on another computer: `http://<mac-name>.local:7820`, for example
   `http://mac-mini.local:7820`, or the Mac's IP address. Enter the token once. The browser
   remembers it.

The log is in `/Library/Logs/cezanne-server.log`.

To try it first without the service, run it by hand:

```bash
"/Applications/Cezanne Scan.app/Contents/MacOS/cezanne-server" --token choose-a-secret
```

and press Ctrl+C to stop it.

## Linux

The `.deb` installs `/usr/bin/cezanne-server`. Copy
[packaging/linux/cezanne-server.service](../packaging/linux/cezanne-server.service) to
`/etc/systemd/system/`, set `User` to your account (it must be in the `scanner` group) and
the token, then:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cezanne-server
```

## Windows

`cezanne-server.exe` is installed next to the app. Direct SCSI on Windows needs
administrator rights, so start it from an administrator command prompt:

```bash
cezanne-server --token choose-a-secret
```

Windows asks once whether to allow it through the firewall. Allow it on private networks.

## Command line

```
cezanne-server [--listen 0.0.0.0:7820] [--token SECRET] [--output-dir DIR]
               [--connect auto|sim|DEVICE|bridge:HOST] [--bridge-token SECRET]
```

| Option | Meaning |
|---|---|
| `--listen` | address and port, `0.0.0.0:7820` by default (every network interface) |
| `--token` | access token for browsers, also read from `CEZANNE_SERVER_TOKEN` |
| `--output-dir` | where scans are written on the server |
| `--connect` | connect at start up, otherwise press Connect on the page |
| `--bridge-token` | token for `--connect bridge:HOST`, also `CEZANNE_BRIDGE_TOKEN` |

## Security

* Without `--token` anyone on the network can use the scanner and download the scans.
  The server warns about this when it starts. Set a token on any shared network.
* The token is checked on every request. The page sends it once and then keeps it in a
  cookie that scripts cannot read.
* The page uses plain HTTP. It is meant for a home or studio network, not for the
  internet. Do not forward the port on your router. For remote use, connect to the
  network with a VPN first.
* File names come from the server (prefix plus a number), and the prefix is reduced to
  plain characters, so a browser cannot write files outside the output folder.
* The safety rules of the driver apply as everywhere else: commands that would write
  firmware or factory data are never sent.

## How it works

`cezanne-server` is a small axum web server with the page (plain HTML, CSS and
JavaScript) built into the program, so there is nothing else to install. One thread owns
the scanner, as in the desktop app. The page polls `/api/state` for progress, the log and
the list of finished files.

| Endpoint | Purpose |
|---|---|
| `GET /api/info`, `POST /api/login` | whether a token is needed, and logging in |
| `GET /api/meta` | film types with their starting adjustments, colour modes |
| `GET /api/state` | connection, progress, log lines, finished files |
| `POST /api/connect`, `/api/disconnect`, `/api/probe`, `GET /api/devices` | the scanner connection |
| `POST /api/prescan`, `GET /api/prescan.png` | whole bed prescan and its display image |
| `GET /api/density?x=&y=` | densities at a point on the bed (mm) |
| `POST /api/preview` | processed preview of a frame, with automatic levels if asked |
| `POST /api/scan`, `POST /api/cancel` | scan frames, cancel |
| `GET /api/files/{id}`, `DELETE /api/files/{id}` | download or remove a finished scan |
| `GET /api/presets`, `POST /api/presets` | presets |
