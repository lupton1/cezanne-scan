# FT-S5000 SCSI protocol

Derived from static analysis of three SCREEN components that agree with each other:

* the scanner firmware (FT-S5000 V2.12, Hitachi SH-2), which shows what the scanner accepts
* the host driver "FT-S5000 Driver" 2.1.3 (PowerPC, with debug function names), which
  shows exactly what ColorGeniusEX sends
* the FT-S5000 Maintainer tool (Java)

No SCREEN code is reproduced here, only the observed interface.

## Architecture

The scanner has two main processors.

* **JCONT** runs the SCSI target firmware (`jrom.com` inside `FT-S5000_V2.12.jfirm`).
  It is a Hitachi SH-2, big endian, ROM at `0x09000000`, RAM data at `0x090C0000`.
  The SCSI protocol chip is memory mapped at `0x0E000000`.
* **JOANA** drives the mechanics and optics (table, lens, CCD, iris, lamps). JCONT talks
  to it over an internal serial link (STX/ETX framing with ACK/NAK). The Maintainer's
  `SendJOANACommand` tunnels raw JOANA commands through SCSI.

On the Mac the host side lives in `/Library/Application Support/SCREEN/DS Drivers/FT-S5000
Driver`, a CFM bundle that ColorGeniusEX, its Server and the Maintainer all load by
function name. It talks SCSI through a `CarbonSCSI` helper bundle. Its configuration file
`FT-S5000 Config` sets a lamp warm up of 180 seconds and a maximum READ size of 512 KB.

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

### Parameter list header

| Bytes | Value |
|---|---|
| 0 and 1 | window data length: descriptors times 176 plus 6 |
| 2 to 5 | zero |
| 6 and 7 | descriptor length, 176 |

### Window descriptor (176 bytes)

Offsets are from the start of the descriptor. "Driver" means the value is what SCREEN's
driver writes (function `SetJobToDevice`), "firmware" means what the scanner checks or
reads. Every byte not listed is zero in the driver's windows.

| Offset | Size | Field | Driver writes | Notes |
|---|---|---|---|---|
| 0 | 1 | window ID | 1 to 99, cycling | firmware accepts 0 to 199 |
| 6 | 4 | upper left X | 0.1 mm | firmware limit: X plus extent at most 3420 |
| 10 | 4 | upper left Y | 0.1 mm | firmware limit: Y plus extent at most 5310 |
| 14, 18 | 4 each | SCSI-2 width and length | 0 | the scanner derives the extent from pixels and resolution |
| 23 | 1 | threshold | line art only | |
| 29 | 1 | flags | 0x80 for some separation types | |
| 41 to 43 | 3 | constants | 2, 100, 0x20 | always |
| 45 | 1 | scan mode | 1 prescan, 2 normal scan | 6, 7, 8 line art, 0x10 or more slow down modes |
| 46 | 2 | pixels per line | from the job | firmware maximum 12000 |
| 48 | 2 | lines | from the job | |
| 50 | 1 | image composition | 2 grey, 5 colour, 0 line art | |
| 51 | 1 | bits per pixel | 8, 16, 24, 48 | firmware picks 1, 2, 3 or 6 bytes per pixel |
| 54 | 4 | X resolution | dpi times 10 times magnification | firmware range 200 to 120000 (FT-S3000 66000) |
| 58 | 4 | Y resolution | same | |
| 62 to 65 | 4 | lookup table selectors | per original type, see below | |
| 66 to 71 | 6 | filter parameters | zero unless a median or defocus filter is on | |
| 132 | 1 | filter on | 0 or 1 | |
| 133 to 141 | | filter and sharpening parameters | from `FT-S5000 Config` | |
| 142 | 1 | original class | 2 reflective, 0 film | 1 and 4 for factory calibration types |
| 144 | 1 | light source | 1 reflective lamp, 0 transmission lamp | firmware switches lamps on this byte |
| 149 | 1 | focus mode | 0 prescan, 1 or 3 with auto focus | |
| 154 to 159 | | focus offsets | from the driver's focus calculation | |
| 160 | 1 | | 0 or 1 from a job option | |

The bed limits (3420 by 5310 in 0.1 mm) give a bed of 342 by 531 mm. The extent of a
window is pixels divided by (resolution field / 10) inches.

### Original types

The driver offers 12 original types. Bit 0x80 of the type code marks a reflective
original. The continuous tone types and their lookup table selector strings (a string
`d0 d1 d2 d3 d4` is packed into bytes 62 to 65 as `d2, d0, d4, d3 * 16 + d1`):

| Type | Light | Colour output | Grey output |
|---|---|---|---|
| Positive Film | transmission | `11003` | `11006` |
| Negative Film | transmission | `11005` | `11005` |
| Photograph | reflective | `22004` | `22004` |
| Printed Material | reflective | `22004` | `22004` |
| Watercolor | reflective | `22004` | `22004` |

The other types are three line art types and four colour separation types.

## SCAN (0x1B)

CDB byte 4 is the number of window IDs that follow (max 100). The data out phase is the
list of window IDs. Each must be defined by SET WINDOW, otherwise 5/26/02. The driver
always sends one window: RESERVE, SET WINDOW, SCAN, READ until done, RELEASE. While the
lamp warms up SCAN fails with NOT READY 04/80, and the driver reads the remaining warm up
time with vendor command 0xCA (function 3, sub function 0x100 transmission or 0x101
reflective).

## READ(10) (0x28)

Returns image data for the active scan. CDB bytes 6 to 8 are the transfer length. The
firmware keeps a count of bytes remaining (lines times bytes per line, computed from the
window) and the host should read exactly that many bytes in total. The driver reads up
to 512 KB per command.

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
| `0xC8` | in | Receive. Byte 2 type (`0x80` CCD or DSP data, `0x03` gamma, `0x84` matrix), bytes 4 and 5 qualifier, byte 3 bit 0 continues a chunked transfer |
| `0xC9` | out | IO Set: maintenance write (function code in the first data word). Not used |
| `0xCA` | in | IO Sense: maintenance read (function in CDB bytes 2 and 3, sub function in 4 and 5). Function 3 reports lamp warm up |
| `0xCB` | none | waits for the mechanics to finish the current move |
| `0xCE` | in | Test Scan Ready: 6 byte scan status block |
