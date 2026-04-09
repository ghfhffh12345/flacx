# Zero-loss PCM remainder register

Stage 5 closes the zero-loss PCM-container rollout as an **audited remainder pass**.

Register statuses:
- `supported` — already covered by the shipped repo state
- `close-now` — grounded, exact, bounded, and worth immediate execution
- `defer` — valid follow-up, but not a Stage 5 close-now item
- `reject` — outside the exact FLAC/product boundary or too ambiguous

## Current register

| Candidate | Source type | Evidence | Status | Rationale | Verification hook | Follow-up |
|---|---|---|---|---|---|---|
| Descriptor-backed raw PCM output symmetry | grounded current gap | `README.md:108`; `crates/flacx/README.md:364`; `crates/flacx/src/raw.rs:17-26` | `defer` | Raw PCM remains ingest-only in the current product, and adding output symmetry would require a broader descriptor/sidecar/API output contract than Stage 5 should reopen. The gap is real, but it is not a Stage 5 `close-now` item. | Confirm docs still describe raw PCM as ingest-only and that no raw decode/output family exists in the shipped CLI/library surface. | Reopen as a dedicated follow-up only if a precise raw-output contract is approved. |
| AIFC `sowt` output symmetry | grounded current gap | `crates/flacx/src/aiff.rs:194-204`; `crates/flacx/src/aiff_output.rs:26-32,58-59,168-170`; `crates/flacx/README.md:348` | `defer` | Exact `sowt` ingest exists today, but Stage 4 intentionally standardized output on canonical AIFC `NONE`. Shipping `sowt` output would reopen that canonical-output policy and is better handled as its own follow-up. | Confirm shipped output remains canonical AIFC `NONE` and that the public docs do not promise `sowt` output. | Reopen only if the product explicitly wants selectable AIFC output forms. |

## Closeout

- Grounded known candidates audited: 2
- `close-now` items: 0
- Additional audit-only candidates grounded enough to add now: 0

**Stage 5 is complete as an audited remainder pass with a two-row register, both rows deferred as explicit follow-ups, and zero `close-now` items.**
