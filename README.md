# Volatus

Volatus shows what uses the space on a Windows drive. It scans a drive or a folder and draws the result as a treemap with a folder tree and a file-type strip.

- **Fast.** On an NTFS drive, an administrator scan reads the file index (the MFT) directly. A scan of a 1.6 TB drive with 5 million files takes about 5 s warm on the test PC. Without administrator rights, Volatus walks the folders.
- **Clear.** Nested folder frames carry their names. File tiles are colored by file type.
- **Context menu.** Right-click a file or folder for the Windows menu, with Explore Folder, Command Prompt Here and Copy Path. Delete updates the map with no rescan.
- **Windows 11 look.** Mica backdrop, light and dark themes that follow Windows.
- **Your settings.** A gear opens a flyout to choose the theme, the size unit and which parts of the window show. See [docs/USAGE.md](docs/USAGE.md#settings).

Volatus is written in Rust with the Slint UI toolkit. It is built and tested on Windows 11. Windows 10 is not tested.

## Install

Download `volatus-X.Y.Z-x86_64.msi` from [Releases](https://github.com/MyndPhreak/Volatus/releases) and run it. It installs Volatus for all users in `C:\Program Files\Volatus` and asks for administrator rights one time. Volatus is then in the Start menu. It needs 64-bit Windows 10 or newer.

- The MSI is not signed. If Windows SmartScreen shows "Windows protected your PC", select **More info**, then **Run anyway**.
- The installer adds "Scan with Volatus" to the Explorer menu of folders, folder backgrounds and drives. You can clear this option during the installation. On Windows 11, the entry is under **Show more options**.
- To install with no window, run `msiexec /i volatus-X.Y.Z-x86_64.msi /qn` in a terminal that runs as administrator. Add `EXPLORERMENU=0` to install without the Explorer menu. An upgrade keeps the menu as it is: `EXPLORERMENU=0` has an effect only on the first installation.
- To uninstall, select Volatus in **Settings > Apps > Installed apps** (**Apps & features** on Windows 10), then **Uninstall**. This also removes the Explorer menu entries.

## Use

See [docs/USAGE.md](docs/USAGE.md) for the screens, the keys and the command line:

```
volatus [folder] [--timing] [--method auto|mft|walk] [--threads N]
```

## Build

```
cargo build --release -p app
```

The program is `target\release\volatus.exe`. Run `cargo test --workspace` for the tests. Two tests that read a real volume need an elevated terminal and are marked `#[ignore]`.

Benchmark results and the method are in [docs/BENCHMARK.md](docs/BENCHMARK.md).

## License

MIT. See [LICENSE](LICENSE).
