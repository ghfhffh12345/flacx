#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import textwrap
import wave
from pathlib import Path


FIXTURES = (
    ("mono-small", 1, 4_096),
    ("stereo-medium", 2, 8_192),
    ("stereo-large", 2, 16_384),
)
RECOMPRESS_ARGS = {
    "threads": 1,
    "level": 0,
    "block_size": 576,
}
RESPONSIBILITIES = (
    ("config / policy", ("RecompressMode", "RecompressConfig", "RecompressBuilder")),
    ("source handoff", ("FlacRecompressSource",)),
    ("session execution", ("Recompressor", "RecompressSummary")),
    ("progress", ("RecompressPhase", "RecompressProgress", "RecompressProgressSink", "EncodePhaseProgress")),
    ("verification", ("VerifyingPcmStream",)),
)
AUDIT_SYMBOLS = (
    "RecompressConfig",
    "RecompressBuilder",
    "Recompressor",
    "FlacRecompressSource",
    "RecompressProgressSink",
    "EncodePhaseProgress",
    "VerifyingPcmStream",
)


class EvidenceError(Exception):
    pass


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def ensure_release_binary(root: Path) -> Path:
    binary = root / "target" / "release" / "flacx"
    if not binary.exists():
        raise EvidenceError(f"missing release binary: {binary}")
    return binary


def run_command(cmd: list[str], *, cwd: Path) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if proc.returncode != 0:
        raise EvidenceError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def load_authority_compare(compare_json_path: Path) -> dict[str, object] | None:
    if not compare_json_path.exists():
        return None
    try:
        return json.loads(compare_json_path.read_text())
    except json.JSONDecodeError as exc:
        raise EvidenceError(f"invalid authority compare json at {compare_json_path}: {exc}") from exc


def locate_binding_corpus(repo_root: Path) -> Path:
    candidates = [repo_root / "test-wavs"]
    candidates.extend(parent / "test-wavs" for parent in repo_root.parents)
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return repo_root / "test-wavs"


def sample_value(frame_index: int, channel_index: int) -> int:
    raw = ((frame_index * (channel_index + 3) * 97) % 65_536) - 32_768
    return max(-32_768, min(32_767, raw))


def write_fixture_wav(path: Path, channels: int, frames: int) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as handle:
        handle.setnchannels(channels)
        handle.setsampwidth(2)
        handle.setframerate(44_100)
        payload = bytearray()
        for frame_index in range(frames):
            for channel_index in range(channels):
                payload.extend(struct.pack("<h", sample_value(frame_index, channel_index)))
        handle.writeframes(payload)


def generate_fixture_corpus(root: Path) -> list[dict[str, object]]:
    fixtures: list[dict[str, object]] = []
    for stem, channels, frames in FIXTURES:
        wav_path = root / f"{stem}.wav"
        write_fixture_wav(wav_path, channels, frames)
        fixtures.append(
            {
                "fixture": stem,
                "wav_path": wav_path,
                "channels": channels,
                "frames": frames,
            }
        )
    return fixtures


def generate_canonical_flac_corpus(baseline_binary: Path, wav_root: Path, flac_root: Path, cwd: Path) -> None:
    if flac_root.exists():
        shutil.rmtree(flac_root)
    flac_root.mkdir(parents=True, exist_ok=True)
    run_command(
        [str(baseline_binary), "encode", str(wav_root), "-o", str(flac_root), "--depth", "0"],
        cwd=cwd,
    )


def run_recompress(binary: Path, cwd: Path, input_flac: Path, output_flac: Path) -> None:
    run_command(
        [
            str(binary),
            "recompress",
            str(input_flac),
            "-o",
            str(output_flac),
            "--threads",
            str(RECOMPRESS_ARGS["threads"]),
            "--level",
            str(RECOMPRESS_ARGS["level"]),
            "--block-size",
            str(RECOMPRESS_ARGS["block_size"]),
        ],
        cwd=cwd,
    )


def recompress_source_files(src_root: Path) -> list[Path]:
    files = []
    for path in src_root.rglob("*.rs"):
        rel = path.relative_to(src_root)
        if rel.name.startswith("recompress") or "recompress" in rel.parts:
            files.append(path)
    return sorted(set(files))


def find_symbol_locations(paths: list[Path], symbol: str) -> list[str]:
    pattern = re.compile(rf"\b(?:pub(?:\(crate\))?\s+)?(?:struct|enum|trait)\s+{re.escape(symbol)}\b")
    matches: list[str] = []
    for path in paths:
        for line_number, line in enumerate(path.read_text().splitlines(), start=1):
            if pattern.search(line):
                matches.append(f"{path.as_posix()}:{line_number}")
    return matches


def write_ownership_map(repo_root: Path, out_path: Path) -> None:
    src_root = repo_root / "crates" / "flacx" / "src"
    files = recompress_source_files(src_root)
    line_items = []
    for path in files:
        rel = path.relative_to(repo_root)
        line_count = len(path.read_text().splitlines())
        line_items.append((rel, line_count))

    lines = [
        "# Recompress Ownership Map",
        "",
        "Generated by `python3 scripts/recompress_evidence.py ...` to keep the recompress",
        "follow-up audit grounded in the current tree.",
        "",
        "## Recompress-owned files",
        "",
    ]
    if not line_items:
        lines.append("- No `recompress*` source files were found.")
    else:
        for rel, line_count in line_items:
            lines.append(f"- `{rel}` — {line_count} LOC")

    lines.extend(
        [
            "",
            "## Responsibility map",
            "",
            "| Concern | Symbols | Current locations |",
            "| --- | --- | --- |",
        ]
    )
    for concern, symbols in RESPONSIBILITIES:
        locations = []
        for symbol in symbols:
            locations.extend(find_symbol_locations(files, symbol))
        formatted_locations = "<br>".join(f"`{location}`" for location in locations) or "_missing_"
        lines.append(f"| {concern} | `{', '.join(symbols)}` | {formatted_locations} |")

    shared_defs = []
    shared_pattern = re.compile(r"\b(?:struct|enum|trait)\s+\w*Recompress\w*\b")
    for path in sorted(src_root.rglob("*.rs")):
        if path in files:
            continue
        for line_number, line in enumerate(path.read_text().splitlines(), start=1):
            if shared_pattern.search(line):
                shared_defs.append(f"{path.relative_to(repo_root).as_posix()}:{line_number}")
    lines.extend(
        [
            "",
            "## Shared-abstraction audit",
            "",
            "- Recompress-specific type definitions outside `recompress*` files:",
        ]
    )
    if shared_defs:
        lines.extend([f"  - `{location}`" for location in shared_defs])
    else:
        lines.append("  - none")

    lines.extend(
        [
            "",
            "## Structural audit commands",
            "",
            "```bash",
            'rg -n "struct VerifyingPcmStream|struct EncodePhaseProgress|trait RecompressProgressSink|pub struct Recompressor|pub struct FlacRecompressSource|pub struct RecompressConfig" crates/flacx/src/recompress* crates/flacx/src/recompress/*.rs',
            'rg -n "recompress_file|recompress_bytes" crates/flacx/src crates/flacx/tests README.md crates/flacx/README.md docs',
            "```",
            "",
            "## Review notes",
            "",
            "- The report is descriptive, not normative: it shows the current placement of recompress-owned concerns.",
            "- The binding architecture rule remains unchanged: recompress stays downstream of the shared encode/decode spine.",
        ]
    )
    write_text(out_path, "\n".join(lines) + "\n")


def write_benchmark_scaffold(
    repo_root: Path,
    baseline_worktree: Path,
    out_path: Path,
    compare_json_path: Path,
) -> None:
    head_binary = repo_root / "target" / "release" / "flacx"
    baseline_binary = baseline_worktree / "target" / "release" / "flacx"
    corpus_root = locate_binding_corpus(repo_root)
    compare = load_authority_compare(compare_json_path)
    authority_status = "PENDING"
    authority_details = [
        f"- Authority compare json present: {'yes' if compare is not None else 'no'} (`{compare_json_path}`)",
    ]
    if compare is not None:
        pass_per_workload = bool(compare.get("pass_per_workload"))
        pass_aggregate = bool(compare.get("pass_aggregate"))
        geometric_mean_ratio = compare.get("geometric_mean_ratio")
        per_workload_gate = compare.get("per_workload_gate", 1.05)
        aggregate_gate = compare.get("aggregate_gate", 1.03)
        authority_status = "PASS" if pass_per_workload and pass_aggregate else "FAIL"
        authority_details.extend(
            [
                f"- Current authority status: {authority_status}",
                f"- Per-workload gate <= {per_workload_gate:.2f}x baseline: {'PASS' if pass_per_workload else 'FAIL'}",
                f"- Aggregate geomean gate <= {aggregate_gate:.2f}x baseline: {'PASS' if pass_aggregate else 'FAIL'}",
                f"- Aggregate geomean ratio: {geometric_mean_ratio:.4f}" if isinstance(geometric_mean_ratio, (int, float)) else "- Aggregate geomean ratio: unavailable",
            ]
        )
    else:
        authority_details.extend(
            [
                "- Current authority status: PENDING",
                "- Run `python3 scripts/cli_perf_compare.py ...` to populate the v0.8.2 authority result.",
            ]
        )
    lines = [
        "# Recompress Logic Refactor Benchmark Scaffold",
        "",
        "This scaffold keeps the recompress-specific verification lane explicit even while the implementation is moving.",
        "",
        "## Readiness",
        "",
        f"- HEAD release binary present: {'yes' if head_binary.exists() else 'no'} (`{head_binary}`)",
        f"- v0.8.2 release binary present: {'yes' if baseline_binary.exists() else 'no'} (`{baseline_binary}`)",
        f"- Binding corpus present: {'yes' if corpus_root.exists() else 'no'} (`{corpus_root}`)",
        "",
        "## Authority status",
        "",
        *authority_details,
        "",
        "## Commands",
        "",
        "```bash",
        "cargo bench -p flacx --bench throughput -- --noplot",
        f"python3 scripts/cli_perf_compare.py --baseline-worktree {baseline_worktree} --corpus {corpus_root} --out-dir .omx/reports/cli-perf/recompress-logic-refactor",
        f"python3 scripts/recompress_evidence.py --baseline-worktree {baseline_worktree} --out-dir .omx/reports",
        "```",
        "",
        "## Expected benchmark ids",
        "",
        "- `encode_corpus_throughput`",
        "- `decode_corpus_throughput`",
        "- `recompress_corpus_throughput`",
        "",
        "## Artifact targets",
        "",
        "- `.omx/reports/recompress-benchmark-compare/recompress-logic-refactor.md`",
        "- `.omx/reports/recompress-corpus-diff/recompress-logic-refactor.json`",
        "- `.omx/reports/architecture/recompress-ownership-map.md`",
        "- `.omx/reports/cli-perf/recompress-logic-refactor/v0.8.2-vs-head.json`",
        "- `.omx/reports/cli-perf/recompress-logic-refactor/v0.8.2-vs-head.md`",
        "",
        "## Binding gate reminder",
        "",
        "- Performance authority remains the historical v0.8.2 compare (`scripts/cli_perf_compare.py`), not the micro-corpus diff in this script.",
        "- The micro-corpus diff is a fast byte-level recompress guardrail for the refactor lane.",
    ]
    write_text(out_path, "\n".join(lines) + "\n")


def write_corpus_diff(repo_root: Path, baseline_worktree: Path, out_path: Path) -> None:
    baseline_binary = ensure_release_binary(baseline_worktree)
    head_binary = ensure_release_binary(repo_root)

    with tempfile.TemporaryDirectory(prefix="flacx-recompress-evidence-") as temp_dir:
        temp_root = Path(temp_dir)
        wav_root = temp_root / "wav"
        flac_root = temp_root / "flac"
        baseline_root = temp_root / "baseline-out"
        head_root = temp_root / "head-out"

        fixtures = generate_fixture_corpus(wav_root)
        generate_canonical_flac_corpus(baseline_binary, wav_root, flac_root, repo_root)
        baseline_root.mkdir(parents=True, exist_ok=True)
        head_root.mkdir(parents=True, exist_ok=True)

        records = []
        for fixture in fixtures:
            stem = fixture["fixture"]
            input_flac = flac_root / f"{stem}.flac"
            baseline_output = baseline_root / f"{stem}.flac"
            head_output = head_root / f"{stem}.flac"
            run_recompress(baseline_binary, baseline_worktree, input_flac, baseline_output)
            run_recompress(head_binary, repo_root, input_flac, head_output)
            records.append(
                {
                    "fixture": stem,
                    "input_flac_bytes": input_flac.stat().st_size,
                    "current_bytes": head_output.stat().st_size,
                    "baseline_bytes": baseline_output.stat().st_size,
                    "byte_equal": baseline_output.read_bytes() == head_output.read_bytes(),
                    "current_sha256": sha256_file(head_output),
                    "baseline_sha256": sha256_file(baseline_output),
                }
            )

    payload = {
        "baseline_worktree": str(baseline_worktree),
        "head_root": str(repo_root),
        "args": RECOMPRESS_ARGS,
        "fixtures": records,
        "all_byte_equal": all(record["byte_equal"] for record in records),
    }
    write_text(out_path, json.dumps(payload, indent=2) + "\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate recompress refactor audit/evidence artifacts."
    )
    parser.add_argument("--baseline-worktree", required=True)
    parser.add_argument("--out-dir", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    repo_root = Path.cwd().resolve()
    baseline_worktree = Path(args.baseline_worktree).resolve()
    out_root = Path(args.out_dir).resolve()
    compare_json_path = out_root / "cli-perf" / "recompress-logic-refactor" / "v0.8.2-vs-head.json"

    if not baseline_worktree.exists():
        raise EvidenceError(f"baseline worktree missing: {baseline_worktree}")

    write_ownership_map(
        repo_root,
        out_root / "architecture" / "recompress-ownership-map.md",
    )
    write_benchmark_scaffold(
        repo_root,
        baseline_worktree,
        out_root / "recompress-benchmark-compare" / "recompress-logic-refactor.md",
        compare_json_path,
    )
    write_corpus_diff(
        repo_root,
        baseline_worktree,
        out_root / "recompress-corpus-diff" / "recompress-logic-refactor.json",
    )

    print(out_root / "architecture" / "recompress-ownership-map.md")
    print(out_root / "recompress-benchmark-compare" / "recompress-logic-refactor.md")
    print(out_root / "recompress-corpus-diff" / "recompress-logic-refactor.json")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except EvidenceError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
