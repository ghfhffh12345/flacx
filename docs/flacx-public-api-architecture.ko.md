# flacx public API architecture guide

이 가이드는 **외부에 노출되는 API의 관점에서 본 현재 `flacx` 아키텍처**를 설명합니다. 구현으로 바로 뛰어들지 않고도 크레이트를 한눈에 이해해야 하는 메인터이너와 기여자를 대상으로 합니다.

이 문서는 다음과 서로 보완됩니다:
- `crates/flacx/src/lib.rs` — 크레이트 계약과 rustdoc을 위한 공개 맵
- `crates/flacx/README.ko.md` — 크레이트 수준 아키텍처 요약
- `docs/flacx-major-refactor-review.ko.md` — 리팩터 검토 체크리스트와 메인터이너 migration 메모

## 1. Architecture summary

`flacx`는 작은 공개 스토리를 중심으로 구성됩니다:

```text
explicit configs + typed boundaries
                │
                ▼
      Encoder / Decoder / Recompressor
                │
                ▼
      container readers / writers
                │
                ▼
           FLAC read/write core
                │
                ▼
   convenience helpers route into the same core
```

핵심 아키텍처 구분은 다음과 같습니다:
- **explicit core** = 크레이트의 의미적 중심
- **convenience/orchestration** = 그 core로 라우팅하는 wrapper

문서는 이 읽기 순서를 유지해야 합니다.

## 2. Public interface map

```text
flacx
├─ modules
│  ├─ core
│  ├─ convenience
│  └─ level
├─ config/builders
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  └─ RecompressConfig / RecompressBuilder
├─ codec façades
│  ├─ Encoder / EncodeSummary
│  ├─ Decoder / DecodeSummary
│  └─ Recompressor / RecompressMode / RecompressPhase / RecompressProgress
├─ typed boundary
│  ├─ PcmStream / PcmStreamSpec / PcmContainer
│  ├─ read_pcm_stream / write_pcm_stream
│  └─ RawPcmDescriptor / RawPcmByteOrder
├─ inspectors
│  ├─ inspect_wav_total_samples
│  ├─ inspect_pcm_total_samples
│  ├─ inspect_flac_total_samples
│  └─ inspect_raw_pcm_total_samples
└─ optional progress
   ├─ ProgressSnapshot
   ├─ EncodeProgress / DecodeProgress
   └─ progress-enabled methods on the façade types
```

## 3. Layer ownership map

| Layer | Public entry points | What it owns | What it should not become |
| --- | --- | --- | --- |
| Explicit core | `core`, configs/builders, `Encoder`, `Decoder`, `Recompressor`, typed PCM helpers | 구성, typed handoff, 명시적 encode/decode/recompress 동작, 요약 | 경로 중심 convenience 스토리 |
| Convenience/orchestration | `convenience`, flat `*_file` / `*_bytes` helpers | one-shot 경로/바이트 라우팅과 확장자 지향 ergonomics | 중복 정책 엔진 |
| Container adaptation | 공개 typed boundary와 그 뒤의 family-specific 동작 | 컨테이너 parsing/writing과 family-specific translation | 최상위 아키텍처를 먼저 설명하는 장소 |
| Support surfaces | `level`, inspector helpers, raw PCM helpers, progress types | core에 인접한 보조 개념 | 주요 개념적 중심 |

## 4. Current source tree snapshot

현재 공개 API 스토리를 지탱하는 구조 스냅샷은 다음과 같습니다:

```text
crates/flacx/src/
├─ lib.rs
├─ config.rs
├─ convenience.rs
├─ encoder.rs
├─ decode.rs
├─ recompress.rs
├─ pcm.rs
├─ input.rs
├─ wav_input.rs
├─ wav_output.rs
├─ decode_output.rs
├─ encode_pipeline.rs
├─ metadata.rs
├─ metadata/
│  ├─ blocks.rs
│  └─ draft.rs
├─ read/
│  ├─ mod.rs
│  ├─ frame.rs
│  └─ metadata.rs
├─ write/
│  ├─ mod.rs
│  └─ frame.rs
├─ raw.rs
├─ level.rs
├─ progress.rs
└─ ... supporting modules omitted here
```

### Reading the tree
- `lib.rs`는 공개 계약 표면입니다.
- `config.rs`, `encoder.rs`, `decode.rs`, `recompress.rs`, `pcm.rs`는 노출된 아키텍처를 가장 빠르게 파악하는 출발점입니다.
- `input.rs`, `wav_input.rs`, `wav_output.rs`, `read/`, `write/`는 리팩터 동안 컨테이너 측 경계와 FLAC 측 경계가 어떻게 분리되었는지를 보여줍니다.
- `metadata/`와 `decode_output.rs`는 주요 책임을 최상위 façade 바깥에 유지하기 위해 존재합니다.

## 5. Interface-to-structure map

```text
Public surface                      Main structural anchors
──────────────────────────────────  ───────────────────────────────────────────
crate contract                      lib.rs
config/builders                     config.rs
explicit encode façade              encoder.rs + encode_pipeline.rs
explicit decode façade              decode.rs + decode_output.rs
recompress façade                   recompress.rs
typed PCM boundary                  pcm.rs + input.rs
WAV-family ingest/output            wav_input.rs + wav_output.rs
FLAC read/write internals           read/ + write/
metadata model / translation        metadata.rs + metadata/
optional progress                   progress.rs
```

이 맵은 의도적으로 얕습니다. 목적은 방향성을 제공하는 것이지 전체 호출 그래프나 내부 실행 추적을 설명하는 것이 아닙니다.

## 6. Feature-gated contract

| Feature | Architectural effect |
| --- | --- |
| `wav` | RIFF/WAVE, RF64, Wave64 패밀리 표면을 활성화합니다. |
| `aiff` | AIFF와 제한된 AIFC 표면을 활성화합니다. |
| `caf` | 제한된 CAF 표면을 활성화합니다. |
| `progress` | 선택적 콜백 지향 진행 상황 표면을 활성화합니다. |

## 7. Narrative priorities for future documentation edits

공개 문서를 편집할 때는 다음 순서를 유지하세요:
1. 공개 아키텍처와 layering
2. 공개 인터페이스 grouping
3. feature-gated 계약
4. subordinate support surfaces
5. 아키텍처 스토리를 밀어내지 않는 범위에서만 practical usage

특히:
- convenience helper가 주요 개념 모델이 되도록 두지 마세요
- 실제 필요가 방향성 파악일 때 튜토리얼 산문으로 시작하지 마세요
- 공개 문서에서 내부 실행 경로를 과도하게 설명하지 마세요

## 8. Docs synchronization checklist

공개 표면이 바뀌면 다음을 함께 확인하세요:
- `crates/flacx/src/lib.rs`
- `crates/flacx/README.md`
- `docs/flacx-public-api-architecture.md`
- 해당 파일들을 가리키는 워크스페이스 수준 문서 맵

## 9. Verification cues

이 문서들을 업데이트할 때 유용한 확인 명령:

```bash
cargo check -p flacx
cargo test -p flacx --test api --test decode
cargo test --workspace
find crates/flacx/src -maxdepth 2 -type f | sort
rg -n "core|convenience|architecture|flacx-public-api-architecture" \
  crates/flacx/src/lib.rs crates/flacx/README.md README.md docs/flacx-public-api-architecture.md
```

이 확인은 산문의 품질 자체를 증명하지는 않지만, 구조적 노후화와 이름 드리프트를 잡아내는 데 도움을 줍니다.
