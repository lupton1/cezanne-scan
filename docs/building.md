# Building

Cezanne Scan is a Rust workspace. Rust stable (1.85 or newer) is required.

```bash
cargo test --workspace
cargo build --release --workspace
```

The binaries are `cezanne-scan` (the app), `cezanne-cli` and `cezanne-bridge`.

## System packages

| System | Needs |
|---|---|
| Windows | Visual Studio Build Tools (MSVC), or the LLVM MinGW route below |
| macOS | Xcode command line tools |
| Debian or Ubuntu | `libxkbcommon-dev libgl1-mesa-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libwayland-dev` |

## Installers

Installers are made with [cargo-packager](https://github.com/crabnebula-dev/cargo-packager)
using [Packager.toml](../Packager.toml).

```bash
cargo install cargo-packager --locked --version 0.11.8
cargo build --release --workspace --target <triple>
cargo packager -c Packager.toml --release --target <triple> \
    --formats <formats> --out-dir target/<triple>/release
```

| System | Triple | Formats | Result |
|---|---|---|---|
| Windows | `x86_64-pc-windows-msvc` | `nsis` | `cezanne-scan_<v>_x64-setup.exe` (per user install) |
| macOS | `aarch64-apple-darwin` | `app,dmg` | `Cezanne Scan.app` and a `.dmg` |
| Linux | `x86_64-unknown-linux-gnu` | `deb,appimage` | `.deb` with the udev rule, and an `.AppImage` |

cargo-packager downloads NSIS and the AppImage tools by itself on first use.

## Continuous integration

[.github/workflows/build.yml](../.github/workflows/build.yml) runs the tests and a simulated
scan on every push to `main`. It builds the three installers when a `v*` tag is pushed,
when the workflow is started by hand, or when a commit message contains `[package]`.
Installers are kept as workflow artifacts. For a `v*` tag a release job then publishes a
GitHub release with the installers attached and the notes for that version taken from
[CHANGELOG.md](../CHANGELOG.md).

To make a release:

```bash
git tag -a v0.3.0 -m "Cezanne Scan 0.3.0"
git push origin v0.3.0
```

after updating the version in `Cargo.toml` and `Packager.toml` and adding a
`## 0.3.0 (date)` section to the changelog.

## Windows without Visual Studio

The development PC for this project has no Visual Studio, so it uses the all LLVM MinGW
target instead. No administrator rights are needed.

1. Install Rust with `rustup` using the host `x86_64-pc-windows-gnu`, then add the target:

   ```bash
   rustup target add x86_64-pc-windows-gnullvm
   ```

2. Download [llvm-mingw](https://github.com/mstorsjo/llvm-mingw/releases) (the
   `ucrt-x86_64` zip) and unpack it.
3. Put a copy of its `llvm-dlltool.exe`, renamed to `dlltool.exe`, together with the DLLs
   from its `bin` folder, in a folder on `PATH`. Do not put the whole llvm-mingw `bin`
   folder on `PATH`, or build scripts will try to link with it.
4. Set these variables (Git Bash syntax):

   ```bash
   export CARGO_BUILD_TARGET=x86_64-pc-windows-gnullvm
   export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER='C:\path\to\llvm-mingw\bin\x86_64-w64-mingw32-clang.exe'
   export CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS='-C target-feature=+crt-static'
   export CC_x86_64_pc_windows_gnullvm='C:\path\to\llvm-mingw\bin\x86_64-w64-mingw32-clang.exe'
   export AR_x86_64_pc_windows_gnullvm='C:\path\to\llvm-mingw\bin\llvm-ar.exe'
   ```

The static CRT setting means the programs need no extra DLLs.

Plain `x86_64-pc-windows-gnu` builds with the toolchain's own MinGW can link but the GUI
crashes at start up because of a mismatch in generated import libraries, so use the
target above.

## Demo mode

The app can run a scripted session against the simulator, which is how the screenshot in
the README was made and a quick way to check a build:

```bash
CEZANNE_DEMO=shot.png CEZANNE_DEMO_OUT=out cezanne-scan
```

It connects to the simulator, prescans, marks three frames, saves a screenshot, scans the
frames into `out` and quits.

## Icons

```bash
python scripts/make_icons.py
```

writes PNG, ICO and ICNS icons into `packaging/icons` with no extra libraries.
