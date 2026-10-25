# Volatus

Volatus shows what uses the space on a Windows drive. It scans a drive or a folder and draws the result as a treemap with a folder tree and a file-type strip.

- **Fast.** On an NTFS drive, an administrator scan reads the file index (the MFT) directly. A scan of a 1.6 TB drive with 5 million files takes about 5 s warm on the test PC. Without administrator rights, Volatus walks the folders.
- **Clear.** Nested folder frames carry their names. File tiles are colored by file type.
- **Windows 11 look.** Mica backdrop, light and dark themes that follow Windows.

Volatus is written in Rust with the Slint UI toolkit. It is built and tested on Windows 11. Windows 10 is not tested.

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
