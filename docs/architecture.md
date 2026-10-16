# Architecture

```
 cezanne-app (egui)      cezanne-cli        cezanne-bridge
        |                     |                   |
        +----------+----------+-------------------+
                   |
             cezanne-core
   +---------------+-----------------+------------------+
   | scanner       | protocol        | image            |
   | (driver)      | cdb, window,    | pipeline, tiff,  |
   |               | gamma, inquiry  | icc, png         |
   +-------+-------+-----------------+------------------+
           |
         scsi::Transport
   +-------+--------+-----------+-------------+
   | linux_sg     | windows   | macos_iokit   | tcp       | sim
   | SG_IO ioctl  | SPTI      | SCSITaskLib   | bridge    | simulated FT-S5000
```

## Layers

**`scsi`** moves one CDB with an optional data phase and returns the status byte and sense
data. Every transport implements the same `Transport` trait, so everything above it is the
same on every system.

* `linux_sg` uses the SCSI generic driver (`/dev/sg*`) and the SG_IO ioctl.
* `windows_spti` uses the SCSI Pass Through Interface on the adapter (`\\.\ScsiN:`) with
  an explicit path, target and LUN, and discovers devices with IOCTL_SCSI_GET_INQUIRY_DATA
  so discovery sends nothing to the devices.
* `macos_iokit` opens the scanner through IOKit's SCSITaskDeviceInterface. macOS
  publishes SCSI devices it has no driver for (scanners included) as
  `SCSITaskUserClientDevice` nubs, which an application can open with exclusive access.
  Addresses look like `iokit:<registry entry id>`.
* `tcp` is the client and server for `cezanne-bridge`. The wire format is documented in
  the module.
* `sim` answers the FT-S5000 command set with the firmware's validation rules and renders
  synthetic film (a colour negative with orange mask, a black and white negative, a slide
  and a step wedge).

**`protocol`** builds CDBs and parses or builds the scanner's data structures. The window
descriptor builder writes the same bytes as SCREEN's own driver (`SetJobToDevice` in
"FT-S5000 Driver" 2.1.3): area offsets in 0.1 mm, resolution as dpi times 10, pixel and
line counts, the scan mode, lookup table selectors and the lamp for each original type.
`AreaUnit` and `resolution_scale` remain as diagnostic options only.

**`scanner`** is the driver: identify, clear unit attention, wait for warm up, reserve,
set window, scan, read the exact byte count in whole lines, release.

**`image`** turns the linear 16 bit scanner data into finished images. The tone model is
the one ColorGeniusEX users know: per channel highlight and shadow densities, then mid tone
gamma, a monotone cubic curve, MIN and MAX output levels, optional unsharp mask, channel
selection, rotation and mirroring. TIFF and ICC writers have no dependencies.

## Safety

`scsi::check_allowed` refuses WRITE BUFFER (firmware download), the vendor maintenance
write (0xC9), and SEND with data types that end up in EEPROM (memory switches, factory
parameters, serial number) or with the save to EEPROM bit. It runs in the driver, in the
bridge server for every incoming CDB, and in the simulator. No build of Cezanne Scan can
send these commands.

## Threads

The app keeps the scanner on a background thread (`worker.rs`). The UI sends requests
(connect, prescan, scan a list of frames, probe) and receives events (progress, prescan
image, saved file, errors). Cancelling sets a shared flag that the driver checks between
reads.

## Data flow of a scan

1. The app builds `WindowSettings` (area in mm, dpi, colour mode, the scanner's original
   type derived from the film type, and whether it is a prescan).
2. `WindowDescriptor::build` turns that into the 176 byte descriptor and the expected
   geometry (pixels per line, lines, bytes per pixel).
3. The driver waits for the lamp to warm up, reserves the scanner, validates the
   descriptor with the firmware's own limits, sends SET WINDOW and SCAN, then READ(10) in
   chunks of whole lines (up to 256 KB) until the expected byte count arrives, and
   releases the scanner.
4. `RawImage::from_scanner` decodes big endian samples (8 bit modes are linearised).
5. `Adjustments::apply` produces the final image, which is written as TIFF with an ICC
   profile.

## Reverse engineering notes

See [protocol.md](protocol.md). Sources: the scanner firmware (Hitachi SH-2, unpacked
from the LHA archive inside `FT-S5000_V2.12.jfirm`), SCREEN's host driver "FT-S5000 Driver"
2.1.3 (PowerPC PEF with debug function names, plus its `FT-S5000 Config` file), the
Maintainer tool and its JNI shim. Tools: Vineflower for the Java classes, Ghidra 12 for the
PowerPC and SH-2 code. No decompiled SCREEN code is kept in the repository.
