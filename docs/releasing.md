# Release automation

This repository is set up for tag-driven release automation using `v*` tags.
The workflow validates the tag against the effective `flacx` package version
and then branches into one of two paths:

- **Final release tag**: publish the `flacx` library crate to crates.io and
  create a GitHub release.
- **Pre-release tag**: create a GitHub prerelease only and skip crates.io
  publishing.

## Tag contract

- Only tags that match `v*` trigger the release flow.
- The workflow computes the effective `flacx` package version from the Cargo
  manifests before any publish or release step runs.
- Final tags must match that effective `flacx` version exactly.
- Pre-release tags must match the same core version and use semver prerelease
  identifiers such as `-rc1`.
- Semver prerelease identifiers determine prerelease handling.

Examples:

- `v0.1.0` → final release
- `v0.1.0-rc1` → GitHub prerelease only

## What gets published

- crates.io receives only the `flacx` library crate.
- GitHub receives only the built-in tagged source archive for the repository.
- No binaries, installers, package-manager artifacts, or separate CLI bundles
  are attached.
- `flacx-cli` stays out of the publish path.

## Required setup

1. Store a crates.io API token in GitHub Actions as a secret named
   `CARGO_REGISTRY_TOKEN`.
2. Expose that secret only to the final-release publish job.
3. Give the release-creating job permission to create GitHub releases.
4. Keep the release workflow scoped to `push.tags: ['v*']`.

## Manual recovery

If the crates.io publish succeeds but GitHub release creation fails:

1. Do **not** republish the crate.
2. Re-run or manually complete only the GitHub release creation step for the
   same tag.
3. Use the same tagged source archive from GitHub.

## Notes for maintainers

- The release workflow is intentionally fail-closed: a version mismatch should
  stop the process before any side effects.
- The release docs describe the current workspace layout without implying that
  the CLI crate is published separately.
