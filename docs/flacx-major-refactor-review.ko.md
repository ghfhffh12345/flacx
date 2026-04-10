# flacx major refactor review and documentation guide

이 메인터이너 문서는 `.omx/plans/prd-flacx-major-refactor.md`를 explicit-core / convenience-layer 리팩터를 위한 코드 품질 검토 체크리스트와 문서 계약으로 번역합니다.

이 문서는 의도적으로 현재 저장소 상태에 근거를 두고 있어, 구현, 테스트, 문서 작업이 동일한 어휘로 수렴할 수 있게 합니다.

## Goals

- 저수준 encode/decode 흐름을 명시적이고 format-generic하게 만들기
- 파일 경로 및 바이트 버퍼 ergonomics를 일급 convenience layer에 유지하기
- 지원되는 format 패밀리를 거친 Cargo feature로 게이트하기
- 문서, 테스트, 검증 레인을 동일한 아키텍처 스토리에 정렬하기

## Grounded review findings

### 1. The public surface is still described from the convenience edge inward

저장소 근거:
- `crates/flacx/src/lib.rs`는 `encode_file`, `encode_bytes`, `decode_file`, `decode_bytes`를 작은 공개 API 스토리로 제시합니다.
- `crates/flacx/README.md`는 어떤 명시적 reader/writer 또는 adapter 경계보다 먼저 파일 helper와 바이트 helper를 중심에 둡니다.
- `crates/flacx/src/encoder.rs`와 `crates/flacx/src/decode.rs`는 각각 generic stream 작업, 파일 helper, in-memory helper를 같은 façade 안에 혼합합니다.

품질 함의:
- 호출자는 여전히 이 크레이트를 explicit core plus orchestration layer가 아니라 경로 지향 convenience API로 이해할 수 있습니다.
- 문서에서 아키텍처 경계가 아직 분명하지 않기 때문에 검토 압력이 최상위 façade에 계속 집중됩니다.

문서 요구사항:
- core layer를 먼저 설명합니다: explicit config, typed PCM handoff, format-specific reader/writer, core encoder/decoder orchestration.
- convenience helper는 두 번째로, config를 유도하고 정책을 중복하지 않고 core로 라우팅하는 얇은 wrapper로 설명합니다.

### 2. Feature-gated format families are not yet visible in the crate contract

저장소 근거:
- `crates/flacx/Cargo.toml`은 현재 `progress`만 노출합니다.
- 코드베이스는 이미 `aiff.rs`, `caf.rs`, `aiff_output.rs`, `caf_output.rs`, `wav_output.rs` 같은 family-specific 모듈을 담고 있지만, 공개 문서는 거친 feature-family 계약을 설명하지 않습니다.

품질 함의:
- 지원 형식의 성장을 컴파일 타임 경계 대신 하드코딩된 분기 하나 더 추가하는 것으로 설명하기 쉬워집니다.
- 가리킬 단일 feature 매트릭스가 없기 때문에 문서와 테스트가 드리프트할 수 있습니다.

문서 요구사항:
- 작고 읽기 쉬운 feature 표면을 문서화합니다:
  - `wav` — RIFF/WAVE, RF64, Wave64
  - `aiff` — AIFF, AIFC allowlist
  - `caf` — CAF allowlist
  - `progress` — callback-style progress reporting
- family gate에 의존하는 모든 README/example/test 명령은 이를 명시적으로 밝혀야 합니다.

### 3. Migration hotspots are concentrated in large container and metadata files

`wc -l crates/flacx/src/*.rs crates/flacx/tests/*.rs`에서 얻은 저장소 근거:
- `crates/flacx/src/metadata.rs` — 1763줄
- `crates/flacx/src/input.rs` — 1259줄
- `crates/flacx/src/read.rs` — 1156줄
- `crates/flacx/src/write.rs` — 884줄
- 컨테이너 출력 모듈도 여전히 큽니다 (`wav_output.rs`, `aiff_output.rs`, `caf_output.rs`)

품질 함의:
- 이 파일들은 리팩터에서 가장 위험한 병합 및 검토 구역입니다.
- 명시적 문서가 없으면 리뷰어는 어떤 변경이 core, container adapter, convenience layer 중 어디에 속하는지 놓칠 수 있습니다.

문서 요구사항:
- 더 많은 코드 이동이 일어나기 전에 각 layer의 의도된 소유권을 이름 붙이는 짧은 architecture map을 유지합니다.
- 향후 문서 업데이트는 단순한 사용자 예시보다 모듈 소유권과 정책 경계에 더 편향되게 유지합니다.

## Target architecture map

### Explicit core

explicit core는 다음의 단일 진실 원천이어야 합니다:
- encode/decode/recompress configuration type
- container adapter와 FLAC logic 사이의 typed PCM stream 또는 frame handoff
- explicit input/output에서 동작하는 core encoder/decoder orchestration
- codec policy, validation, summary reporting

explicit core가 **소유하면 안 되는 것**:
- file-extension inference
- path-based convenience routing
- one-off helper를 위한 ad-hoc policy fork

### Container adapters

container reader와 writer는 다음을 소유해야 합니다:
- header parsing 및 emission
- 해당 family에 대한 family-specific metadata translation
- 해당 family에 대한 exactness / allowlist validation
- typed PCM abstraction으로의 변환과 그 반대 변환

### Convenience layer

convenience layer는 다음을 소유해야 합니다:
- file-to-file helper
- byte-buffer helper
- 명시적으로 요청되었을 때의 extension-based inference
- 사용자 친화적 입력에서 core config를 안전하게 유도하는 일

규칙: convenience helper는 orchestration할 수 있지만, core나 container adapter에 이미 있는 codec policy를 재구현해서는 안 됩니다.

## Documentation contract for the refactor

리팩터가 착지하면 문서는 이 순서로 동일한 스토리를 말해야 합니다:

1. **Architecture and feature gates**
   - explicit core
   - convenience layer
   - supported feature families
2. **Core-first examples**
   - explicit encoder/decoder construction
   - 관련된 경우 explicit container selection
3. **Convenience examples**
   - file helpers
   - byte helpers
   - opt-in behavior로서의 extension inference
4. **Verification story**
   - targeted tests
   - feature-matrix smoke coverage
   - benchmark lane

권장 문서 표면:
- `crates/flacx/src/lib.rs`의 crate-level rustdoc
- 공개 라이브러리 사용을 위한 `crates/flacx/README.md`
- 높은 수준의 feature story와 doc map을 위한 workspace `README.md`
- 아키텍처와 review watchpoint를 위한 maintainer-only docs

## Migration notes for maintainers

최종 모듈 이름을 너무 일찍 고정하지 않고도 refactor PR을 검토할 수 있도록 이 메모를 사용하세요.

### Public-story migration

- **Current story:** `Encoder` / `Decoder`와 `encode_file`, `encode_bytes`, `decode_file`, `decode_bytes`가 기본 API처럼 읽힙니다.
- **Target story:** explicit encoder/decoder configuration과 typed container adapter가 주된 아키텍처가 되고, path와 byte helper는 convenience orchestration으로 제시됩니다.

### Ownership migration

- container-specific parsing/writing 기대치를 최상위 façade 문서에서 reader/writer ownership note로 이동합니다
- config derivation과 extension inference는 core codec 동작이 아니라 convenience-layer 동작으로 문서화합니다
- feature-family 문서는 거칠고 안정적이며 Cargo manifest, rustdoc, README example 전반에 공유되도록 유지합니다

### Review note

전환 동안 문서는 현재 façade 진입점과 목표 layered architecture를 모두 일시적으로 언급할 수 있습니다. 문서가 방향을 명확히 하고 convenience wrapper를 유일한 개념 모델로 설명하지 않는 한 이는 허용됩니다.

## Verification lanes that docs should name explicitly

리팩터는 다음 레인에 맞추어 문서를 정렬된 상태로 유지해야 합니다:

- targeted regression tests:
  - `cargo test -p flacx --test api --test decode`
- feature matrix smoke:
  - `cargo test -p flacx --no-default-features --features progress,wav`
  - `cargo test -p flacx --no-default-features --features progress,wav,aiff,caf`
- crate diagnostics:
  - `cargo check -p flacx`
- throughput baseline:
  - `cargo bench -p flacx --bench throughput`

doc/example가 feature family에 의존한다면, verification section은 같은 gate를 실행하는 명령을 보여줘야 합니다.

## Review checklist for implementation PRs

- 문서 순서가 convenience wrapper보다 explicit core를 먼저 제시하는가?
- feature family 이름이 Cargo feature, README 텍스트, 테스트 명령 전반에서 일관적인가?
- reader/writer 모듈이 최상위 convenience façade가 아니라 container-specific policy를 소유하는가?
- convenience helper가 로컬에서 정책 분기를 만들지 않고 단일 진실 원천에 위임하는가?
- verification 명령이 feature-on과 feature-off 동작을 모두 다루는가?

## Suggested follow-up doc edits after code lands

1. `crates/flacx/src/lib.rs` rustdoc를 explicit-core story로 시작하도록 업데이트한다
2. `crates/flacx/README.md`의 feature 예시와 supported-format matrix를 새로 고친다
3. workspace `README.md`의 dependency 예시와 documentation map을 새로 고친다
4. 아키텍처 메모 근처에 benchmark와 feature-smoke 명령을 유지하여 review 동안 성능 요구사항이 계속 보이게 한다
