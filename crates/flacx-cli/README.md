# flacx-cli

Command-line WAV/FLAC conversion powered by the `flacx` library crate.

`flacx-cli` is the CLI crate in this workspace. It is a thin adapter over the
same encode/decode pipeline exposed by the library crate and stays separate from the
publishable library package.

## Run locally

```bash
cargo run -p flacx-cli -- encode input.wav -o output.flac --level 8 --threads 4
cargo run -p flacx-cli -- encode album-dir -o encoded-album --depth 0
cargo run -p flacx-cli -- decode input.flac -o output.wav --threads 4
cargo run -p flacx-cli -- decode encoded-album -o decoded-album --depth 0
```

## Command shape

- `flacx encode <input> [-o <output-or-dir>] [--depth <depth>]`
- `flacx decode <input> [-o <output-or-dir>] [--depth <depth>]`
- encode-only flags:
  - `--output` / `-o`
  - `--level`
  - `--threads`
  - `--block-size`
  - `--depth`
- decode-only flags:
  - `--output` / `-o`
  - `--threads`
  - `--depth`

Encode output behavior:

- single-file input with no `-o` writes a sibling `.flac` next to the source WAV
- single-file input with `-o <path>` writes exactly to that file path
- folder input with no `-o` writes `.flac` siblings next to each discovered WAV
- folder input with `-o <dir>` writes under the destination root while preserving relative subpaths
- `--depth` defaults to `1`, affects only folder input, and uses `0` for unlimited traversal
- encode `--threads` defaults to `8`

Decode output behavior:

- single-file input with no `-o` writes a sibling `.wav` next to the source FLAC
- single-file input with `-o <path>` writes exactly to that file path
- folder input with no `-o` writes `.wav` siblings next to each discovered FLAC
- folder input with `-o <dir>` writes under the destination root while preserving relative subpaths
- `--depth` defaults to `1`, affects only folder input, and uses `0` for unlimited traversal

## Progress display

- interactive terminals show a live progress line during encode and decode
- redirected or non-interactive runs do not emit progress UI
- progress comes from the library progress hooks, while the CLI owns rendering
- single-file runs show filename, percent, elapsed time, ETA, and rate
- folder runs show overall batch progress plus per-file progress on the same live line
- batch progress totals use exact samples processed across the full planned worklist
- ETA and Rate stay in a short warm-up state until two advancing updates and at least 250 ms of elapsed progress time have been observed

## Workspace relationship

- `crates/flacx` provides the reusable Rust API
- `crates/flacx-cli` provides the end-user CLI
- both use the same narrow encode/decode product surface and follow the same workspace version
