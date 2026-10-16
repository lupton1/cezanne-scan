# Changelog

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
