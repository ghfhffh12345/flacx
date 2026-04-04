# flacx-cli

Command-line WAV/FLAC conversion powered by the `flacx` library crate.

`flacx-cli` is the CLI crate in this workspace. It is a thin adapter over the
same encode/decode pipeline exposed by the library crate and stays separate from the
publishable library package.

## Run locally

```bash
cargo run -p flacx-cli -- encode input.wav output.flac --level 8 --threads 4
cargo run -p flacx-cli -- decode input.flac output.wav
```

## Command shape

- `flacx encode <input> <output>`
- `flacx decode <input> <output>`
- encode-only flags:
  - `--level`
  - `--threads`
  - `--block-size`

## Progress display

- interactive terminals show a live progress bar during encode
- redirected or non-interactive runs do not emit progress UI
- decode runs stay quiet unless they fail
- progress comes from the library's encoder-backed progress reporting on the
  existing fast encode path
- this crate explicitly enables the `flacx` `progress` feature
- the progress line stays single-line and ASCII-compatible, with percent plus
  `ETA` and `Rate`
- `ETA` and `Rate` stay in a short warm-up state until the renderer has seen
  two advancing updates and at least 250 ms of elapsed encode time

## Workspace relationship

- `crates/flacx` provides the reusable Rust API
- `crates/flacx-cli` provides the end-user CLI
- both use the same narrow encode/decode product surface and follow the same workspace version
