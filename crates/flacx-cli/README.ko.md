# flacx-cli

`flacx-cli`는 PCM 컨테이너/FLAC 변환과 FLAC 재압축을 위한 워크스페이스 명령줄 인터페이스입니다.
동일한 encode/decode 파이프라인을 `flacx` 라이브러리 크레이트와 공유하며,
게시 가능한 라이브러리 패키지와는 분리되어 유지됩니다.

> Warning: flacx-cli is still experimental. The current `fxmd` layout is canonical `v1`; mode presets only adjust capture/emission and validation around that format.

## Run it locally

워크스페이스 루트에서 릴리스 바이너리를 빌드하세요:

```bash
cargo build --release
```

그런 다음 `target/release/`에서 `flacx`를 직접 실행하세요(또는 해당 디렉터리를 `PATH`에 추가한 후 실행하세요):

```bash
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
flacx decode input.flac -o output.aifc --threads 4
flacx decode encoded-album -o decoded-album --depth 0
```

## Command model

CLI는 세 개의 최상위 명령을 노출합니다:

- `flacx encode <input> [-o <output-or-dir>] [--level <0-8>] [--threads <n>] [--block-size <samples>] [--mode <loose|default|strict>] [--depth <n>]`
- `flacx decode <input> [-o <output-or-dir>] [--threads <n>] [--mode <loose|default|strict>] [--depth <n>]`
- `flacx recompress <input> [-o <output-or-dir>] [--in-place] [--level <0-8>] [--threads <n>] [--block-size <samples>] [--mode <loose|default|strict>] [--depth <n>]`

입력은 단일 파일이거나 디렉터리 트리일 수 있습니다.
디렉터리 순회는 `--depth`로 제어됩니다.

## Encode

### Flags

- `-o, --output <path>`
- `--output-family <wave|rf64|w64|aiff|aifc|caf>` (directory decode only)
- `--level <0-8>`
- `--threads <n>`
- `--block-size <samples>`
- `--mode <loose|default|strict>`
- `--depth <n>`

### Defaults and behavior

- `--level` 기본값은 `8`입니다.
- `--threads` 기본값은 `8`입니다.
- `--block-size`는 선택 사항이며, 생략하면 블록 크기는 선택된 압축 레벨에서 가져옵니다.
- `--mode` 기본값은 `default`입니다.
- `--depth` 기본값은 `1`입니다.
- `--depth`는 디렉터리 입력에만 영향을 줍니다.
- 무제한 재귀 순회에는 `--depth 0`을 사용하세요.
- `-o` 없는 단일 파일 입력은 소스 PCM 컨테이너 옆에 형제 `.flac`를 씁니다.
- 단일 파일 입력에서 `-o <path>`는 해당 정확한 파일 경로에 씁니다.
- `-o` 없는 디렉터리 입력은 발견된 각 `.wav`, `.rf64`, `.w64`, `.aif`, `.aiff`, `.aifc`, `.caf` 옆에 형제 `.flac`를 씁니다.
- raw PCM 인코드는 `--raw`와 descriptor 플래그를 통한 명시적 방식만 지원되며, 일반 `.raw` / `.pcm` 파일은 자동 탐색되지 않습니다.
- `-o <dir>`가 있는 디렉터리 입력은 대상 디렉터리 아래에 상대 하위 경로를 보존합니다.
- 단일 파일 입력에서 `-o`는 파일 경로여야 합니다.
- 디렉터리 입력에서 `-o`는 디렉터리 경로여야 합니다.

### Examples

```bash
flacx encode input.wav
flacx encode input.w64 -o output.flac --level 8 --threads 4
flacx encode input.aiff -o output.flac --threads 4
flacx encode input.caf -o output.flac --threads 4
flacx encode input.pcm --raw --sample-rate 44100 --channels 2 --bits-per-sample 16 --container-bits 16 --byte-order le -o output.flac
flacx encode album-dir -o encoded-album --depth 0
```

## Decode

### Flags

- `-o, --output <path>`
- `--threads <n>`
- `--mode <loose|default|strict>`
- `--depth <n>`

### Defaults and behavior

- `--threads`는 선택 사항입니다.
- 생략하면 decode 경로는 라이브러리 기본 thread 수를 사용합니다.
- `--mode` 기본값은 `default`입니다.
- `--depth` 기본값은 `1`입니다.
- `--depth`는 디렉터리 입력에만 영향을 줍니다.
- 무제한 재귀 순회에는 `--depth 0`을 사용하세요.
- `-o` 없는 단일 파일 입력은 소스 FLAC 옆에 형제 `.wav`를 씁니다.
- 단일 파일 입력에서 `-o <path>`는 해당 정확한 파일 경로에 씁니다.
- `-o` 없는 디렉터리 입력은 발견된 각 FLAC 옆에 형제 `.wav`를 씁니다.
- `-o <dir>`가 있는 디렉터리 입력은 대상 디렉터리 아래에 상대 하위 경로를 보존합니다.
- 명시적 decode 출력 경로는 `.wav`, `.rf64`, `.w64`, `.aif`, `.aiff`, `.aifc`, `.caf`를 대상으로 할 수 있습니다.
- `--output-family`는 디렉터리 decode에만 적용되며 배치 출력 확장자를 균일하게 변경합니다.
- `--mode loose`는 `fxmd` capture/emission을 비활성화하고 완화 가능한 validation도 비활성화합니다.
- `--mode default`는 canonical `fxmd v1` 동작을 보존하고 잘못되었거나 중복된 `fxmd` payload를 거부합니다.
- `--mode strict`는 canonical `fxmd v1` 동작을 보존하고, 완화 가능한 validation 세트를 활성화하며, 잘못되었거나 중복된 `fxmd` payload를 거부합니다.
- 단일 파일 입력에서 `-o`는 파일 경로여야 합니다.
- 디렉터리 입력에서 `-o`는 디렉터리 경로여야 합니다.

### Examples

```bash
flacx decode input.flac
flacx decode input.flac -o output.w64 --threads 4
flacx decode input.flac -o output.caf --threads 4
flacx decode album-dir -o decoded-album --output-family aiff --depth 0
flacx decode encoded-album -o decoded-album --depth 0
flacx decode input.flac --mode loose
flacx decode input.flac --mode strict
```

## Recompress

### Flags

- `-o, --output <path>`
- `--in-place`
- `--level <0-8>`
- `--threads <n>`
- `--block-size <samples>`
- `--mode <loose|default|strict>`
- `--depth <n>`

### Defaults and behavior

- `--level` 기본값은 `8`입니다.
- `--threads` 기본값은 `8`입니다.
- `--block-size`는 선택 사항이며, 생략하면 블록 크기는 선택된 압축 레벨에서 가져옵니다.
- `--mode` 기본값은 `default`입니다.
- `--depth` 기본값은 `1`입니다.
- `--depth`는 디렉터리 입력에만 영향을 줍니다.
- 무제한 재귀 순회에는 `--depth 0`을 사용하세요.
- `-o` 없는 단일 파일 입력은 소스 FLAC 옆에 형제 `.recompressed.flac`를 씁니다.
- 단일 파일 입력에서 `-o <path>`는 해당 정확한 파일 경로에 씁니다.
- `--in-place`는 성공적인 재압축 후 소스 FLAC을 교체하도록 명시적으로 opt-in 합니다.
- `--in-place`는 `-o`와 호환되지 않습니다.
- `--in-place` 없이 동일 경로 출력은 거부됩니다.
- `-o` 없는 디렉터리 입력은 발견된 각 FLAC 옆에 형제 `.recompressed.flac`를 씁니다.
- `-o <dir>`가 있는 디렉터리 입력은 대상 디렉터리 아래에 상대 하위 경로를 보존하며 출력 루트가 다르므로 원래 파일명을 유지합니다.
- `--in-place`가 있는 디렉터리 입력은 각 성공적인 temp-file commit 이후 발견된 소스 파일을 제자리에서 다시 씁니다.
- 디렉터리 in-place overwrite는 전체 배치에 대한 all-or-nothing 트랜잭션이 아니라 파일별 atomic 동작입니다.
- `--mode`는 metadata 처리와 validation을 기존 loose/default/strict 정책 모델과 정렬된 상태로 유지합니다.
- 단일 파일 입력에서 `-o`는 파일 경로여야 합니다.
- 디렉터리 입력에서 `-o`는 디렉터리 경로여야 합니다.

### Examples

```bash
flacx recompress input.flac
flacx recompress input.flac -o input.recompressed.flac --level 0 --threads 4
flacx recompress input.flac --in-place --level 0 --threads 4
flacx recompress album-dir -o recompressed-album --depth 0
flacx recompress album-dir --in-place --depth 0
```

## Output layout summary

| Input shape | `-o` omitted | `-o <file>` | `-o <dir>` |
| --- | --- | --- | --- |
| Single file | 소스 파일 옆의 형제 출력 | 정확한 파일 경로 | 거부됨 |
| Directory | 발견된 각 파일 옆의 형제 출력 | 거부됨 | 대상 루트 아래에 상대 하위 경로 보존 |

## Progress display

CLI는 표준 오류가 인터랙티브 터미널에 연결되어 있을 때만 진행 상황을 렌더링합니다.

- 인터랙티브 터미널은 라이브 encode/decode/recompress 진행 줄을 표시합니다
- 리디렉션되었거나 비인터랙티브 실행은 진행 UI를 억제합니다
- 진행 데이터는 라이브러리 progress hook에서 오며, CLI는 그것만 렌더링합니다
- 단일 파일 실행은 현재 파일명, 퍼센트, 경과 시간, ETA, 속도를 표시합니다
- 디렉터리 실행은 전체 배치 진행 상황과 파일별 진행 상황을 별도의 라이브 줄에 표시합니다
- recompress 진행 상황은 phase-aware이며 decode와 encode 작업을 모두 보고합니다
- 배치 진행 총계는 계획된 전체 작업 목록에 걸친 정확한 샘플 수를 사용합니다
- ETA와 속도는 두 번의 전진 업데이트와 최소 250ms의 경과 진행 시간이 관찰될 때까지 짧은 warm-up 상태에 머뭅니다

## Relationship to the library crate

- `crates/flacx`는 재사용 가능한 Rust API를 제공합니다.
- `crates/flacx-cli`는 최종 사용자 CLI를 제공합니다.
- 두 크레이트는 동일한 워크스페이스 버전과 동일한 encode/decode 파이프라인을 공유합니다
- CLI는 별도의 게시 대상이 아니라 라이브러리 위의 얇은 adapter입니다

라이브러리 API 가이드는 `crates/flacx/README.ko.md`를 참고하세요.
워크스페이스 수준 맥락은 저장소 루트의 `README.ko.md`를 참고하세요.
