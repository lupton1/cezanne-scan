# Status

Version 0.1.0, October 2026.

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

## Not yet verified

**Nothing has run on a real FT-S5000 yet.** The firmware analysis gives the command set,
data formats and validation rules with good confidence. These points still need the real
scanner:

1. **Area unit.** The firmware limits (3420 by 5310) fit a bed of about 342 by 531 mm in
   0.1 mm units, which is very likely right. 1/1200 inch, the SCSI-2 default, would make
   the bed only 72 mm wide. Switchable in the app and CLI just in case.
2. **Resolution field scale.** The 32 bit resolution fields accept 200 to 120000, read as
   dpi times 10. Switchable.
3. **Vendor window fields** that the firmware reads but whose meaning is unclear (see the
   table in protocol.md). They are sent as zero, which the firmware accepts.
4. **Gamma table format** for the 8 bit modes. Cezanne Scan does not download gamma tables
   by default and scans in 16 bit.
5. **Windows SPTI** access to a scanner on a SCSI card. The code follows the documented
   interface but has not seen a real device.

The first test steps are in [hardware.md](hardware.md).

## What would help most

* The output of `cezanne-cli probe` from the real scanner.
* The file **"FT-S5000 Driver"** from the old Mac. It is the SCREEN driver that
  ColorGeniusEX uses and holds the exact window values the original software sends. It
  lives in a folder called **DS Drivers** (look in `/Library`, `~/Library` and the System
  Folder). It was not part of the archive used so far. It would stay outside the
  repository like the other SCREEN files.

## Continuous integration

The GitHub Actions workflow builds the Windows, macOS and Linux installers. If a run fails
its log is on the Actions tab of the repository.
