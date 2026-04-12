#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any


DEFAULT_RUNS = 5
DEFAULT_SINGLE_FILE = "test1.wav"
TRACE_ENV = "FLACX_PROGRESS_TRACE"


@dataclass
class RunResult:
    wall_seconds: float
    file_finish_seconds: float
    planning_overhead_seconds: float
    returncode: int
    stdout: str
    stderr: str
    trace_path: str | None
    output_path: str


@dataclass
class WorkloadResult:
    name: str
    mode: str
    source: str
    baseline_runs: list[RunResult]
    head_runs: list[RunResult]
    baseline_median_wall_seconds: float
    head_median_wall_seconds: float
    ratio_head_over_baseline: float
    baseline_median_file_finish_seconds: float
    head_median_file_finish_seconds: float
    baseline_median_planning_overhead_seconds: float
    head_median_planning_overhead_seconds: float


@dataclass
class CompareReport:
    corpus: str
    baseline_worktree: str
    head_binary: str
    baseline_binary: str
    generated_flac_root: str
    runs: int
    single_file: str
    generated_layout: list[str]
    test_wavs_sha256: dict[str, str]
    generated_flac_sha256: dict[str, str]
    workloads: list[WorkloadResult]
    geometric_mean_ratio: float
    per_workload_gate: float
    aggregate_gate: float
    pass_per_workload: bool
    pass_aggregate: bool


class BenchmarkError(Exception):
    pass


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def collect_checksums(root: Path) -> dict[str, str]:
    result: dict[str, str] = {}
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        result[str(path.relative_to(root))] = sha256_file(path)
    return result


def ensure_release_binary(worktree: Path | None) -> Path:
    base = worktree if worktree is not None else Path.cwd()
    binary = base / "target" / "release" / "flacx"
    if not binary.exists():
        raise BenchmarkError(f"missing release binary: {binary}")
    return binary


def run_command(cmd: list[str], *, env: dict[str, str], cwd: Path) -> tuple[float, subprocess.CompletedProcess[str]]:
    start = time.perf_counter()
    proc = subprocess.run(cmd, cwd=cwd, env=env, text=True, capture_output=True)
    elapsed = time.perf_counter() - start
    return elapsed, proc


def parse_trace_file(path: Path) -> float:
    if not path.exists():
        return 0.0
    total = 0.0
    for line in path.read_text().splitlines():
        fields = dict(field.split("=", 1) for field in line.split("\t") if "=" in field)
        if fields.get("event") == "file_finish":
            total += float(fields["elapsed_seconds"])
    return total


def run_workload(binary: Path, repo_root: Path, args: list[str], output_target: Path, trace_path: Path) -> RunResult:
    env = os.environ.copy()
    env[TRACE_ENV] = str(trace_path)
    env.pop("FLACX_REQUIRE_INTERACTIVE", None)
    elapsed, proc = run_command([str(binary), *args], env=env, cwd=repo_root)
    file_finish = parse_trace_file(trace_path)
    return RunResult(
        wall_seconds=elapsed,
        file_finish_seconds=file_finish,
        planning_overhead_seconds=max(0.0, elapsed - file_finish),
        returncode=proc.returncode,
        stdout=proc.stdout,
        stderr=proc.stderr,
        trace_path=str(trace_path),
        output_path=str(output_target),
    )


def median(values: list[float]) -> float:
    ordered = sorted(values)
    n = len(ordered)
    mid = n // 2
    if n % 2:
        return ordered[mid]
    return (ordered[mid - 1] + ordered[mid]) / 2.0


def flac_path_for(wav_path: Path, corpus_root: Path, flac_root: Path) -> Path:
    return flac_root / wav_path.relative_to(corpus_root).with_suffix(".flac")


def generate_canonical_flac_corpus(baseline_binary: Path, corpus_root: Path, flac_root: Path) -> None:
    if flac_root.exists():
        shutil.rmtree(flac_root)
    flac_root.mkdir(parents=True)
    cmd = [str(baseline_binary), "encode", str(corpus_root), "-o", str(flac_root), "--depth", "0"]
    elapsed, proc = run_command(cmd, env={k: v for k, v in os.environ.items() if k != TRACE_ENV}, cwd=Path.cwd())
    if proc.returncode != 0:
        raise BenchmarkError(f"failed to generate canonical FLAC corpus in {elapsed:.3f}s\n{proc.stderr}")


def write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)


def extract_test_names(api_test: Path) -> list[str]:
    names: list[str] = []
    for line in api_test.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("fn ") and stripped.endswith("() {"):
            names.append(stripped[3:-4])
    return names


def export_api_report(repo_root: Path, out_path: Path) -> None:
    lib_rs = repo_root / "crates" / "flacx" / "src" / "lib.rs"
    api_test = repo_root / "crates" / "flacx" / "tests" / "api.rs"
    lib_lines = lib_rs.read_text().splitlines()
    test_names = extract_test_names(api_test)
    architecture_sentinels = [
        name
        for name in test_names
        if any(
            marker in name
            for marker in (
                "reader_session_flow",
                "decode_api_accepts",
                "recompress",
                "aiff",
                "caf",
                "builtin_convenience_no_longer_uses_legacy_helpers",
            )
        )
    ]
    family_exports = [
        stripped
        for line in lib_lines
        if (stripped := line.strip()).startswith("pub use ")
        and any(token in stripped for token in ("WavReader", "AiffReader", "CafReader"))
    ]
    lines = [
        "# Public API Export Audit",
        "",
        f"- lib.rs: `{lib_rs.relative_to(repo_root)}`",
        f"- api sentinel: `{api_test.relative_to(repo_root)}`",
        "",
        "## Public re-exports in lib.rs",
        "",
    ]
    for line in lib_lines:
        stripped = line.strip()
        if stripped.startswith("pub use ") or stripped.startswith("pub mod "):
            lines.append(f"- `{stripped}`")
    lines.extend([
        "",
        "## Architecture invariants tied to the v0.8.2 compare",
        "",
        "- Encode/decode stay primary when the public root still exposes the explicit reader/session flow (`read_pcm_reader`, `FlacReader`, `Encoder`, `Decoder`).",
        "- Builtin remains subordinate when it stays isolated behind `pub mod builtin` rather than replacing the explicit core exports.",
        "- Recompress remains an adapter lane when it is reported beside, not above, the encode/decode façade exports.",
        "- WAV, AIFF, and CAF stay peer container families when their family readers remain exported side-by-side:",
        "",
    ])
    for export in family_exports:
        lines.append(f"  - `{export}`")
    lines.extend([
        "",
        "## API sentinel coverage for the architecture contract",
        "",
    ])
    for name in architecture_sentinels:
        lines.append(f"- `{name}`")
    lines.extend([
        "",
        "## API sentinel coverage hints",
        "",
        "- `cargo test -p flacx --test api` is the executable guardrail for current public entrypoints.",
        "- This report is a human-readable audit aid and must be interpreted alongside the API test pass.",
    ])
    write_text(out_path, "\n".join(lines) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline-worktree", required=True)
    parser.add_argument("--corpus", required=True)
    parser.add_argument("--out-dir", required=True)
    parser.add_argument("--runs", type=int, default=DEFAULT_RUNS)
    parser.add_argument("--single-file", default=DEFAULT_SINGLE_FILE)
    args = parser.parse_args()

    repo_root = Path.cwd()
    baseline_worktree = Path(args.baseline_worktree).resolve()
    corpus_root = Path(args.corpus).resolve()
    out_dir = Path(args.out_dir).resolve()
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "tmp").mkdir(parents=True, exist_ok=True)

    baseline_binary = ensure_release_binary(baseline_worktree)
    head_binary = ensure_release_binary(repo_root)

    single_wav = corpus_root / args.single_file
    if not single_wav.exists():
        raise BenchmarkError(f"single-file corpus input missing: {single_wav}")

    test_wavs_sha = collect_checksums(corpus_root)
    flac_root = out_dir / "generated-flac-corpus"
    generate_canonical_flac_corpus(baseline_binary, corpus_root, flac_root)
    generated_sha = collect_checksums(flac_root)

    layout_lines = [str(path.relative_to(flac_root)) for path in sorted(p for p in flac_root.rglob("*") if p.is_file())]
    write_text(out_dir / "generated-layout.md", "# Generated FLAC Corpus Layout\n\n" + "\n".join(f"- `{line}`" for line in layout_lines) + "\n")
    write_text(out_dir / "generated-flac-corpus.sha256", "".join(f"{digest}  {name}\n" for name, digest in generated_sha.items()))
    write_text(out_dir / "test-wavs.sha256", "".join(f"{digest}  {name}\n" for name, digest in test_wavs_sha.items()))
    export_api_report(repo_root, out_dir / "public-api-exports.md")

    single_flac = flac_path_for(single_wav, corpus_root, flac_root)
    workloads: list[tuple[str, str, str, list[str], list[str]]] = [
        ("encode_single", "encode", str(single_wav), ["encode", str(single_wav), "-o"], ["single.flac"]),
        ("encode_directory", "encode", str(corpus_root), ["encode", str(corpus_root), "-o", "--depth", "0"], ["dir_out"]),
        ("decode_single", "decode", str(single_flac), ["decode", str(single_flac), "-o"], ["single.wav"]),
        ("decode_directory", "decode", str(flac_root), ["decode", str(flac_root), "-o", "--depth", "0"], ["dir_out"]),
        ("recompress_single", "recompress", str(single_flac), ["recompress", str(single_flac), "-o"], ["single.flac"]),
        ("recompress_directory", "recompress", str(flac_root), ["recompress", str(flac_root), "-o", "--depth", "0"], ["dir_out"]),
    ]

    results: list[WorkloadResult] = []
    for name, mode, source, prefix_args, output_names in workloads:
        baseline_runs: list[RunResult] = []
        head_runs: list[RunResult] = []
        for run_index in range(args.runs):
            for label, binary, sink in (("baseline", baseline_binary, baseline_runs), ("head", head_binary, head_runs)):
                run_root = Path(tempfile.mkdtemp(prefix=f"flacx-{name}-{label}-{run_index}-", dir=str(out_dir / "tmp")))
                if output_names == ["dir_out"]:
                    output_path = run_root / "out"
                else:
                    output_path = run_root / output_names[0]
                trace_path = run_root / "progress.trace"
                cmd = prefix_args.copy()
                if mode in {"encode", "decode", "recompress"} and "-o" in cmd:
                    insert_at = cmd.index("-o") + 1
                    cmd.insert(insert_at, str(output_path))
                result = run_workload(binary, repo_root if binary == head_binary else baseline_worktree, cmd, output_path, trace_path)
                if result.returncode != 0:
                    raise BenchmarkError(
                        f"workload {name} failed for {label} run {run_index + 1}\ncmd: {' '.join(cmd)}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
                    )
                sink.append(result)
                shutil.rmtree(run_root)
        baseline_wall = median([r.wall_seconds for r in baseline_runs])
        head_wall = median([r.wall_seconds for r in head_runs])
        baseline_file = median([r.file_finish_seconds for r in baseline_runs])
        head_file = median([r.file_finish_seconds for r in head_runs])
        baseline_over = median([r.planning_overhead_seconds for r in baseline_runs])
        head_over = median([r.planning_overhead_seconds for r in head_runs])
        results.append(
            WorkloadResult(
                name=name,
                mode=mode,
                source=source,
                baseline_runs=baseline_runs,
                head_runs=head_runs,
                baseline_median_wall_seconds=baseline_wall,
                head_median_wall_seconds=head_wall,
                ratio_head_over_baseline=head_wall / baseline_wall if baseline_wall else math.inf,
                baseline_median_file_finish_seconds=baseline_file,
                head_median_file_finish_seconds=head_file,
                baseline_median_planning_overhead_seconds=baseline_over,
                head_median_planning_overhead_seconds=head_over,
            )
        )

    geometric_mean_ratio = math.exp(sum(math.log(w.ratio_head_over_baseline) for w in results) / len(results))
    report = CompareReport(
        corpus=str(corpus_root),
        baseline_worktree=str(baseline_worktree),
        head_binary=str(head_binary),
        baseline_binary=str(baseline_binary),
        generated_flac_root=str(flac_root),
        runs=args.runs,
        single_file=args.single_file,
        generated_layout=layout_lines,
        test_wavs_sha256=test_wavs_sha,
        generated_flac_sha256=generated_sha,
        workloads=results,
        geometric_mean_ratio=geometric_mean_ratio,
        per_workload_gate=1.05,
        aggregate_gate=1.03,
        pass_per_workload=all(w.ratio_head_over_baseline <= 1.05 for w in results),
        pass_aggregate=geometric_mean_ratio <= 1.03,
    )
    json_path = out_dir / "v0.8.2-vs-head.json"
    json_path.write_text(json.dumps(asdict(report), indent=2))

    md_lines = [
        "# CLI Perf Compare — HEAD vs v0.8.2",
        "",
        f"- Corpus: `{corpus_root}`",
        f"- Baseline worktree: `{baseline_worktree}`",
        f"- Runs per workload: {args.runs}",
        f"- Single-file representative: `{args.single_file}`",
        f"- Canonical generated FLAC corpus: `{flac_root}` (created once, then treated read-only)",
        "",
        "## Gate summary",
        "",
        f"- Per-workload gate <= 1.05x baseline: {'PASS' if report.pass_per_workload else 'FAIL'}",
        f"- Aggregate geomean gate <= 1.03x baseline: {'PASS' if report.pass_aggregate else 'FAIL'}",
        f"- Aggregate geomean ratio: {geometric_mean_ratio:.4f}",
        "",
        "## Workloads",
        "",
        "| Workload | Baseline median (s) | HEAD median (s) | Ratio | Baseline file-finish (s) | HEAD file-finish (s) | Baseline planning overhead (s) | HEAD planning overhead (s) |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    for workload in results:
        md_lines.append(
            f"| {workload.name} | {workload.baseline_median_wall_seconds:.3f} | {workload.head_median_wall_seconds:.3f} | {workload.ratio_head_over_baseline:.3f} | {workload.baseline_median_file_finish_seconds:.3f} | {workload.head_median_file_finish_seconds:.3f} | {workload.baseline_median_planning_overhead_seconds:.3f} | {workload.head_median_planning_overhead_seconds:.3f} |"
        )
    md_lines.extend([
        "",
        "## Notes",
        "",
        "- Planning overhead is approximated as total wall-clock minus summed `file_finish` trace time from `FLACX_PROGRESS_TRACE`.",
        "- Interactive/progress-cost validation is not part of this report and should be added only if progress behavior changes.",
        "- Library public-surface preservation still requires `cargo test -p flacx --test api` alongside `public-api-exports.md`.",
    ])
    write_text(out_dir / "v0.8.2-vs-head.md", "\n".join(md_lines) + "\n")
    print(json_path)
    print(out_dir / "v0.8.2-vs-head.md")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
