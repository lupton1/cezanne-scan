# FT-S5000 SCSI protocol

Status: work in progress. Derived from static analysis of the scanner firmware
(FT-S5000 V2.12, Hitachi SH-2) and the FT-S5000 Maintainer tool. No SCREEN code is
reproduced here, only the observed interface.

## Architecture

The scanner has two main processors.

* **JCONT** runs the SCSI target firmware (`jrom.com` inside `FT-S5000_V2.12.jfirm`).
  It is a Hitachi SH-2, big endian, ROM at `0x09000000`, RAM data at `0x090C0000`.
  The SCSI protocol chip is memory mapped at `0x0E000000`.
* **JOANA** drives the mechanics and optics (table, lens, CCD, iris, lamps). JCONT talks
  to it over an internal serial link (STX/ETX framing with ACK/NAK). The Maintainer's
  `SendJOANACommand` tunnels raw JOANA commands through SCSI.

The host side SCSI code lives in a CFM driver called `FT-S5000 Driver` (loaded from a
`DS Drivers` folder). It is not part of the material we have, so this document is built
from the firmware (target side).

## Identity (INQUIRY)

| Field | Value |
|---|---|
| Peripheral device type | 6 (scanner) |
| ANSI version / response format | 2 / 2 |
| Additional length | 0x20 |
| Vendor | `SCREEN  ` |
| Product | `FT-S5000        ` (sister model `FT-S3000`, code name SAMBA) |
| Revision | firmware version, for example `2.12` |
| Internal code name | MAMBO |

## Command set

Dispatcher: firmware function at `0x0901785C`. The CDB starts at offset 0x0C of the
firmware's request block.

| Opcode | Command | Notes |
|---|---|---|
| `0x00` | TEST UNIT READY | |
| `0x03` | REQUEST SENSE | |
| `0x12` | INQUIRY | EVPD bit selects a second (vendor) page |
| `0x16` | RESERVE UNIT | |
| `0x17` | RELEASE UNIT | |
| `0x1B` | SCAN | |
| `0x1D` | SEND DIAGNOSTIC | |
| `0x24` | SET WINDOW | |
| `0x25` | GET WINDOW | |
| `0x28` | READ(10) | data type code selects image or auxiliary data |
| `0x2A` | SEND(10) | data type code selects gamma, matrix and other tables |
| `0x3B` | WRITE BUFFER | firmware download. **Never used by Cezanne Scan.** |
| `0x4D` | LOG SENSE | |
| `0xC8` `0xC9` `0xCA` `0xCB` `0xCE` | vendor specific | see below |
| other | | CHECK CONDITION, ILLEGAL REQUEST, ASC 0x20 (invalid opcode) |

Pending unit attention conditions are reported on every command except INQUIRY and
REQUEST SENSE.

## Response and sense handling

Every command finishes with a status byte. On error the firmware builds fixed format
sense data (18 bytes): byte 0 `0x70` (current) or `0x71` (deferred), byte 2 sense key,
byte 7 additional length `0x0B`, byte 12 ASC, byte 13 ASCQ, bytes 15 to 17 sense key
specific. Codes seen in the firmware:

| Key | ASC/ASCQ | Meaning |
|---|---|---|
| 5 ILLEGAL REQUEST | 20/00 | invalid opcode |
| 5 ILLEGAL REQUEST | 24/00 | invalid field in CDB |
| 5 ILLEGAL REQUEST | 26/02 | parameter value invalid (bad window, gamma slot, SEND type) |
| 5 ILLEGAL REQUEST | 2C/00 | command sequence error (vendor reads) |
| 4 HARDWARE ERROR | 84/00 | EEPROM write failed |
| 4 HARDWARE ERROR | 88/xx | scan task error, xx is the internal error number |
| 6 UNIT ATTENTION | various | queued per initiator, reported on the next command |

## SET WINDOW (0x24) and GET WINDOW (0x25)

* CDB bytes 6 to 8: parameter list length (max `0x5000`). GET WINDOW: CDB byte 5 is the
  window ID.
* Parameter list: 8 byte header (stored verbatim) followed by one or more **176 byte
  (0xB0) window descriptors**. GET WINDOW returns header plus one descriptor (184 bytes).
* Window IDs 0 to 199 are accepted. A descriptor is stored only after it passes
  validation, otherwise SET WINDOW fails with 5/26/02.

### Window descriptor (176 bytes)

Offsets are from the start of the descriptor. Confidence: **high** means the firmware
reads the field with that size and meaning is clear from context. **medium** means size
and use are known but units are inferred. **low** means only that the firmware reads it.

| Offset | Size | Field | Confidence | Notes |
|---|---|---|---|---|
| 0 | 1 | window ID | high | |
| 2 | 2 | X resolution (SCSI-2 standard slot) | low | not read by the scan engine |
| 4 | 2 | Y resolution (SCSI-2 standard slot) | low | not read by the scan engine |
| 6 | 4 | upper left X | high | validated `0 <= ULX` and `ULX + width <= 3420` |
| 10 | 4 | upper left Y | high | validated `ULY + length <= 5310` |
| 14 | 4 | width | medium | |
| 18 | 4 | length | medium | |
| 29 | 1 | flags, bit 7 used | low | |
| 44 | 1 | original type selector | medium | checked by a model specific validator |
| 45 | 1 | scan flags (bit 0, bit 1, values 0x11 to 0x13 special) | low | bit 0 forces a 10 line mode, bit 1 doubles a speed factor |
| 46 | 2 | X resolution, dots per inch | medium | max 12000 |
| 48 | 2 | Y resolution, dots per inch | medium | used for the line count |
| 50 | 1 | image composition | high | 2 = grey, 5 = RGB (SCSI-2 codes) |
| 51 | 1 | bits per pixel | high | 16 = 16 bit grey, 48 = 16 bit RGB (8 bit values also accepted) |
| 54 | 4 | magnification or output resolution X | medium | range per model, FT-S5000 200 to 120000 |
| 58 | 4 | magnification or output resolution Y | medium | same range |
| 63 to 65 | 1 each | per channel parameter R, G, B | low | passed to a per channel setup routine |
| 66 | 2 | | low | |
| 70 | 2 | | low | |
| 132 | 1 | enable flag | low | |
| 133, 135, 136, 137 | 1 each | | low | 137 bits 0 and 1 select a table bank |
| 138, 140 | 2 each | | low | |
| 142 | 1 | gamma table selector | medium | index into downloaded gamma slots |
| 144 | 1 | light source | medium | 0 = transmissive lamp, 1 = reflective lamp, 2 = none |
| 149 | 1 | | low | |
| 154, 156, 158 | 2 each | | low | |
| 160 | 1 | | low | |

The 3420 by 5310 limits match a bed of about 342 by 531 mm in 0.1 mm units, which is
the working assumption until confirmed on hardware.

## SCAN (0x1B)

CDB byte 4 is the number of window IDs that follow (max 100). The data out phase is the
list of window IDs. Each must be defined by SET WINDOW, otherwise 5/26/02.

## READ(10) (0x28)

Returns image data for the active scan. CDB bytes 6 to 8 are the transfer length. The
firmware keeps a count of bytes remaining (lines times bytes per line, computed from the
window) and the host should read exactly that many bytes in total.

## SEND(10) (0x2A)

CDB byte 2 is the data type code, byte 5 the qualifier, bytes 6 to 8 the length. CDB byte
4 bit 0 asks the scanner to also save the table to its EEPROM.

| Type | Qualifier | Length | Content | Used by Cezanne Scan |
|---|---|---|---|---|
| `0x03` | 1 to 6 | up to 1530 | gamma tables, 3 channels of 510 bytes | yes |
| `0x84` | 1 to 4 | 60 | RGB colour matrix | yes, never with the save bit |
| `0x8A` | 1 | 16 | memory switches | **no** |
| `0x8B` | 1 | 16335 | factory mechanical parameters | **no** |
| `0x8C` | 1 | 10 to 16 | serial number | **no** |

## Vendor commands

| Opcode | Direction | Purpose |
|---|---|---|
| `0xC8` | in | vendor READ. Byte 2 type (`0x80` CCD or DSP data, `0x03` gamma, `0x84` matrix), bytes 4 and 5 qualifier, byte 3 bit 0 continues a chunked transfer |
| `0xC9` | out | maintenance write (function code in the first data word). Not used |
| `0xCA` | in | maintenance read (function code in CDB bytes 2 and 3). Diagnostics only |
| `0xCB` | none | waits for the mechanics to finish the current move |
| `0xCE` | in | 6 byte status block |
