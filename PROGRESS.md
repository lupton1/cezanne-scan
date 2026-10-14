# Progress

Lock: 2026-10-15T08:45:00+11:00 (main interactive session)

Phase: 6 - packaging, CI and docs
Done:
- Repo cloned, toolchains in C:\Users\olros\tools (rust gnu, jdk 21, vineflower, ghidra 12.1.4, llvm-mingw). 7-Zip is installed.
- Maintainer jar decompiled to re/java-src. It is only a high level API (SCSIToolkit native methods).
- lib5000SCSIDriver.jnilib decompiled. It is only a shim that loads the real "FT-S5000 Driver"
  (CFM bundle in a "DS Drivers" folder) by function name. That driver is NOT in the zip.
  The 2.0.8 updater is an Installer VISE archive (proprietary compression, no open extractor).
- FIRMWARE: FT-S5000_V2.12.jfirm = 512 byte header + LHA lh5 archive of jrom.com (7-Zip extracts it).
  jrom.com is Hitachi SH-2 big endian, base 0x09000000, vector table at offset 0.
  Inquiry identity: vendor "SCREEN  " product "FT-S5000", internal name MAMBO.
- SCSI dispatcher found (firmware 0x0901785C), full command set + window layout in docs/protocol.md
- crates/cezanne-core: transports (sg, spti, tcp bridge, simulator), protocol, scanner driver,
  image pipeline, ICC + TIFF writer, presets. 37 tests pass.
- cezanne-cli, cezanne-bridge, cezanne-app (egui) built and verified against the simulator.
  App demo mode: CEZANNE_DEMO=shot.png CEZANNE_DEMO_OUT=dir runs connect, prescan, 3 frames, scan, quit.

Next:
- icons, cargo-packager config, Windows NSIS installer built locally and smoke tested
- .github/workflows/build.yml (windows msvc, macos arm64, ubuntu)
- README + docs (hardware, building, architecture, STATUS, CHANGELOG)

Notes:
- LOCAL BUILD: no Visual Studio on this PC. In git bash: `. C:/Users/olros/tools/cz-env.sh` then cargo.
  Target x86_64-pc-windows-gnullvm, linker llvm-mingw clang, +crt-static (no extra DLLs).
  Output: C:\Users\olros\tools\cezanne-target\x86_64-pc-windows-gnullvm\
- Z: is an SMB share (\\192.168.0.110\Lupton). Keep CARGO_TARGET_DIR on local disk.
- Ghidra runner: C:\Users\olros\tools\run-ghidra.sh. Scripts in C:\Users\olros\tools\ghidra-scripts.
  Annotated firmware C: C:\Users\olros\tools\cz-ghidra-out\jrom.annot.c, handlers in scsi_handlers.c
