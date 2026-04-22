# flacx

`flacx` is the reusable Rust library in this workspace.
It provides encode, decode, and recompress workflows for supported PCM
containers and FLAC.

> `flacx` is still experimental, so APIs and format support may change.

## Quick start

Add the crate to your project:

```bash
cargo add flacx
```

The default feature set enables WAV, AIFF, and CAF support. If you want to
select features yourself, start with:

```toml
[dependencies]
flacx = { version = "0.10.0", default-features = false, features = ["wav", "progress"] }
```

For one-shot file workflows, use the built-in helpers:

```rust
use flacx::builtin::{decode_file, encode_file, recompress_file};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let encoded = encode_file("input.wav", "output.flac")?;
    let decoded = decode_file("output.flac", "roundtrip.wav")?;
    let recompressed = recompress_file("output.flac", "output.recompressed.flac")?;

    println!("encoded {} samples", encoded.total_samples);
    println!("decoded {} frames", decoded.frame_count);
    println!("recompressed {} samples", recompressed.total_samples);
    Ok(())
}
```

For finer control, use the explicit pipeline:
`PcmReader` for PCM-container inputs, `read_flac_reader` for FLAC inputs,
`EncoderConfig` / `DecodeConfig` / `RecompressConfig` for session policy, and
`inspect_pcm_total_samples` when you need a preflight sample count without a
full decode.

## Feature flags

- `wav` — WAV, RF64, and Wave64 support
- `aiff` — AIFF and AIFC support
- `caf` — CAF support
- `progress` — progress snapshots and callbacks

## Reference

For the full API, examples, and module documentation, see the rustdoc on
<https://docs.rs/flacx> or run `cargo doc -p flacx --open`.
