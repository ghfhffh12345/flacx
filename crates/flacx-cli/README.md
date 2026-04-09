# flacx-cli

`flacx-cli` is the workspace command-line interface for WAV/FLAC conversion and
FLAC recompression.
It uses the same encode/decode pipeline as the `flacx` library crate and is
kept separate from the publishable library package.

> Warning: flacx-cli is still experimental. The current `fxmd` layout is canonical `v1`; mode presets only adjust capture/emission and validation around that format.

## Run it locally

Build the release binary from the workspace root:

```bash
cargo build --release
```

Then run `flacx` directly from `target/release/` (or after adding that directory to your `PATH`):

```bash
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
flacx decode input.flac -o output.wav --threads 4
flacx decode encoded-album -o decoded-album --depth 0
flacx recompress input.flac -o input.recompressed.flac --level 0 --threads 4
flacx recompress input.flac --in-place --level 0 --threads 4
flacx recompress album-dir -o recompressed-album --depth 0
```

## Command model

The CLI exposes three top-level commands:

- `flacx encode <input> [-o <output-or-dir>] [--level <0-8>] [--threads <n>] [--block-size <samples>] [--mode <loose|default|strict>] [--depth <n>]`
- `flacx decode <input> [-o <output-or-dir>] [--threads <n>] [--mode <loose|default|strict>] [--depth <n>]`
- `flacx recompress <input> [-o <output-or-dir>] [--in-place] [--level <0-8>] [--threads <n>] [--block-size <samples>] [--mode <loose|default|strict>] [--depth <n>]`

The input can be either a single file or a directory tree.
Directory traversal is controlled by `--depth`.

## Encode

### Flags

- `-o, --output <path>`
- `--level <0-8>`
- `--threads <n>`
- `--block-size <samples>`
- `--mode <loose|default|strict>`
- `--depth <n>`

### Defaults and behavior

- `--level` defaults to `8`.
- `--threads` defaults to `8`.
- `--block-size` is optional; when omitted, the block size comes from the selected compression level.
- `--mode` defaults to `default`.
- `--depth` defaults to `1`.
- `--depth` only affects directory input.
- Use `--depth 0` for unlimited recursive traversal.
- Single-file input with no `-o` writes a sibling `.flac` next to the source WAV.
- Single-file input with `-o <path>` writes to that exact file path.
- Directory input with no `-o` writes `.flac` siblings next to each discovered WAV.
- Directory input with `-o <dir>` preserves relative subpaths under the destination directory.
- For single-file input, `-o` must be a file path.
- For directory input, `-o` must be a directory path.

### Examples

```bash
flacx encode input.wav
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
```

## Decode

### Flags

- `-o, --output <path>`
- `--threads <n>`
- `--mode <loose|default|strict>`
- `--depth <n>`

### Defaults and behavior

- `--threads` is optional.
- When omitted, the decode path uses the library default thread count.
- `--mode` defaults to `default`.
- `--depth` defaults to `1`.
- `--depth` only affects directory input.
- Use `--depth 0` for unlimited recursive traversal.
- Single-file input with no `-o` writes a sibling `.wav` next to the source FLAC.
- Single-file input with `-o <path>` writes to that exact file path.
- Directory input with no `-o` writes `.wav` siblings next to each discovered FLAC.
- Directory input with `-o <dir>` preserves relative subpaths under the destination directory.
- `--mode loose` disables `fxmd` capture/emission and disables relaxable validation.
- `--mode default` preserves the canonical `fxmd v1` behavior and rejects malformed or duplicate `fxmd` payloads.
- `--mode strict` preserves the canonical `fxmd v1` behavior, enables the relaxable validation set, and rejects malformed or duplicate `fxmd` payloads.
- For single-file input, `-o` must be a file path.
- For directory input, `-o` must be a directory path.

### Examples

```bash
flacx decode input.flac
flacx decode input.flac -o output.wav --threads 4
flacx decode encoded-album -o decoded-album --depth 0
flacx decode input.flac --mode loose
flacx decode input.flac --mode strict
```

## Recompress

### Flags

- `-o, --output <path>`
- `--in-place`
- `--level <0-8>`
- `--threads <n>`
- `--block-size <samples>`
- `--mode <loose|default|strict>`
- `--depth <n>`

### Defaults and behavior

- `--level` defaults to `8`.
- `--threads` defaults to `8`.
- `--block-size` is optional; when omitted, the block size comes from the selected compression level.
- `--mode` defaults to `default`.
- `--depth` defaults to `1`.
- `--depth` only affects directory input.
- Use `--depth 0` for unlimited recursive traversal.
- Single-file input with no `-o` writes a sibling `.recompressed.flac` next to the source FLAC.
- Single-file input with `-o <path>` writes to that exact file path.
- `--in-place` explicitly opts into replacing the source FLAC after successful recompression.
- `--in-place` is incompatible with `-o`.
- Without `--in-place`, same-path output is rejected.
- Directory input with no `-o` writes `.recompressed.flac` siblings next to each discovered FLAC.
- Directory input with `-o <dir>` preserves relative subpaths under the destination directory and keeps the original filename because the output root differs.
- Directory input with `--in-place` rewrites discovered source files in place after each successful temp-file commit.
- Directory in-place overwrite is atomic per file, not an all-or-nothing transaction across the whole batch.
- `--mode` keeps metadata handling and validation aligned with the existing loose/default/strict policy model.
- For single-file input, `-o` must be a file path.
- For directory input, `-o` must be a directory path.

### Examples

```bash
flacx recompress input.flac
flacx recompress input.flac -o input.recompressed.flac --level 0 --threads 4
flacx recompress input.flac --in-place --level 0 --threads 4
flacx recompress album-dir -o recompressed-album --depth 0
flacx recompress album-dir --in-place --depth 0
```

## Output layout summary

| Input shape | `-o` omitted | `-o <file>` | `-o <dir>` |
| --- | --- | --- | --- |
| Single file | sibling output next to the source file | exact file path | rejected |
| Directory | sibling outputs next to each discovered file | rejected | preserve relative subpaths under the destination root |

## Progress display

The CLI renders progress only when standard error is attached to an interactive
terminal.

- interactive terminals show live encode/decode/recompress progress lines
- redirected or non-interactive runs suppress the progress UI
- progress data comes from the library progress hooks; the CLI only renders it
- single-file runs show the current filename, percent, elapsed time, ETA, and rate
- directory runs show overall batch progress and per-file progress on separate live lines
- recompress progress is phase-aware and reports both decode and encode work
- batch progress totals use exact sample counts across the full planned worklist
- ETA and rate remain in a short warm-up state until two advancing updates and at least 250 ms of elapsed progress time have been observed

## Relationship to the library crate

- `crates/flacx` provides the reusable Rust API.
- `crates/flacx-cli` provides the end-user CLI.
- both crates share the same workspace version and the same encode/decode pipeline
- the CLI is a thin adapter over the library, not a separate publishing target

For the library API guide, see `crates/flacx/README.md`.
For workspace-level context, see the repository root `README.md`.
