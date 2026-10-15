# Progress

Lock: 2026-10-15T08:55:00+11:00 (main interactive session)

ALL PHASES DONE

Phase: 7 - complete (v0.1.0)

Done:
- Reverse engineering: firmware (SH-2) SCSI dispatcher, window layout, SEND types, vendor
  commands. docs/protocol.md
- cezanne-core, cezanne-app, cezanne-cli, cezanne-bridge. 37 tests pass. Linux and macOS
  type check clean.
- Windows NSIS installer built locally and smoke tested (install, demo run, uninstall).
  Copy in dist/ (git ignored).
- CI workflow pushed with a [package] commit. Result not checked (private repo, browser not
  signed in to GitHub).
- README and docs complete.

Open items for the user (see docs/STATUS.md):
- Check the first GitHub Actions run and download the macOS and Linux installers.
- First real hardware test: docs/hardware.md (cezanne-cli probe output).
- Find the "FT-S5000 Driver" file on the old Mac (DS Drivers folder) for the remaining unknowns.

Notes:
- LOCAL BUILD: no Visual Studio on this PC. In git bash: `. C:/Users/olros/tools/cz-env.sh` then cargo.
  Target x86_64-pc-windows-gnullvm, linker llvm-mingw clang, +crt-static (no extra DLLs).
- Z: is an SMB share. Keep CARGO_TARGET_DIR on local disk.
- Ghidra output and annotated firmware C: C:\Users\olros\tools\cz-ghidra-out
