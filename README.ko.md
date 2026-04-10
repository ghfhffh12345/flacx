# flacx workspace

Rust로 작성된 고성능 PCM 컨테이너/FLAC 변환 및 FLAC 재압축 워크스페이스입니다.

> Warning: flacx is still experimental; APIs, CLI flags, and metadata details may change without notice.

이 저장소는 사용자에게 노출되는 두 개의 크레이트를 가진 Cargo 워크스페이스입니다:

- `crates/flacx` — 게시 가능한 라이브러리 크레이트
- `crates/flacx-cli` — 동일한 파이프라인 위에 구축되었지만 게시되지는 않는 CLI 크레이트

이 워크스페이스에는 메인터이너 문서와 로컬 개발 보조 도구도 포함됩니다. 공개 문서는 지원되는 라이브러리 및 CLI 워크플로에 계속 초점을 맞춥니다.

## Workspace layout

```text
.
├─ crates/
│  ├─ flacx/       # publishable library crate
│  └─ flacx-cli/   # workspace CLI crate
└─ docs/           # maintainer documentation
```

## Quick start

### Library

프로젝트에 라이브러리 크레이트를 추가하세요:

```toml
[dependencies]
flacx = "0.8.2"
```

라이브러리는 기본적으로 내장 `wav`, `aiff`, `caf` 컨테이너 패밀리를 활성화합니다. Rust에서 콜백 기반 진행 상황 보고가 필요하다면 `progress`를 추가하세요.

그런 다음 Rust에서 지원되는 PCM 컨테이너를 FLAC으로 인코드할 수 있습니다:

```rust
use flacx::{Encoder, EncoderConfig, level::Level};

let config = EncoderConfig::builder()
    .level(Level::Level8)
    .threads(4)
    .build();

Encoder::new(config)
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

그리고 FLAC을 다시 지원되는 PCM 컨테이너로 디코드할 수 있습니다:

```rust
use flacx::Decoder;

Decoder::default()
    .decode_file("input.flac", "output.wav")
    .unwrap();
```

편의 헬퍼 대신 명시적인 adapter/core 경로를 사용하고 싶다면, 라이브러리 크레이트는 `flacx::core::{PcmStream, read_pcm_stream,
write_pcm_stream, Encoder, Decoder}`도 노출합니다.

크레이트 중심 사용 가이드는 [`crates/flacx/README.ko.md`](crates/flacx/README.ko.md)를 참고하세요.

### CLI

워크스페이스 루트에서 릴리스 바이너리를 빌드하세요:

```bash
cargo build --release
```

그런 다음 `target/release/`에서 `flacx`를 직접 실행하세요(또는 해당 디렉터리를 `PATH`에 추가한 후 실행하세요):

```bash
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
flacx decode input.flac -o output.aiff --threads 4
flacx decode encoded-album -o decoded-album --depth 0
```

지원되는 CLI 형태:

- `flacx encode <input> [-o <output-or-dir>] [--depth <depth>]`
- `flacx decode <input> [-o <output-or-dir>] [--depth <depth>]`
- encode 전용 플래그:
  - `--output`
  - `--level`
  - `--threads`
  - `--block-size`
  - `--depth`
- decode 전용 플래그:
  - `--output`
  - `--threads`
  - `--depth`

인코드/디코드 기본값과 폴더 동작:

- `-o` 없는 단일 파일 입력은 소스 PCM 컨테이너 옆에 형제 `.flac`를 씁니다
- `-o` 없는 폴더 입력은 발견된 각 `.wav`, `.rf64`, `.w64`, `.aif`, `.aiff`, `.aifc`, `.caf` 옆에 형제 `.flac`를 씁니다
- `-o <dir>`가 있는 폴더 입력은 대상 루트 아래에 상대 하위 경로를 보존합니다
- `-o` 없는 단일 파일 decode 입력은 소스 FLAC 옆에 형제 `.wav`를 씁니다
- `-o` 없는 폴더 decode 입력은 발견된 각 FLAC 옆에 형제 `.wav`를 씁니다
- `-o <dir>`가 있는 폴더 decode 입력은 대상 루트 아래에 상대 하위 경로를 보존합니다
- 명시적 decode 출력 경로는 `.wav`, `.rf64`, `.w64`, `.aif`, `.aiff`, `.aifc`, `.caf`를 대상으로 할 수 있습니다
- decode 디렉터리 output-family 오버라이드는 명시적이며, 선택자가 없으면 배치 출력은 여전히 `.wav`를 기본값으로 사용합니다
- `--depth` 기본값은 `1`이며 디렉터리 입력에만 영향을 주고, 무제한 순회에는 `0`을 사용합니다
- encode `--threads` 기본값은 `8`입니다
- raw PCM 인코드는 `--raw`와 descriptor 플래그를 통한 명시적 방식만 지원되며, 일반 `.raw` / `.pcm` 파일은 자동 탐색되지 않습니다
- raw PCM은 여전히 입력 전용이며 decode/output 패밀리가 아닙니다

진행 상황 표시:

- 인터랙티브 터미널은 인코드와 디코드 중 라이브 진행 줄을 표시합니다
- 리디렉션되었거나 비인터랙티브 실행에서는 진행 UI를 출력하지 않습니다
- 단일 파일 실행은 현재 파일명, 경과 시간, ETA, 속도를 표시합니다
- 폴더 실행은 전체 배치 진행 상황과 파일별 진행 상황을 별도의 라이브 줄에 표시합니다
- 배치 진행 상황은 계획된 전체 작업 목록에 걸친 정확한 처리 샘플 수를 사용합니다

CLI 사용 세부사항은 [`crates/flacx-cli/README.ko.md`](crates/flacx-cli/README.ko.md)를 참고하세요.

## Workspace commands

```bash
cargo build --workspace
cargo test --workspace
flacx --help
cargo run -p flacx --release --example benchmark
```

## Performance note

라이브러리와 CLI는 동일한 튜닝된 인코드/디코드 파이프라인을 공유합니다. 벤치마크는 각 크레이트의 `benches/` 디렉터리 아래에 있으며 `cargo bench`로 실행합니다. 일반 사용자는 워크스페이스를 빌드하거나 사용하는 데 그 워크플로가 필요하지 않습니다.

## Releases

태그 릴리스는 `v*` 태그를 사용합니다:

- 최종 태그는 `flacx` 라이브러리 크레이트를 crates.io에 게시하고 GitHub 릴리스를 생성합니다
- `v1.2.3-rc1`과 같은 프리릴리스 태그는 GitHub 프리릴리스만 생성합니다
- GitHub 릴리스 페이지는 저장소의 내장 태그 소스 아카이브만 사용하며, 바이너리, 설치 프로그램, 별도의 CLI 번들은 첨부되지 않습니다

릴리스 워크플로 세부사항, 필요한 시크릿 설정, 수동 복구 메모는 [`docs/releasing.ko.md`](docs/releasing.ko.md)를 참고하세요.

## Documentation map

- [`crates/flacx/README.ko.md`](crates/flacx/README.ko.md) — 크레이트 수준 공개 API 아키텍처 가이드
- [`docs/flacx-public-api-architecture.ko.md`](docs/flacx-public-api-architecture.ko.md) — 현재 공개 표면과 소스 구조에 대한 확장된 메인터이너 지향 가이드
- [`docs/flacx-major-refactor-review.ko.md`](docs/flacx-major-refactor-review.ko.md) — explicit-core / convenience-layer 리팩터에 대한 메인터이너 가이드
- [`crates/flacx-cli/README.ko.md`](crates/flacx-cli/README.ko.md) — CLI 사용자 가이드
- [`docs/releasing.ko.md`](docs/releasing.ko.md) — 메인터이너 릴리스 워크플로
