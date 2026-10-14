# Progress

Lock: 2026-10-15T08:22:56+11:00 (main interactive session)

Phase: 3 - Rust core done, CLI/bridge/app next
Done:
- Repo cloned, toolchains in C:\Users\olros\tools (rust gnu, jdk 21, vineflower, ghidra 12.1.4). 7-Zip is installed.
- Maintainer jar decompiled to re/java-src. It is only a high level API (SCSIToolkit native methods).
- lib5000SCSIDriver.jnilib decompiled. It is only a shim that loads the real "FT-S5000 Driver"
  (CFM bundle in a "DS Drivers" folder) by function name. That driver is NOT in the zip.
  The 2.0.8 updater is an Installer VISE archive (proprietary compression, no open extractor).
- FIRMWARE: FT-S5000_V2.12.jfirm = 512 byte header + LHA lh5 archive of jrom.com (7-Zip extracts it).
  jrom.com is Hitachi SH-2 big endian, base 0x09000000, vector table at offset 0.
  Inquiry identity: vendor "SCREEN  " product "FT-S5000", internal name MAMBO.
  It also has a serial service console (two letter commands, table near 0x090041ee).
- Ghidra output: C:\Users\olros\tools\cz-ghidra-out (jrom.bin.c, Server.c, ColorGeniusEX.c, jnilib).

- SCSI dispatcher found (firmware 0x0901785C), full command set + window layout in docs/protocol.md
- crates/cezanne-core: transports (sg, spti, tcp bridge, simulator), protocol, scanner driver,
  image pipeline (HD/SD densities, curves, levels, sharpen), ICC + TIFF writer, presets. 36 tests pass.
Next:
- crates/cezanne-cli (list, probe, scan) and crates/cezanne-bridge
- crates/cezanne-app (egui)
- packaging (cargo-packager), CI workflow, README/docs

Notes:
- Z: is an SMB share (\\192.168.0.110\Lupton). Keep CARGO_TARGET_DIR=C:\Users\olros\tools\cezanne-target
- Ghidra runner: C:\Users\olros\tools\run-ghidra.sh. Scripts in C:\Users\olros\tools\ghidra-scripts
