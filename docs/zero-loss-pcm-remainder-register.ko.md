# Zero-loss PCM remainder register

Stage 5는 zero-loss PCM-container rollout을 **감사된 remainder pass**로 마감합니다.

레지스터 상태:
- `supported` — 이미 배포된 저장소 상태에서 다뤄짐
- `close-now` — 근거가 있고, 정확하며, 범위가 제한되어 있고, 즉시 실행할 가치가 있음
- `defer` — 유효한 후속 작업이지만 Stage 5 `close-now` 항목은 아님
- `reject` — 정확한 FLAC/제품 경계 밖에 있거나 너무 모호함

## Current register

| Candidate | Source type | Evidence | Status | Rationale | Verification hook | Follow-up |
|---|---|---|---|---|---|---|
| Descriptor-backed raw PCM output symmetry | grounded current gap | `README.md:108`; `crates/flacx/README.md:364`; `crates/flacx/src/raw.rs:17-26` | `defer` | Raw PCM은 현재 제품에서 입력 전용으로 남아 있으며, output symmetry를 추가하려면 Stage 5가 다시 열어서는 안 되는 더 넓은 descriptor/sidecar/API output 계약이 필요합니다. 격차는 실제로 존재하지만, Stage 5 `close-now` 항목은 아닙니다. | 문서가 여전히 raw PCM을 ingest-only로 설명하고 있으며, 배포된 CLI/라이브러리 표면에 raw decode/output family가 존재하지 않음을 확인한다. | 정확한 raw-output 계약이 승인된 경우에만 별도 후속 작업으로 다시 연다. |
| AIFC `sowt` output symmetry | grounded current gap | `crates/flacx/src/aiff.rs:194-204`; `crates/flacx/src/aiff_output.rs:26-32,58-59,168-170`; `crates/flacx/README.md:348` | `defer` | 정확한 `sowt` 입력은 오늘 이미 존재하지만, Stage 4는 의도적으로 출력을 canonical AIFC `NONE`으로 표준화했습니다. `sowt` 출력을 배포하면 그 canonical-output 정책을 다시 열게 되며 별도 후속 작업으로 다루는 편이 낫습니다. | 배포된 출력이 여전히 canonical AIFC `NONE`이며 공개 문서가 `sowt` 출력을 약속하지 않음을 확인한다. | 제품이 선택 가능한 AIFC 출력 형태를 명시적으로 원할 때만 다시 연다. |

## Closeout

- 감사된 grounded known candidate 수: 2
- `close-now` 항목 수: 0
- 지금 추가할 만큼 충분히 근거가 있는 audit-only candidate 수: 0

**Stage 5는 두 행짜리 레지스터, 두 행 모두 명시적 후속 작업으로 deferred, `close-now` 항목 0개인 audited remainder pass로 완료되었습니다.**
