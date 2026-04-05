# flacx-cli

Command-line WAV/FLAC conversion powered by the `flacx` library crate.

`flacx-cli` is the CLI crate in this workspace. It is a thin adapter over the
same encode/decode pipeline exposed by the library crate and stays separate from the
publishable library package.

## Run locally

```bash
cargo run -p flacx-cli -- encode input.wav output.flac --level 8 --threads 4
cargo run -p flacx-cli -- decode input.flac output.wav --threads 4
```

## Command shape

- `flacx encode <input> <output>`
- `flacx decode <input> <output>`
- encode-only flags:
  - `--level`
  - `--threads`
  - `--block-size`
- decode-only flags:
  - `--threads`

## Progress display

- interactive terminals show a live progress line during encode and decode
- redirected or non-interactive runs do not emit progress UI
- progress comes from the library progress hooks, while the CLI owns rendering
- encode and decode use the same single-line, ASCII-compatible percent / ETA / Rate format
- ETA and Rate stay in a short warm-up state until two advancing updates and at least 250 ms of elapsed progress time have been observed

## Workspace relationship

- `crates/flacx` provides the reusable Rust API
- `crates/flacx-cli` provides the end-user CLI
- both use the same narrow encode/decode product surface and follow the same workspace version
