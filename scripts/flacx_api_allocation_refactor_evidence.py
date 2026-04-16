#!/usr/bin/env python3
from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Iterable


DEFAULT_ARTIFACT_ROOT = ".omx/reports/perf/flacx-api-allocation-refactor"
DEFAULT_BASELINE_WORKTREE = ".omx/worktrees/flacx-api-allocation-refactor-baseline"
BENCHMARK_AUTHORITY_COMMAND = "cargo bench -p flacx --bench throughput -- --noplot"
BENCHMARK_GATE_RATIO = 1.05
ROUNDTRIP_BENCHMARK_ID = "test_wavs_roundtrip_throughput"

BENCHMARK_GROUPS: dict[str, tuple[str, ...]] = {
    "corpus_throughput": (
        "encode_corpus_throughput",
        "decode_corpus_throughput",
        "recompress_corpus_throughput",
    ),
    "session_orchestration": (
        "builtin_bytes_encode",
        "builtin_bytes_decode",
        "builtin_bytes_recompress",
    ),
    "metadata_write": ("metadata_write_path",),
    "decode_frame_materialization": ("decode_frame_materialization",),
    "roundtrip": (ROUNDTRIP_BENCHMARK_ID,),
}

BENCHMARK_FILE_RULES: tuple[tuple[tuple[str, ...], tuple[str, ...]], ...] = (
    (
        (
            "crates/flacx/src/convenience.rs",
            "crates/flacx/src/encoder.rs",
            "crates/flacx/src/decode.rs",
            "crates/flacx/src/recompress/source.rs",
            "crates/flacx/src/recompress/session.rs",
        ),
        BENCHMARK_GROUPS["session_orchestration"],
    ),
    (
        (
            "crates/flacx/src/metadata.rs",
            "crates/flacx/src/metadata/blocks.rs",
            "crates/flacx/src/wav_output.rs",
            "crates/flacx/src/aiff_output.rs",
            "crates/flacx/src/caf_output.rs",
            "crates/flacx/src/recompress/source.rs",
        ),
        BENCHMARK_GROUPS["metadata_write"],
    ),
    (
        (
            "crates/flacx/src/read/mod.rs",
            "crates/flacx/src/read/frame.rs",
            "crates/flacx/src/decode_output.rs",
            "crates/flacx/src/encode_pipeline.rs",
            "crates/flacx/src/input.rs",
        ),
        BENCHMARK_GROUPS["decode_frame_materialization"],
    ),
)

PUBLIC_HOTSPOTS: tuple[tuple[str, str], ...] = (
    ("crates/flacx/src/input.rs", "EncodePcmStream"),
    ("crates/flacx/src/read/mod.rs", "DecodePcmStream"),
    ("crates/flacx/src/recompress/source.rs", "FlacRecompressSource"),
)


@dataclass
class BenchmarkRecord:
    benchmark_id: str
    group: str
    baseline_median_ns: float | None
    head_median_ns: float | None
    ratio_head_over_baseline: float | None
    gate: float
    status: str
    notes: list[str]


@dataclass
class RoundTripRecord:
    input_filename: str
    baseline_output_path: str | None
    head_output_path: str | None
    baseline_sha256: str | None
    head_sha256: str | None
    byte_equal: bool | None
    baseline_bytes: int | None
    head_bytes: int | None
    baseline_wall_seconds: float | None
    head_wall_seconds: float | None


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


def run_command(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        text=True,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise EvidenceError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def locate_binding_corpus(repo_root: Path) -> Path | None:
    candidates = [repo_root / "test-wavs"]
    candidates.extend(parent / "test-wavs" for parent in repo_root.parents)
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


def relative_repo_files(root: Path, glob: str) -> dict[str, Path]:
    return {path.relative_to(root).as_posix(): path for path in root.glob(glob) if path.is_file()}


def file_bytes_or_none(path: Path) -> bytes | None:
    return path.read_bytes() if path.exists() else None


def discover_changed_files(baseline_root: Path, head_root: Path) -> list[str]:
    baseline_files = relative_repo_files(baseline_root, "crates/flacx/**/*.rs")
    head_files = relative_repo_files(head_root, "crates/flacx/**/*.rs")
    changed: set[str] = set()
    for rel in sorted(set(baseline_files) | set(head_files)):
        if file_bytes_or_none(baseline_files.get(rel, Path("__missing__"))) != file_bytes_or_none(
            head_files.get(rel, Path("__missing__"))
        ):
            changed.add(rel)
    return sorted(changed)


def map_changed_files_to_benchmarks(changed_files: Iterable[str]) -> dict[str, list[str]]:
    mapping: dict[str, set[str]] = {}
    changed_set = set(changed_files)
    for files, benchmark_ids in BENCHMARK_FILE_RULES:
        if changed_set.intersection(files):
            for benchmark_id in benchmark_ids:
                mapping.setdefault(benchmark_id, set()).update(
                    sorted(changed_set.intersection(files))
                )

    if changed_set:
        for benchmark_id in (*BENCHMARK_GROUPS["corpus_throughput"], ROUNDTRIP_BENCHMARK_ID):
            mapping.setdefault(benchmark_id, set()).update(changed_set)

    return {benchmark_id: sorted(paths) for benchmark_id, paths in sorted(mapping.items())}


def benchmark_candidates(criterion_root: Path, benchmark_id: str) -> list[Path]:
    candidates = [
        *criterion_root.glob(f"**/{benchmark_id}/new/estimates.json"),
        *criterion_root.glob(f"**/{benchmark_id}/base/estimates.json"),
    ]
    return sorted({path.resolve() for path in candidates})


def load_estimate_ns(path: Path) -> float:
    payload = json.loads(path.read_text())
    for key in ("median", "mean", "slope"):
        section = payload.get(key)
        if isinstance(section, dict) and isinstance(section.get("point_estimate"), (int, float)):
            return float(section["point_estimate"])
    raise EvidenceError(f"no usable estimate in {path}")


def load_benchmark_estimate(criterion_root: Path, benchmark_id: str) -> tuple[float | None, list[str]]:
    if not criterion_root.exists():
        return None, [f"criterion root missing: {criterion_root}"]
    candidates = benchmark_candidates(criterion_root, benchmark_id)
    if not candidates:
        return None, [f"missing criterion estimate for {benchmark_id} under {criterion_root}"]
    estimate = load_estimate_ns(candidates[0])
    notes = [f"estimate source: {candidates[0]}"]
    if len(candidates) > 1:
        notes.append(f"multiple matches found; using first of {len(candidates)} candidates")
    return estimate, notes


def collect_benchmark_records(
    baseline_criterion_root: Path,
    head_criterion_root: Path,
) -> list[BenchmarkRecord]:
    records: list[BenchmarkRecord] = []
    for group, benchmark_ids in BENCHMARK_GROUPS.items():
        for benchmark_id in benchmark_ids:
            baseline_ns, baseline_notes = load_benchmark_estimate(
                baseline_criterion_root, benchmark_id
            )
            head_ns, head_notes = load_benchmark_estimate(head_criterion_root, benchmark_id)
            notes = [*baseline_notes, *head_notes]
            ratio = (
                head_ns / baseline_ns
                if baseline_ns is not None and head_ns is not None and baseline_ns != 0
                else None
            )
            status = "PENDING"
            if ratio is not None:
                status = "PASS" if ratio <= BENCHMARK_GATE_RATIO else "FAIL"
            elif baseline_ns is not None or head_ns is not None:
                status = "INCOMPLETE"
            records.append(
                BenchmarkRecord(
                    benchmark_id=benchmark_id,
                    group=group,
                    baseline_median_ns=baseline_ns,
                    head_median_ns=head_ns,
                    ratio_head_over_baseline=ratio,
                    gate=BENCHMARK_GATE_RATIO,
                    status=status,
                    notes=notes,
                )
            )
    return records


def format_ns(ns: float | None) -> str:
    if ns is None:
        return "_missing_"
    return f"{ns:.0f} ns"


def format_ratio(ratio: float | None) -> str:
    if ratio is None:
        return "_pending_"
    return f"{ratio:.4f}x"


def write_benchmark_summary(
    out_dir: Path,
    baseline_criterion_root: Path,
    head_criterion_root: Path,
    changed_file_map: dict[str, list[str]],
    records: list[BenchmarkRecord],
) -> None:
    payload = {
        "baseline_criterion_root": str(baseline_criterion_root),
        "head_criterion_root": str(head_criterion_root),
        "gate_ratio_head_over_baseline": BENCHMARK_GATE_RATIO,
        "records": [asdict(record) for record in records],
        "changed_file_map": changed_file_map,
    }
    write_text(out_dir / "bench-summary.json", json.dumps(payload, indent=2) + "\n")

    lines = [
        "# flacx API allocation refactor benchmark summary",
        "",
        f"- Baseline criterion root: `{baseline_criterion_root}`",
        f"- Head criterion root: `{head_criterion_root}`",
        f"- Slowdown gate: `head / baseline <= {BENCHMARK_GATE_RATIO:.2f}x`",
        "",
        "> Note: the summary reads Criterion estimate artifacts (`median`, with `mean`/`slope` fallback) and treats the head-to-baseline time ratio as the binding slowdown gate.",
        "",
        "## Changed-file benchmark map",
        "",
    ]
    if not changed_file_map:
        lines.append("- No changed `crates/flacx` files were detected between the baseline worktree and head worktree.")
    else:
        for benchmark_id, paths in changed_file_map.items():
            lines.append(f"- `{benchmark_id}`")
            for path in paths:
                lines.append(f"  - `{path}`")

    lines.extend(["", "## Benchmark results", "", "| Benchmark ID | Group | Baseline | Head | Ratio | Status |", "| --- | --- | --- | --- | --- | --- |"])
    for record in records:
        lines.append(
            "| "
            + " | ".join(
                [
                    f"`{record.benchmark_id}`",
                    record.group,
                    format_ns(record.baseline_median_ns),
                    format_ns(record.head_median_ns),
                    format_ratio(record.ratio_head_over_baseline),
                    record.status,
                ]
            )
            + " |"
        )
        for note in record.notes:
            lines.append(f"|  |  |  |  |  | note: {note} |")

    write_text(out_dir / "bench-summary.md", "\n".join(lines) + "\n")


def parse_public_items(root: Path) -> dict[str, list[str]]:
    pattern = re.compile(r"^\s*pub\s+(?:struct|enum|trait)\s+([A-Za-z0-9_]+)")
    items: dict[str, list[str]] = {}
    for rel, path in relative_repo_files(root, "crates/flacx/src/**/*.rs").items():
        for line_number, line in enumerate(path.read_text().splitlines(), start=1):
            match = pattern.match(line)
            if match:
                items.setdefault(match.group(1), []).append(f"{rel}:{line_number}")
    return items


def extract_symbol_block(path: Path, symbol: str) -> str | None:
    if not path.exists():
        return None
    lines = path.read_text().splitlines()
    start = None
    for index, line in enumerate(lines):
        if re.search(rf"\b{re.escape(symbol)}\b", line):
            start = index
            break
    if start is None:
        return None

    brace_depth = 0
    started_block = False
    collected: list[str] = []
    for line in lines[start:]:
        collected.append(line)
        brace_depth += line.count("{")
        brace_depth -= line.count("}")
        if "{" in line:
            started_block = True
        if started_block and brace_depth <= 0:
            break
    return "\n".join(collected).strip() + "\n"


def write_public_surface_check(
    baseline_root: Path,
    head_root: Path,
    out_dir: Path,
) -> None:
    baseline_items = parse_public_items(baseline_root)
    head_items = parse_public_items(head_root)

    added = sorted(set(head_items) - set(baseline_items))
    removed = sorted(set(baseline_items) - set(head_items))

    lines = [
        "# flacx API allocation refactor public-surface check",
        "",
        f"- Baseline worktree: `{baseline_root}`",
        f"- Head worktree: `{head_root}`",
        "",
        "## Public item delta",
        "",
        f"- Added public items: {len(added)}",
        f"- Removed public items: {len(removed)}",
        "",
    ]
    if added:
        lines.append("### Added public items")
        for name in added:
            for location in head_items[name]:
                lines.append(f"- `{name}` at `{location}`")
        lines.append("")
    if removed:
        lines.append("### Removed public items")
        for name in removed:
            for location in baseline_items[name]:
                lines.append(f"- `{name}` at `{location}`")
        lines.append("")
    if not added and not removed:
        lines.append("- No added or removed public items detected.")
        lines.append("")

    lines.extend(["## Approval-boundary hotspots", ""])
    for rel_path, symbol in PUBLIC_HOTSPOTS:
        baseline_block = extract_symbol_block(baseline_root / rel_path, symbol)
        head_block = extract_symbol_block(head_root / rel_path, symbol)
        status = "PASS" if baseline_block == head_block else "REVIEW"
        baseline_sha = (
            hashlib.sha256(baseline_block.encode()).hexdigest() if baseline_block is not None else None
        )
        head_sha = hashlib.sha256(head_block.encode()).hexdigest() if head_block is not None else None
        lines.extend(
            [
                f"### `{symbol}` ({status})",
                f"- File: `{rel_path}`",
                f"- Baseline block sha256: `{baseline_sha or 'missing'}`",
                f"- Head block sha256: `{head_sha or 'missing'}`",
            ]
        )
        if baseline_block != head_block:
            lines.append("- The extracted public contract block changed; review against the approval gate before merging.")
        else:
            lines.append("- The extracted public contract block is unchanged.")
        lines.append("")

    write_text(out_dir / "public-surface-check.md", "\n".join(lines) + "\n")


def rust_string_literal(path: Path) -> str:
    return json.dumps(path.as_posix())


def make_roundtrip_runner_source() -> str:
    return textwrap.dedent(
        """
        use std::env;
        use std::path::PathBuf;
        use std::time::Instant;

        use flacx::builtin;

        fn main() -> Result<(), Box<dyn std::error::Error>> {{
            let mut args = env::args().skip(1);
            let corpus_root = PathBuf::from(args.next().expect("corpus root"));
            let out_root = PathBuf::from(args.next().expect("output root"));
            let mut files: Vec<PathBuf> = args.map(PathBuf::from).collect();
            files.sort();

            for input in files {{
                let rel = input.strip_prefix(&corpus_root)?.to_path_buf();
                let stem = rel
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or("invalid file stem")?
                    .to_string();
                let rel_parent = rel.parent().map(PathBuf::from).unwrap_or_default();
                let stage_root = out_root.join(&rel_parent).join(&stem);
                std::fs::create_dir_all(&stage_root)?;

                let encoded = stage_root.join("encoded.flac");
                let decoded = stage_root.join("decoded.wav");
                let reencoded = stage_root.join("reencoded.flac");

                let start = Instant::now();
                builtin::encode_file(&input, &encoded)?;
                builtin::decode_file(&encoded, &decoded)?;
                builtin::encode_file(&decoded, &reencoded)?;
                let elapsed = start.elapsed().as_secs_f64();

                println!("{}\\t{}\\t{}", rel.display(), reencoded.display(), elapsed);
            }}

            Ok(())
        }}
        """
    ).strip() + "\n"


def run_roundtrip_for_repo(repo_root: Path, corpus_root: Path, out_root: Path) -> dict[str, tuple[Path, float]]:
    if not corpus_root.exists():
        raise EvidenceError(f"test-wavs corpus missing: {corpus_root}")

    files = sorted(path for path in corpus_root.rglob("*") if path.is_file())
    if not files:
        raise EvidenceError(f"test-wavs corpus is empty: {corpus_root}")

    with tempfile.TemporaryDirectory(prefix="flacx-allocation-roundtrip-") as temp_dir:
        temp_root = Path(temp_dir)
        cargo_toml = textwrap.dedent(
            f"""
            [package]
            name = "flacx-allocation-roundtrip-runner"
            version = "0.1.0"
            edition = "2021"

            [dependencies]
            flacx = {{ path = {rust_string_literal(repo_root / "crates" / "flacx")} }}
            """
        ).strip() + "\n"
        write_text(temp_root / "Cargo.toml", cargo_toml)
        write_text(temp_root / "src/main.rs", make_roundtrip_runner_source())

        proc = run_command(
            [
                "cargo",
                "run",
                "--quiet",
                "--release",
                "--manifest-path",
                str(temp_root / "Cargo.toml"),
                "--",
                str(corpus_root),
                str(out_root),
                *[str(path) for path in files],
            ],
            cwd=repo_root,
        )

    records: dict[str, tuple[Path, float]] = {}
    for line in proc.stdout.splitlines():
        rel, output_path, elapsed = line.split("\t")
        records[rel] = (Path(output_path), float(elapsed))
    return records


def collect_roundtrip_records(
    baseline_root: Path,
    head_root: Path,
    corpus_root: Path | None,
    out_dir: Path,
) -> list[RoundTripRecord]:
    if corpus_root is None:
        write_text(
            out_dir / "test-wavs-roundtrip.json",
            json.dumps(
                {
                    "status": "PENDING",
                    "reason": "test-wavs corpus not found",
                },
                indent=2,
            )
            + "\n",
        )
        return []

    baseline_outputs = run_roundtrip_for_repo(
        baseline_root, corpus_root, out_dir / "baseline-roundtrip"
    )
    head_outputs = run_roundtrip_for_repo(head_root, corpus_root, out_dir / "head-roundtrip")

    records: list[RoundTripRecord] = []
    for rel in sorted(set(baseline_outputs) | set(head_outputs)):
        baseline_output, baseline_elapsed = baseline_outputs.get(rel, (None, None))
        head_output, head_elapsed = head_outputs.get(rel, (None, None))
        baseline_sha = sha256_file(baseline_output) if baseline_output is not None else None
        head_sha = sha256_file(head_output) if head_output is not None else None
        baseline_bytes = baseline_output.stat().st_size if baseline_output is not None else None
        head_bytes = head_output.stat().st_size if head_output is not None else None
        records.append(
            RoundTripRecord(
                input_filename=rel,
                baseline_output_path=str(baseline_output) if baseline_output is not None else None,
                head_output_path=str(head_output) if head_output is not None else None,
                baseline_sha256=baseline_sha,
                head_sha256=head_sha,
                byte_equal=(
                    baseline_sha == head_sha
                    if baseline_sha is not None and head_sha is not None
                    else None
                ),
                baseline_bytes=baseline_bytes,
                head_bytes=head_bytes,
                baseline_wall_seconds=baseline_elapsed,
                head_wall_seconds=head_elapsed,
            )
        )

    payload = {
        "status": "COMPLETE",
        "baseline_worktree": str(baseline_root),
        "head_worktree": str(head_root),
        "corpus_root": str(corpus_root),
        "records": [asdict(record) for record in records],
        "all_byte_equal": all(record.byte_equal for record in records),
    }
    write_text(out_dir / "test-wavs-roundtrip.json", json.dumps(payload, indent=2) + "\n")
    return records


def write_roundtrip_markdown(out_dir: Path, records: list[RoundTripRecord], corpus_root: Path | None) -> None:
    lines = [
        "# flacx API allocation refactor test-wavs roundtrip report",
        "",
        f"- Corpus root: `{corpus_root}`" if corpus_root is not None else "- Corpus root: _missing_",
        "",
    ]
    if not records:
        lines.extend(
            [
                "No roundtrip records were generated.",
                "",
                "- If `./test-wavs` is intentionally external to the worktree, rerun this script with a baseline worktree that can also resolve the shared corpus path.",
            ]
        )
        write_text(out_dir / "test-wavs-roundtrip.md", "\n".join(lines) + "\n")
        return

    lines.extend(
        [
            "| Input | Byte equal | Baseline bytes | Head bytes | Baseline SHA-256 | Head SHA-256 | Baseline wall (s) | Head wall (s) |",
            "| --- | --- | --- | --- | --- | --- | --- | --- |",
        ]
    )
    for record in records:
        lines.append(
            "| "
            + " | ".join(
                [
                    f"`{record.input_filename}`",
                    str(record.byte_equal),
                    str(record.baseline_bytes),
                    str(record.head_bytes),
                    f"`{record.baseline_sha256}`" if record.baseline_sha256 else "_missing_",
                    f"`{record.head_sha256}`" if record.head_sha256 else "_missing_",
                    f"{record.baseline_wall_seconds:.4f}" if record.baseline_wall_seconds is not None else "_missing_",
                    f"{record.head_wall_seconds:.4f}" if record.head_wall_seconds is not None else "_missing_",
                ]
            )
            + " |"
        )
    write_text(out_dir / "test-wavs-roundtrip.md", "\n".join(lines) + "\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate final verification and report-synthesis artifacts for the flacx API allocation refactor lane."
    )
    parser.add_argument(
        "--baseline-worktree",
        required=True,
        help=f"Path to the pinned baseline worktree (expected default: {DEFAULT_BASELINE_WORKTREE}).",
    )
    parser.add_argument("--out-dir", default=DEFAULT_ARTIFACT_ROOT)
    parser.add_argument("--baseline-criterion-root")
    parser.add_argument("--head-criterion-root")
    parser.add_argument(
        "--skip-roundtrip",
        action="store_true",
        help="Skip the `test-wavs` encode -> decode -> re-encode compare lane.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    head_root = Path.cwd().resolve()
    baseline_root = Path(args.baseline_worktree).resolve()
    out_dir = Path(args.out_dir).resolve()

    if not baseline_root.exists():
        raise EvidenceError(f"baseline worktree missing: {baseline_root}")

    baseline_criterion_root = (
        Path(args.baseline_criterion_root).resolve()
        if args.baseline_criterion_root
        else baseline_root / "target" / "criterion"
    )
    head_criterion_root = (
        Path(args.head_criterion_root).resolve()
        if args.head_criterion_root
        else head_root / "target" / "criterion"
    )

    changed_files = discover_changed_files(baseline_root, head_root)
    changed_file_map = map_changed_files_to_benchmarks(changed_files)
    records = collect_benchmark_records(baseline_criterion_root, head_criterion_root)

    write_benchmark_summary(
        out_dir,
        baseline_criterion_root,
        head_criterion_root,
        changed_file_map,
        records,
    )
    write_public_surface_check(baseline_root, head_root, out_dir)

    corpus_root = locate_binding_corpus(head_root)
    roundtrip_records: list[RoundTripRecord] = []
    if args.skip_roundtrip:
        write_text(
            out_dir / "test-wavs-roundtrip.json",
            json.dumps(
                {
                    "status": "SKIPPED",
                    "reason": "--skip-roundtrip",
                    "corpus_root": str(corpus_root) if corpus_root is not None else None,
                },
                indent=2,
            )
            + "\n",
        )
    else:
        roundtrip_records = collect_roundtrip_records(
            baseline_root, head_root, corpus_root, out_dir
        )
    write_roundtrip_markdown(out_dir, roundtrip_records, corpus_root)

    print(out_dir / "bench-summary.md")
    print(out_dir / "bench-summary.json")
    print(out_dir / "public-surface-check.md")
    print(out_dir / "test-wavs-roundtrip.md")
    print(out_dir / "test-wavs-roundtrip.json")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except EvidenceError as exc:
        print(f"error: {exc}", file=sys.stderr)
        raise SystemExit(1)
