# flacx-cli

Command-line WAV-to-FLAC encoding powered by the `flacx` library crate.

`flacx-cli` is the CLI crate in this workspace. It is a thin adapter over the
same encode pipeline exposed by the library crate and stays separate from the
publishable library package.

## Run locally

```bash
cargo run -p flacx-cli -- encode input.wav output.flac --level 8 --threads 4
```

## Command shape

- `flacx encode <input> <output>`
- `--level`
- `--threads`
- `--block-size`

## Workspace relationship

- `crates/flacx` provides the reusable Rust API
- `crates/flacx-cli` provides the end-user CLI
- both use the same encode engine and follow the same workspace version
