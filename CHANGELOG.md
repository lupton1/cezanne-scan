# Changelog

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
