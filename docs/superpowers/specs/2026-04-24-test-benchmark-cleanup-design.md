# Test And Benchmark Cleanup Design

**Date:** 2026-04-24

**Goal:** Remove brittle, low-value test code and improve the quality of the test and benchmark suites by preferring public-contract checks and clearer benchmark structure over source-text inspection.

## Constraints

- Public API behavior must remain unchanged.
- Coverage should protect observable contracts, not source formatting or file-local implementation details.
- README and doc wording should not be treated as a stable test contract unless there is no meaningful behavioral substitute.
- Benchmarks should preserve their measurement intent and fixture mix.
- Cleanup should avoid broad harness churn that adds new dependencies or new testing frameworks.

## Current State

The current suite mixes strong behavioral tests with several brittle source-inspection checks:

1. `crates/flacx/tests/api.rs` contains multiple `include_str!(...)` assertions against library source files.
2. `crates/flacx/tests/decode.rs` contains a benchmark-coupling test that inspects `crates/flacx/benches/throughput.rs` as raw text.
3. `crates/flacx/benches/throughput.rs` and `crates/flacx-cli/benches/directory_parity.rs` each contain local setup and matrix helpers that can be simplified or made more explicit.

The issue is not merely style. These tests are weak because they:

- fail on harmless refactors such as renames, formatting changes, or moved code
- do not prove that the public API is still usable
- couple tests to implementation layout across file boundaries
- encourage preserving wording and structure rather than preserving behavior

## Problems To Solve

### Brittle API Assertions

Several tests currently assert facts like:

- a source file still contains a specific `pub use`
- a type declaration appears in a specific module file
- a convenience implementation still contains a particular call sequence
- docs and README examples contain particular strings

These checks are not strong evidence of a stable public contract. A consumer cares whether the exported API compiles and behaves correctly, not whether a string exists in a source file.

### Benchmark-Coupling Test

`crates/flacx/tests/decode.rs` currently checks the throughput benchmark by substring-matching its source. That does not validate runtime behavior and creates unnecessary coupling between tests and benchmark implementation details.

### Benchmark Readability

The benchmark files are already functional, but some fixture/setup code can be clearer:

- shared thread/matrix constants are defined independently
- setup and per-iteration concerns are mixed closely together
- fixture preparation code can be more explicit about what is corpus-wide versus benchmark-specific

## Options Considered

### Option 1: Delete Only The Brittle Tests

- remove the `include_str!(...)` tests
- keep benchmarks structurally unchanged

Why this is insufficient:

- it reduces noise, but leaves gaps where some tests were trying to protect real API expectations
- it misses straightforward benchmark cleanup that improves maintainability without changing behavior

### Option 2: Replace Brittle Tests With Contract Checks And Tighten Benchmarks

- remove source-text assertions
- replace them with compile/runtime tests against exported types and functions where practical
- remove benchmark-source inspection tests
- simplify benchmark helper structure without changing benchmark intent

Why this is recommended:

- preserves meaningful coverage while removing low-signal checks
- improves test stability under internal refactors
- keeps the scope focused and avoids unnecessary new tooling

### Option 3: Introduce A Dedicated Compile-Fail Harness

- add `trybuild` or an equivalent API-surface harness
- convert many source checks into compile-pass/compile-fail fixture tests

Why this is not preferred:

- stronger in some cases, but too much harness expansion for this cleanup
- adds more moving parts than needed to address the current problems

## Chosen Design

Adopt Option 2.

The cleanup will replace source-text assertions with public-contract checks where there is real contract value, and it will delete tests that only enforce wording, file placement, or specific internal call shapes.

## Test Design

### Remove Source-Text Inspection

Remove tests whose primary mechanism is:

- `include_str!(...)`
- `.contains(...)` against source files
- counting source substrings to infer control flow or implementation shape

This includes the benchmark-source coupling test in `crates/flacx/tests/decode.rs` and the brittle API-source checks in `crates/flacx/tests/api.rs`.

### Replace With Public-Contract Tests

Where the removed test was guarding a real public expectation, replace it with a stronger check based on direct usage:

- instantiate exported public types directly
- call public constructors and builders
- run representative encode/decode/recompress flows through the public surface
- assert output equivalence or observable configuration behavior

Examples of the intended replacements:

- if a test wants to prove direct-construction stream types remain usable, construct those types in test code rather than searching for their struct definitions in module files
- if a test wants to prove top-level aliases remain usable, import and exercise those aliases directly
- if a test wants to prove convenience helpers use the intended reset API path, preserve only the externally observable behavior and remove internal call-shape policing

### Drop Wording Policing

README and library-doc wording will not be treated as a stable test contract for this cleanup.

That means:

- remove tests that assert documentation phrases verbatim
- keep behavioral examples covered through executable API usage rather than string matching

## Benchmark Design

### Throughput Benchmarks

In `crates/flacx/benches/throughput.rs`:

- keep the existing corpus and large-streaming benchmark coverage
- simplify shared helper plumbing where it removes duplication or clarifies intent
- prefer shared helpers/constants for decode thread matrices when both tests and benchmarks conceptually rely on the same comparison set

The benchmark should still measure the same encode/decode/recompress paths after cleanup.

### CLI Benchmarks

In `crates/flacx-cli/benches/directory_parity.rs`:

- keep the encode-directory, decode-directory, and large-streaming decode measurements
- improve fixture/setup clarity by separating persistent corpus preparation from per-iteration output directories
- remove incidental complexity that does not contribute to benchmark fidelity

## Non-Goals

- no public API redesign
- no new benchmark framework
- no broad rewrite of unrelated tests
- no attempt to freeze README or rustdoc wording through tests

## Validation Plan

1. Remove one brittle test or group of brittle tests.
2. Add the replacement contract test first where replacement coverage is warranted.
3. Run the targeted test and confirm the new check fails before the implementation change if a red/green cycle is applicable.
4. Apply the cleanup/refactor.
5. Re-run the targeted test files.
6. Run `cargo test --workspace --tests --benches --no-run`.

## Expected Outcome

After the cleanup:

- the test suite will be less sensitive to internal file edits and harmless refactors
- public-surface coverage will rely more on usage and behavior than on source text
- benchmark code will be easier to understand and maintain
- overall signal-to-noise in failures should improve because failing tests will map more directly to broken contracts
