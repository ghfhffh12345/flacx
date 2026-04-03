//! Workspace documentation for the `flacx-cli` crate.
//!
//! `flacx-cli` provides the command-line interface for WAV-to-FLAC encoding in
//! this workspace. It is kept separate from the publishable `flacx` library
//! crate while reusing the same encode pipeline and workspace version.
//!
//! # Command shape
//!
//! - `flacx encode <input> <output>`
//! - `--level`
//! - `--threads`
//! - `--block-size`
