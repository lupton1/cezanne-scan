# Status

Version 0.2.0, October 2026.

## Verified

| What | How |
|---|---|
| Protocol builders, window descriptor, sense, inquiry, gamma | unit tests |
| Full scan through the driver (set window, scan, read exact byte count) | tests against the simulator |
| Bridge client and server, token check, refusal of firmware writes from a raw client | loopback tests |
| Image pipeline (positive, flipped negative, auto levels, levels, curve, rotate, channel pick) | unit tests |
| TIFF and ICC output | unit tests and visual checks |
| Desktop app end to end (connect, prescan, frames, scan, save) | scripted demo run against the simulator |
| Windows installer | built locally, installed per user without admin, app and tools run, uninstalled cleanly |
| Linux and macOS code | type checked locally for both targets, packaged by GitHub Actions |

## Confirmed from SCREEN's own driver (version 0.2.0)

The FT-S5000 Driver 2.1.3 from the original Mac installation was analysed and the window
builder now writes the same bytes as ColorGeniusEX:

* scan area offsets are in 0.1 mm
* the resolution fields are dpi times 10
* width and length are left at zero, the scanner derives the extent from pixel count and
  resolution
* the constant bytes, scan mode, lookup table selectors and lamp selection per original
  type
* the SET WINDOW, SCAN and READ command layouts, which were already right

## Not yet verified

**Nothing has run on a real FT-S5000 yet.** The protocol is now known from both sides
(firmware and SCREEN's driver), so the remaining risk is mostly in the hardware path:

1. **SCSI card and transport.** Linux SG_IO and Windows SPTI follow their documented
   interfaces but have not seen this scanner.
2. **Focus.** The driver computes per scan focus offsets (bytes 149 to 159). Cezanne Scan
   sends zero there, which is what the driver does for a prescan, so the scanner uses its
   default focus. Automatic focus can be added once real scans can be compared.
3. **Filters and sharpening in the scanner** (bytes 66 to 71 and 132 to 141) are left off.
   Sharpening is done on the computer instead.
4. **Gamma tables** for the 8 bit modes are not downloaded. Scan in 16 bit.

The first test steps are in [hardware.md](hardware.md). The output of `cezanne-cli probe`
from the real scanner is still the most useful thing to have.

## Continuous integration

The GitHub Actions workflow builds the Windows, macOS and Linux installers. The first
packaging run was started by the commit "Packaging, CI and documentation [package]" on
2026-10-15. Its result has not been checked yet (the repository is private and was not
viewable from the build machine). Check the Actions tab. The installers are attached to the
run as artifacts. If a job failed, its log shows why.

A locally built Windows installer (from the LLVM MinGW toolchain, see building.md) was
tested on 2026-10-15: per user install without admin rights, app demo run, CLI and bridge
run, clean uninstall.
