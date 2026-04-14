# Encoding Performance Max Limits / Decode Recovery Report

## Session
- Date: 2026-04-15
- Worker lane: worker-2 verification + compare guardrails
- HEAD under test: `4a627fb`
- Compare revision: `ceb7cd7`
- Benchmark command: `cargo bench -p flacx --bench throughput -- --noplot`

## Same-machine benchmark replay

| Revision | Encode MiB/s | Decode MiB/s | Recompress MiB/s |
| --- | ---: | ---: | ---: |
| `HEAD` (`4a627fb`) | 22.393 | 38.341 | 15.282 |
| `ceb7cd7` | 13.823 | 36.840 | 11.986 |

## Gate status
- Encode target `>= 24.267 MiB/s`: **FAIL** (`HEAD` replay: `22.393 MiB/s`)
- Decode recovery vs fresh same-machine `ceb7cd7` replay: **PASS** (`38.341 MiB/s >= 36.840 MiB/s`)
- Historical decode expectation `~43.256 MiB/s`: **NOT MET** in this replay (`38.341 MiB/s`)

## Guardrails covered in tests
- `crates/flacx/tests/api.rs`
  - `explicit_reader_session_supports_variable_block_schedule_semantics`
  - `explicit_reader_session_progress_matches_default_output_for_variable_schedule`
  - `wav_reader_stream_appends_into_existing_output_buffer`
  - `raw_reader_stream_appends_into_existing_output_buffer`

These checks protect the reader-session/block-schedule semantics and the append-into-existing-buffer behavior that the allocation-reduction path depends on.

## Verification commands
- `cargo fmt --all -- --check` → PASS
- `cargo check -p flacx` → PASS
- `cargo test -p flacx --test api` → PASS
- `cargo test -p flacx --test encode --test recompress --test decode` → PASS
- `cargo test -p flacx --no-default-features --features progress,wav` → PASS
- `cargo test -p flacx --no-default-features --features progress,wav,aiff,caf` → PASS
- `cargo test -p flacx --doc` → PASS

## Notes
- The fresh same-machine comparison supports the explicit replay gate against `ceb7cd7`.
- The unchanged encode gate remains red, so this report is a verification artifact, not final plan sign-off.
- The replayed `ceb7cd7` decode result is materially lower than the historical `43.256 MiB/s` note, so any final sign-off should explain environment drift/noise if that higher figure remains unattainable.
