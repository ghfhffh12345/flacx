# flacx-cli

`flacx-cli` is the workspace command-line entry point for `flacx`.
It provides encode, decode, and recompress workflows from the shell.

> `flacx-cli` is still experimental, so CLI details may evolve.

## Quick start

Build the binary from a checkout:

```bash
cargo build -p flacx-cli --release
```

Then use the `flacx` binary:

```bash
flacx --help
```

## Examples

Encode a PCM container to FLAC:

```bash
flacx encode input.wav -o output.flac
```

Decode FLAC back to a PCM container:

```bash
flacx decode input.flac -o output.wav
```

Recompress a FLAC file in place:

```bash
flacx recompress input.flac --in-place
```

## Reference

Use the built-in help for the authoritative command reference:

```bash
flacx --help
flacx encode --help
flacx decode --help
flacx recompress --help
```
