# Release automation

이 저장소는 `v*` 태그에 기반한 태그 주도 release automation을 사용합니다. 이 워크플로는 태그를 Cargo manifest의 유효 `flacx` 패키지 버전과 대조한 다음 두 경로 중 하나를 따릅니다:

- **Final release tag** — `flacx` 라이브러리 크레이트를 crates.io에 게시하고 GitHub 릴리스를 생성
- **Prerelease tag** — GitHub prerelease만 생성하고 crates.io 게시를 건너뜀

릴리스 흐름은 의도적으로 fail-closed입니다. 버전 불일치는 어떤 부수 효과가 일어나기 전에 프로세스를 멈춰야 합니다.

## Release prep order

어떤 태그도 push하기 전에 다음 순서로 릴리스를 준비하세요:

1. 저장소 근거를 바탕으로 release-prep 파일을 업데이트한다.
2. 계획된 release-prep 변경에 필요하다면 cargo 명령을 통해 `Cargo.lock`을 자동으로 새로 고친다.
3. 로컬에서 검증, 빌드, 테스트를 수행한다.
4. 계획된 새로 고침이 `Cargo.lock`을 바꿨다면 그것까지 포함해 추적되는 release-prep 파일을 stage한다.
5. release-prep gate를 실행한다.
6. 계획된 `Cargo.lock` delta를 포함해 release-prep 변경을 커밋한다.
7. 브랜치를 push하고 태그를 만든 뒤 release workflow에 넘긴다.

`Cargo.lock`을 수동으로 편집하지 마세요.

생성된 `.omx/logs/release-notes-v<version>.md` 파일은 로컬 전용으로 유지되며 절대 stage하거나 commit하지 않습니다.

## Tag contract

- `v*`와 일치하는 태그만 release flow를 트리거합니다.
- 워크플로는 어떤 publish 또는 release 단계가 실행되기 전에 Cargo manifest에서 유효 `flacx` 패키지 버전을 계산합니다.
- Final tag는 해당 유효 `flacx` 버전과 정확히 일치해야 합니다.
- Prerelease tag는 같은 core version과 `-rc1` 같은 semver prerelease 식별자를 사용해야 합니다.
- Semver prerelease 식별자가 prerelease 처리를 결정합니다.

예시:

- `v0.1.0` → final release
- `v0.1.0-rc1` → GitHub prerelease only

## What gets published

- crates.io는 `flacx` 라이브러리 크레이트만 받습니다.
- GitHub는 저장소의 내장 태그 소스 아카이브만 받습니다.
- 바이너리, 설치 프로그램, 패키지 매니저 아티팩트, 별도 CLI 번들은 첨부되지 않습니다.
- `flacx-cli`는 publish 경로에 포함되지 않습니다.

## Required setup

1. crates.io API 토큰을 GitHub Actions의 `CARGO_REGISTRY_TOKEN` 시크릿으로 저장한다.
2. 그 시크릿은 final-release publish job에만 노출한다.
3. release를 생성하는 job에 GitHub release 생성 권한을 부여한다.
4. release workflow가 `push.tags: ['v*']`로 제한되도록 유지한다.

## Manual recovery

release workflow는 승인된 수정 이후 GitHub release 생성을 자동으로 완료해야 합니다. Manual recovery는 정상 경로의 일부가 아닙니다.

만약 crates.io 게시에는 성공했지만 외부 workflow 문제 때문에 GitHub release 생성이 여전히 실패한다면:

1. 크레이트를 **다시 게시하지 않는다**.
2. 동일한 태그에 대해 GitHub release 생성 단계만 다시 실행하거나 수동으로 완료한다.
3. GitHub의 동일한 tagged source archive를 사용한다.

## Notes for maintainers

- release docs는 CLI 크레이트가 별도로 게시된다는 인상을 주지 않으면서 현재 워크스페이스 레이아웃을 설명합니다.
- 로컬 release note는 저장소 근거에서 생성되며 release commit에는 포함되지 않습니다.
- 태그에 인코딩된 버전과 Cargo manifest 버전이 어긋나면, 워크플로는 어떤 publish 또는 release 부수 효과보다 먼저 멈춰야 합니다.
