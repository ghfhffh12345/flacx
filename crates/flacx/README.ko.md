# flacx

Rust를 위한 고성능 PCM 컨테이너/FLAC 변환 및 FLAC 재압축.

`flacx`는 이 워크스페이스에서 게시 가능한 라이브러리 크레이트입니다. 이 README는 **현재 공개 API 표면**을 빠르게 다시 파악해야 하는 메인터이너와 기여자를 위한 크레이트 수준 아키텍처 가이드입니다.

> Warning: this crate is still experimental. The current `fxmd` layout is the canonical `v1` format, and historical `fxmd` payload variants are not supported.

## Documentation intent

이 문서는 의도적으로 초보자용 튜토리얼이나 편의성 우선 워크스루가 아닙니다. 이것은 `crates/flacx/src/lib.rs`에 있는 크레이트 rustdoc의 공개 아키텍처 동반 문서입니다.

다음과 같은 질문에 답해야 할 때 이 문서를 사용하세요:
- `flacx`는 어떤 개념적 표면을 노출하는가?
- explicit core와 convenience layer는 어디에서 나뉘는가?
- 현재 어떤 소스 파일이 그 표면을 담당하는가?
- 어떤 feature gate가 공개 계약을 형성하는가?

더 큰 구조적 관점은 [`docs/flacx-public-api-architecture.ko.md`](../../docs/flacx-public-api-architecture.ko.md)를 참고하세요.

## Package surface

```toml
[dependencies]
flacx = "0.8.2"
```

기본 feature 패밀리:
- `wav` => RIFF/WAVE, RF64, Wave64
- `aiff` => AIFF, AIFC
- `caf` => CAF

선택적 feature:
- `progress` => 콜백 지향 진행 상황 보고

## Public API interface map

```text
flacx
├─ core
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  ├─ RecompressConfig / RecompressBuilder
│  ├─ Encoder / EncodeSummary
│  ├─ Decoder / DecodeSummary
│  ├─ Recompressor / RecompressMode / RecompressPhase / RecompressProgress
│  ├─ PcmStream / PcmStreamSpec / PcmContainer
│  ├─ read_pcm_stream / write_pcm_stream
│  └─ RawPcmDescriptor / RawPcmByteOrder / inspect_raw_pcm_total_samples
├─ inspectors
│  ├─ inspect_pcm_total_samples
│  ├─ inspect_wav_total_samples
│  ├─ inspect_flac_total_samples
│  └─ inspect_raw_pcm_total_samples
├─ convenience
│  ├─ encode_file / encode_bytes
│  ├─ decode_file / decode_bytes
│  ├─ recompress_file / recompress_bytes
│  └─ inspection-helper re-exports
├─ level
└─ progress (feature = "progress")
   ├─ ProgressSnapshot
   ├─ EncodeProgress / DecodeProgress
   └─ progress-enabled encode/decode/recompress methods
```

## Public symbol tree

```text
crate root
├─ modules
│  ├─ core
│  ├─ convenience
│  └─ level
├─ config + builders
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  └─ RecompressConfig / RecompressBuilder
├─ codec façades
│  ├─ Encoder / EncodeSummary
│  ├─ Decoder / DecodeSummary
│  └─ Recompressor / RecompressMode / RecompressPhase / RecompressProgress
├─ typed PCM + raw PCM boundary
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
   └─ progress-enabled methods on Encoder / Decoder / Recompressor
```

## Layer contract

| Layer | Public API surface | Ownership |
| --- | --- | --- |
| Explicit core | `flacx::core`, config/builders, codec façades, typed PCM helpers | 코덱 구성, typed PCM handoff, 명시적 encode/decode/recompress 동작, 요약 보고를 위한 단일 진실 원천입니다. |
| Convenience/orchestration | `flacx::convenience`, flat `*_file` / `*_bytes` helpers | 파일 및 바이트 워크플로, 확장자 추론, core로의 경량 라우팅을 담당합니다. |
| Support surfaces | `level`, raw PCM helpers, inspectors, progress types | 주요 아키텍처 스토리가 되지 않으면서 공개 상태를 유지하는 보조 개념입니다. |

### Key rule

아키텍처는 **explicit core에서 바깥쪽으로** 읽어야 합니다. convenience layer는 의도적으로 얇게 유지되며 크레이트의 의미적 중심으로 취급되어서는 안 됩니다.

## Current source structure snapshot

현재 공개 계약을 뒷받침하는 소스 트리는 다음과 같습니다:

```text
crates/flacx/src/
├─ lib.rs                 # public re-exports and crate contract
├─ config.rs              # EncoderConfig / DecodeConfig + builders
├─ convenience.rs         # one-shot file/byte orchestration
├─ encoder.rs             # encode façade
├─ decode.rs              # decode façade
├─ recompress.rs          # subordinate FLAC→FLAC façade
├─ pcm.rs                 # typed PCM boundary
├─ input.rs               # format-family dispatch for PCM ingest
├─ wav_input.rs           # WAV/RF64/Wave64 reader family
├─ wav_output.rs          # WAV-family writer family
├─ decode_output.rs       # decode-side temp output helpers
├─ encode_pipeline.rs     # encode planning helpers
├─ metadata.rs            # public metadata-facing helpers
├─ metadata/
│  ├─ blocks.rs           # metadata block model
│  └─ draft.rs            # metadata drafting/translation helpers
├─ read/
│  ├─ mod.rs              # FLAC read orchestration
│  ├─ frame.rs            # frame parsing/decoding
│  └─ metadata.rs         # FLAC metadata parsing + inspection
├─ write/
│  ├─ mod.rs              # FLAC write orchestration
│  └─ frame.rs            # frame/subframe serialization
└─ progress.rs            # optional progress support
```

이 트리는 의도적으로 아키텍처 중심이며 완전 열거형이 아닙니다. 즉, 모든 helper 모듈을 문서화하기보다 어떤 파일이 공개 스토리를 고정하는지 강조합니다.

## Interface map: outside-in view

```text
supported PCM container / raw PCM / FLAC
                │
                ▼
      public config + builders
                │
                ▼
  Encoder / Decoder / Recompressor
      │             │            │
      │             │            └─ subordinate FLAC→FLAC flow
      │             │
      │             └─ decode output + container writers
      │
      └─ PCM ingest dispatch + encode pipeline
                │
                ▼
        typed PCM boundary
                │
                ▼
 convenience helpers (`*_file`, `*_bytes`) route into the same core
```

## Feature-gated contract

| Feature | Public effect |
| --- | --- |
| `wav` | RIFF/WAVE, RF64, Wave64 입력/출력 표면을 활성화합니다. |
| `aiff` | AIFF와 제한된 AIFC 표면을 활성화합니다. |
| `caf` | 제한된 CAF 표면을 활성화합니다. |
| `progress` | `ProgressSnapshot`, `EncodeProgress`, `DecodeProgress`, 진행 상황 지원 메서드를 활성화합니다. |

## Public surface notes

### Config and builder surfaces
- `EncoderConfig` / `EncoderBuilder`
- `DecodeConfig` / `DecodeBuilder`
- `RecompressConfig` / `RecompressBuilder`

공개 API가 의도적으로 어떤 조정 손잡이를 노출하는지 궁금할 때 가장 먼저 봐야 할 곳입니다.

### Codec façades
- `Encoder`
- `Decoder`
- `Recompressor`

이들은 주요 명시적 워크플로를 표현하는 안정된 façade 타입입니다. recompress 경로 역시 공개되어 있지만, encode/decode 아키텍처에 종속된 것으로 서술되어야 합니다.

### Typed PCM boundary
- `PcmStream`
- `PcmStreamSpec`
- `PcmContainer`
- `read_pcm_stream`
- `write_pcm_stream`

이것은 컨테이너 adapter와 FLAC 코덱 파이프라인 사이의 이음새입니다.

### Convenience/orchestration surface
- `encode_file`, `encode_bytes`
- `decode_file`, `decode_bytes`
- `recompress_file`, `recompress_bytes`

이 helper들은 중요하지만, 별도의 아키텍처 중심이 아니라 위의 동일한 명시적 표면을 감싸는 wrapper입니다.

## Metadata and preservation note

공개 문서는 계속해서 metadata preservation을 크레이트 계약의 일부로 다뤄야 하지만, 최상위 방향성 스토리로 두어서는 안 됩니다. 특히:
- canonical private preservation chunk는 통합된 `fxmd v1` 레이아웃입니다
- 과거 `fxmd` payload variant는 의도적으로 지원되지 않습니다
- 디코드된 WAV-family 출력은 오디오 샘플이 변하지 않더라도 preservation metadata를 포함할 수 있습니다

## Documentation consistency contract

공개 문서를 업데이트할 때는 다음 표면을 정렬된 상태로 유지하세요:
1. `crates/flacx/src/lib.rs` — 크레이트 계약과 공개 re-export 맵
2. `crates/flacx/README.md` — 한눈에 보는 아키텍처 가이드
3. `docs/flacx-public-api-architecture.md` — 확장된 구조 가이드

이들 중 하나가 바뀌면 다른 둘도 드리프트가 없는지 확인해야 합니다.

## Related docs

- [`crates/flacx/src/lib.rs`](src/lib.rs) — crate rustdoc source
- [`docs/flacx-public-api-architecture.ko.md`](../../docs/flacx-public-api-architecture.ko.md) — 확장된 아키텍처 가이드
- [`../../README.ko.md`](../../README.ko.md) — 워크스페이스 개요
- [`../../docs/flacx-major-refactor-review.ko.md`](../../docs/flacx-major-refactor-review.ko.md) — 리팩터 검토와 메인터이너 체크리스트
