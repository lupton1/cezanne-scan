# Changelog

## 0.4.0 (2026-11-02)

* Server mode: `cezanne-server` runs on the computer the scanner is connected to (for
  example a Mac mini) and serves the full Cezanne Scan interface as a web page. Any
  browser on the network can prescan, set up frames, adjust them and scan, and the
  finished 16 bit TIFF files with their ICC profile download to that browser's computer.
  A copy stays on the server until removed. Optional access token
* `packaging/macos/install-server.sh` installs the server as a launch daemon that starts
  when the Mac boots, and a systemd unit is included for Linux
* The installers now include `cezanne-server`
* The scan server lists the scans already in its folder when it starts, so files made
  before a restart of the server or the Mac can still be downloaded
* The scan page says when a scan starts, and its phone layout is tidier
* `install-server.sh --bridge-token` for a server that reaches the scanner through a bridge
* A user manual with pictures: [docs/manual.md](docs/manual.md)
* Colour now matches ColorGeniusEX: finished scans carry a profile with the exact
  colorimetry of SCREEN's 5000stdRGB (the FT-S5000's own RGB space, gamma 2.289), which
  the owner's ColorGeniusEX recipes used as both input and output profile. Earlier
  versions tagged files as sRGB primaries with gamma 2.2, so colour managed editors showed
  them less saturated than ColorGeniusEX scans. Previews are converted for the screen, and
  the Grey button uses this space's luminance weights

## 0.3.1 (2026-10-19)

* Windows: the exe now statically links the C runtime, so it no longer needs the Microsoft
  Visual C++ Redistributable (VCRUNTIME140.dll). A CI built release could fail to start on a
  machine without that runtime. This release ships a self contained Windows installer. macOS and Linux are unchanged from v0.3.0

## 0.3.0 (2026-10-16)

* macOS: direct connection to a scanner on a SCSI card through IOKit's SCSI task
  interface (choose This computer). Not yet tested on hardware. The bridge remains the
  option for Macs without a usable SCSI card

## 0.2.2 (2026-10-16)

* macOS: the app is now universal and runs on Intel Macs as well as Apple Silicon (0.2.1
  and earlier only ran on Apple Silicon). It is signed ad hoc and the minimum is macOS 10.15
* Install instructions for macOS 15 Sequoia, which removed the right click Open shortcut
  for apps without a Developer ID

## 0.2.1 (2026-10-16)

Fixes found while reviewing the documentation against the code.

* Negative film types are scanned with the scanner's Positive Film type and inverted on
  the computer, matching the documented owner workflow
* SCAN is retried while the scanner reports NOT READY (lamp warm up), like SCREEN's driver
* Documentation reviewed against the code (architecture, building, hardware, status,
  protocol)

## 0.2.0 (2026-10-16)

Window descriptors now match SCREEN's own driver byte for byte.

* Analysed "FT-S5000 Driver" 2.1.3 (the host driver ColorGeniusEX uses). Confirmed the
  0.1 mm area unit and the dpi times 10 resolution fields
* Width and length are no longer sent, the scanner derives the extent from pixels and
  resolution, as with the original driver
* Added the constant vendor bytes, prescan and scan modes, lookup table selectors and lamp
  selection for each original type (positive film, negative film, photograph, printed
  material, watercolor)
* Fixed byte 142, which is the original class and not a gamma slot
* SET WINDOW header now carries the window data length like the driver's
* Window IDs start at 1, pixel counts round down so a scan never runs past its area
* MIT licence
* Releases are published with installers for Windows, macOS and Linux

## 0.1.0 (2026-10-15)

First version.

* FT-S5000 SCSI protocol decoded from the scanner firmware: command set, window descriptor,
  SEND data types, vendor commands, sense codes
* `cezanne-core`: Linux SG_IO, Windows SPTI, network bridge and simulator transports, the
  scanner driver, the image pipeline, TIFF and ICC output, presets
* `cezanne-scan` desktop app: prescan, frames, per frame HD and SD, droppers, curves,
  levels, sharpening, black and white channel pick, batch scanning
* `cezanne-cli`: list, probe, sense, scan, presets
* `cezanne-bridge`: share a scanner over the network for Macs and other computers
* Installers: Windows NSIS (per user), macOS app and dmg, Linux deb and AppImage
* Firmware and EEPROM writes are blocked in every layer
