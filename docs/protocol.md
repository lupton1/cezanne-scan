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
