# Migration to v5

v5 changes the repository from a single Cargo package into a workspace with
separate library and CLI crates.

## What changed

- single package → Cargo workspace
- reusable library moved to `crates/flacx`
- CLI moved to `crates/flacx-cli`
- only the library crate is prepared for crates.io publication

## Old vs new layout

| Before v5 | After v5 |
| --- | --- |
| root `Cargo.toml` was the `flacx` package | root `Cargo.toml` is the workspace manifest |
| `src/lib.rs` and `src/main.rs` lived in one package | library and CLI live in separate crates |
| CLI dependencies were bundled into the library package | CLI-only dependencies stay in `flacx-cli` |

## Command migration

| Before v5 | After v5 |
| --- | --- |
| `cargo run -- encode ...` | `cargo run -p flacx-cli -- encode ...` |
| `cargo test` | `cargo test --workspace` |
| `cargo run --release --example benchmark` | `cargo run -p flacx --release --example benchmark` |

## Dependency migration

Library consumers should continue depending on the `flacx` crate. The crate
name stays the same; only the repository layout changes.

## What did not change

- codec behavior
- supported format envelope
- benchmark expectations on `test-wavs/`
- the shared encode engine used by library and CLI

## Common questions

### Where did the CLI go?

The CLI now lives in the `flacx-cli` workspace crate.

### Which crate should I depend on?

Depend on `flacx` for library use. The CLI crate is not intended as the
publishable crates.io package.

### Why make the repo a workspace?

To give the library a clean crates.io identity, separate CLI concerns from the
publishable crate, and make future maintenance/release work clearer.
