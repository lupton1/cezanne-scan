# Hardware guide

## What the scanner needs

The FT-S5000 is a SCSI-2 device with a 50 pin connector. Any computer that runs Cezanne
Scan directly needs:

1. A SCSI host adapter with a driver for that system
2. A cable or adapter from the card's connector to the scanner's 50 pin connector
3. Correct termination at both ends of the bus

## Choosing a SCSI card

| System | Recommendation |
|---|---|
| **Linux** | The most reliable choice. PCIe cards based on the LSI 53C1030 (for example LSI20320IE, driver `mptspi`) or Adaptec AIC-79xx (29320LPE, driver `aic79xx`) work with current kernels. Older PCI Adaptec 29160 and 29160N cards (driver `aic7xxx`) also work in machines with a PCI slot. Cezanne owners have used the Adaptec 29160N with this scanner. |
| **Windows 10 and 11** | Possible but less certain. Check that a 64 bit driver exists for the exact card before buying. Running Cezanne Scan as administrator is required for direct SCSI access. |
| **macOS** | Current Macs cannot drive this scanner directly. Use a small Linux computer with a SCSI card as a bridge, see [Using a Mac](../README.md#using-a-mac) in the README. |

A small Linux box with a PCIe slot, or an old desktop, makes a good dedicated bridge for a
Mac. Any distribution with a recent kernel will do.

## Cabling and termination

* Ultra320 and Ultra160 cards have a 68 pin connector. Use a 68 to 50 pin cable or
  adapter that terminates the unused high byte, or a 68 pin to 50 pin Centronics cable
  made for scanners.
* The bus needs a terminator at each end. The card terminates its end automatically. Make
  sure the scanner end is terminated (internal termination switch or an external
  terminator on the second port).
* Use the shortest good cable you can.

## SCSI ID

The original Maintainer log shows the scanner at bus 0, ID 0. Any free ID works, as long as
it is different from the card (usually ID 7) and any other device on the bus.

## First test on real hardware

The protocol is known from the scanner firmware and from SCREEN's own driver (see
[STATUS.md](STATUS.md)). These steps are all safe. Nothing here writes to the scanner's
memory.

1. Power the scanner on, wait for it to finish its start up, then start the computer.
2. Find it:

   ```bash
   cezanne-cli list
   ```

3. Save a read only report. Please keep this output, it is the best record of how the
   scanner answered:

   ```bash
   cezanne-cli probe > probe.txt
   ```

4. A small test scan of the top left corner of the bed:

   ```bash
   cezanne-cli scan --area 0,0,50,50 --dpi 300 --raw -o test-corner.tif
   ```

   The file should be 590 by 590 pixels and show the top left corner of the bed.

5. Put a ruler on the glass and scan a known area, for example `--area 0,0,100,20`. The
   picture should show 100 mm of the ruler.

The **Advanced (diagnostics)** settings in the app (area unit and resolution field scale)
and the matching `--area-unit` and `--res-scale` options of `cezanne-cli` are diagnostic
only. The defaults match SCREEN's driver.

## The lamp warm up

SCREEN's driver waits 180 seconds after switching lamps. Cezanne Scan waits while the
scanner reports that it is not ready (on TEST UNIT READY before the scan and on SCAN
itself), for up to five minutes. The first scan after
switching the scanner on, or after changing between film and prints, therefore takes a
few minutes longer.

## Never do this

Do not use other tools to send WRITE BUFFER (0x3B) or vendor command 0xC9 to the scanner.
They rewrite firmware and factory calibration. Cezanne Scan blocks them in every layer.
