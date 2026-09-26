#!/usr/bin/env python3
"""Run paired Phase 2 comparisons and alternating standalone benchmarks."""

from __future__ import annotations

import argparse
import json
import math
import re
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
CORPUS_PATH = ROOT / "server/fixtures/routing/phase2-benchmark.json"
K_VALUES = (20, 50, 100)
STRATEGIES = ("union", "fixed", "compare")


def run_route(
    binary: Path,
    network: Path,
    case: dict[str, Any],
    k: int,
    strategy: str,
    departure_at: str,
) -> dict[str, Any]:
    origin = case["from"]
    destination = case["to"]
    command = [
        str(binary),
        "--network",
        str(network),
        "--from-lat",
        str(origin["latitude"]),
        "--from-lon",
        str(origin["longitude"]),
        "--to-lat",
        str(destination["latitude"]),
        "--to-lon",
        str(destination["longitude"]),
        "--depart-at",
        departure_at,
        "--modes",
        ",".join(case["modes"]),
        "--service-conditions",
        "fixture",
        "--phase2-strategy",
        strategy,
        "--max-static-corridors",
        str(k),
        "--trace",
        "--json",
    ]
    started = time.perf_counter_ns()
    process = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    if process.returncode:
        raise RuntimeError(
            f"via-route failed for {case['id']} K={k} {strategy}: "
            f"{(process.stdout + process.stderr)[-2000:]}"
        )
    diagnostics = process.stdout + "\n" + process.stderr
    options = parse_option_count(process.stdout)
    if options is None:
        raise RuntimeError(f"via-route returned no JSON result for {case['id']} K={k} {strategy}")
    return {
        "strategy": strategy,
        "wall_ms": round(elapsed_ms, 3),
        "phase2": parse_phase2(diagnostics, strategy) if strategy != "compare" else None,
        "options": options,
        "diagnostics": diagnostics,
    }


def parse_option_count(stdout: str) -> int | None:
    json_start = stdout.find("\n{\n")
    if json_start >= 0:
        stdout = stdout[json_start + 1 :]
    try:
        result = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    options = result.get("journey_options")
    return len(options) if isinstance(options, list) else 0


def numeric_field(line: str, name: str) -> int | None:
    match = re.search(rf"\b{name}=(\d+)", line)
    return int(match.group(1)) if match else None


def parse_phase2(diagnostics: str, strategy: str) -> dict[str, Any] | None:
    search_name = "fixed corridor evaluation" if strategy == "fixed" else "temporal search"
    line = next((item for item in diagnostics.splitlines() if search_name in item), "")
    if not line:
        if "timing snapshot" not in diagnostics:
            return None
        raise RuntimeError(f"did not find {search_name!r} diagnostics")
    fields = {
        "phase2_latency_us": numeric_field(line, "phase2_latency_us"),
        "departure_lookups": numeric_field(line, "departure_lookups"),
        "final_candidates": numeric_field(line, "final_candidates"),
        "states_evaluated": numeric_field(
            line,
            "corridor_states_evaluated" if strategy == "fixed" else "states_expanded",
        ),
        "effective_trips_examined": numeric_field(
            line, "effective_trips_examined" if strategy == "fixed" else "effective_trips"
        ),
    }
    required = ("phase2_latency_us", "departure_lookups", "final_candidates", "states_evaluated")
    if any(fields[name] is None for name in required):
        raise RuntimeError(f"missing required {search_name} diagnostics: {line}")
    if strategy == "fixed":
        fields["corridors_evaluated"] = numeric_field(line, "corridors_evaluated")
        fields["corridors_feasible"] = numeric_field(line, "corridors_feasible")
    return fields


def parse_comparison(
    diagnostics: str,
) -> tuple[dict[str, Any], str | None, str | None, list[str]]:
    line = next(
        (item for item in diagnostics.splitlines() if "Phase 2 strategy comparison" in item),
        "",
    )
    if not line:
        if "timing snapshot" in diagnostics:
            raise RuntimeError("timing snapshot ran without paired Phase 2 comparison diagnostics")
        return {
            "union_latency_us": None,
            "fixed_latency_us": None,
            "union_candidate_count": 0,
            "fixed_candidate_count": 0,
            "union_departure_lookups": 0,
            "fixed_departure_lookups": 0,
            "union_states_expanded": 0,
            "fixed_states_evaluated": 0,
            "same_best_exact_signature": False,
            "same_best_structural_signature": False,
            "union_best_in_retained_topk_exact": False,
            "union_best_in_retained_topk_structural": False,
            "recombination_used": False,
            "best_arrival_delta_seconds": None,
            "fixed_no_route_union_route": False,
            "union_no_route_fixed_route": False,
            "phase2_executed": False,
        }, None, None, []
    fields: dict[str, Any] = {}
    for name, value in re.findall(
        r"\b([a-z_]+): (true|false|Some\(-?\d+\)|None|\d+)", line
    ):
        if value == "true":
            fields[name] = True
        elif value == "false":
            fields[name] = False
        elif value.startswith("Some("):
            fields[name] = int(value[5:-1])
        elif value == "None":
            fields[name] = None
        else:
            fields[name] = int(value)
    required = {
        "union_latency_us",
        "fixed_latency_us",
        "union_candidate_count",
        "fixed_candidate_count",
        "same_best_exact_signature",
        "same_best_structural_signature",
        "recombination_used",
        "best_arrival_delta_seconds",
    }
    missing = required - fields.keys()
    if missing:
        raise RuntimeError(f"missing comparison fields {sorted(missing)}: {line}")
    fields["phase2_executed"] = True
    union_best = debug_summary(line, "union_best")
    fixed_best = debug_summary(line, "fixed_best")
    retained = [
        item.split("DEBUG via_server::routing: retained static corridor ", 1)[1]
        for item in diagnostics.splitlines()
        if "DEBUG via_server::routing: retained static corridor corridor_rank=" in item
    ]
    return fields, union_best, fixed_best, retained


def debug_summary(line: str, name: str) -> str | None:
    match = re.search(rf'{name}=Some\("((?:\\.|[^"\\])*)"\)', line)
    if not match:
        return None
    return json.loads('"' + match.group(1) + '"')


def percentile(values: list[float | int], fraction: float) -> float | int | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(fraction * len(ordered)) - 1)]


def describe(values: list[float | int]) -> dict[str, float | int | None]:
    return {
        "count": len(values),
        "mean": statistics.fmean(values) if values else None,
        "median": statistics.median(values) if values else None,
        "p95": percentile(values, 0.95),
        "min": min(values) if values else None,
        "max": max(values) if values else None,
    }


def median_field(rows: list[dict[str, Any]], strategy: str, field: str) -> float | int | None:
    values = [
        row["phase2"][field]
        for row in rows
        if row["strategy"] == strategy
        and row["phase2"] is not None
        and row["phase2"].get(field) is not None
    ]
    return statistics.median(values) if values else None


def aggregate(paired: list[dict[str, Any]], standalone: list[dict[str, Any]]) -> dict[str, Any]:
    by_k: dict[str, Any] = {}
    for k in K_VALUES:
        pairs = [row for row in paired if row["k"] == k]
        single = [row for row in standalone if row["k"] == k]
        paired_count = len(pairs)
        both_successes = sum(
            row["comparison"]["fixed_candidate_count"] > 0
            and row["comparison"]["union_candidate_count"] > 0
            for row in pairs
        )
        both_no_route = sum(
            row["comparison"]["fixed_candidate_count"] == 0
            and row["comparison"]["union_candidate_count"] == 0
            for row in pairs
        )
        both_routes = [
            row["comparison"]["best_arrival_delta_seconds"]
            for row in pairs
            if row["comparison"]["best_arrival_delta_seconds"] is not None
        ]
        gains = [
            -row["comparison"]["best_arrival_delta_seconds"]
            for row in pairs
            if row["comparison"]["recombination_used"]
            and row["comparison"]["best_arrival_delta_seconds"] is not None
            and row["comparison"]["best_arrival_delta_seconds"] < 0
        ]
        by_k[str(k)] = {
            "requests": paired_count,
            "both_successes": both_successes,
            "both_no_route": both_no_route,
            "fixed_successes": sum(row["comparison"]["fixed_candidate_count"] > 0 for row in pairs),
            "union_successes": sum(row["comparison"]["union_candidate_count"] > 0 for row in pairs),
            "same_exact": sum(row["comparison"]["same_best_exact_signature"] for row in pairs),
            "same_structural": sum(
                row["comparison"]["same_best_structural_signature"] for row in pairs
            ),
            "recombinations": sum(row["comparison"]["recombination_used"] for row in pairs),
            "fixed_no_route_union_route": sum(
                row["comparison"]["fixed_no_route_union_route"] for row in pairs
            ),
            "union_no_route_fixed_route": sum(
                row["comparison"]["union_no_route_fixed_route"] for row in pairs
            ),
            "arrival_deltas": describe(both_routes),
            "recombination_gains": describe(gains),
            "standalone": {
                strategy: {
                    "phase2_us": describe(
                        [
                            row["phase2"]["phase2_latency_us"]
                            for row in single
                            if row["strategy"] == strategy and row["phase2"] is not None
                        ]
                    ),
                    "departure_lookups_median": median_field(single, strategy, "departure_lookups"),
                    "states_median": median_field(single, strategy, "states_evaluated"),
                    "candidates_median": median_field(single, strategy, "final_candidates"),
                    "cli_wall_ms": describe(
                        [row["wall_ms"] for row in single if row["strategy"] == strategy]
                    ),
                }
                for strategy in ("fixed", "union")
            },
        }
    return by_k


def percent(count: int, total: int) -> str:
    return f"{100 * count / total:.1f}%" if total else "n/a"


def ms(value: float | int | None) -> str:
    return f"{value / 1000:.3f} ms" if value is not None else "n/a"


def seconds(value: float | int | None) -> str:
    return f"{value:.2f} s" if value is not None else "n/a"


def repo_relative(path: Path) -> str | None:
    try:
        return path.resolve().relative_to(ROOT).as_posix()
    except ValueError:
        return None


def render_report(metadata: dict[str, Any], paired: list[dict[str, Any]], aggregate_by_k: dict[str, Any]) -> str:
    lines = [
        "# Phase 2 strategy comparison",
        "",
        "## Setup",
        "",
        f"- Commit: `{metadata['commit']}`",
        f"- Experiment time: `{metadata['experiment_time_utc']}`",
        f"- Transit network revision: `{metadata['static_network_revision']}`",
        f"- Schedule revision: `{metadata['schedule_revision']}`",
        "- Timing mode: deterministic fixture schedules (`--service-conditions fixture`); no TDX requests.",
        "- Street routing: the CLI's built-in `StraightLineStreetRouter`; local Valhalla was unavailable.",
        f"- Corpus: `{len(metadata['cases'])}` fixture ODs at `{metadata['departure_at']}`; {metadata['repetitions']} paired repeats and {metadata['repetitions']} standalone repeats after one discarded warmup.",
        "- Limits: K=20/50/100; max transfers=4; max departures/query=3; temporal expansion limit=2,000; static search expansion limit=20,000; candidates=16.",
        "- Corpus limitation: the fixture has no THSR→Metro transfer, so that case is a deliberate no-route control. The checked-in Taipei artifact was rejected by the current loader because it uses the older `services` JSON schema. No current-format real-network artifact was available.",
        "- Live TDX was skipped because `TDX_CLIENT_ID` and `TDX_CLIENT_SECRET` are unset. The report therefore measures fixture schedule quality, not live service quality.",
        "",
        "## Paired quality results",
        "",
        "Each paired row that executed Phase 2 ran Compare mode, so both algorithms used the same Phase 1 corridors and one loaded TimingSnapshot. Rows with `phase2_executed=false` had no retained transit corridor, so Phase 2 did not run or load a snapshot. A negative arrival delta means Union arrived earlier. Winner-agreement and arrival-delta statistics include only requests where both strategies found a route; success and no-route counts use every request.",
        "",
        "| K | Requests | Both no route | Fixed success | Union success | Same exact winner | Same structural winner | Union recombination | Fixed none / Union route | Union none / Fixed route | Arrival delta mean / median / p95 / min..max | Recombination gain mean / median / p95 / max |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for k, data in aggregate_by_k.items():
        count = data["requests"]
        deltas = data["arrival_deltas"]
        gains = data["recombination_gains"]
        lines.append(
            f"| {k} | {count} | {data['both_no_route']} | "
            f"{percent(data['fixed_successes'], count)} | {percent(data['union_successes'], count)} | "
            f"{percent(data['same_exact'], data['both_successes'])} | "
            f"{percent(data['same_structural'], data['both_successes'])} | "
            f"{percent(data['recombinations'], count)} | "
            f"{data['fixed_no_route_union_route']} ({percent(data['fixed_no_route_union_route'], count)}) | "
            f"{data['union_no_route_fixed_route']} ({percent(data['union_no_route_fixed_route'], count)}) | "
            f"{seconds(deltas['mean'])} / {seconds(deltas['median'])} / {seconds(deltas['p95'])} / "
            f"{seconds(deltas['min'])}..{seconds(deltas['max'])} | "
            f"{seconds(gains['mean'])} / {seconds(gains['median'])} / "
            f"{seconds(gains['p95'])} / {seconds(gains['max'])} |"
        )
    lines.extend(
        [
            "",
            "## Standalone Phase 2 performance",
            "",
            "Each strategy ran in its own process. Union and Fixed order alternated by case, K, and repetition. Phase 2 sample counts exclude requests with no retained transit corridor; CLI wall time includes every request, process startup, and fixture setup.",
            "",
            "| K | Strategy | Phase 2 samples | Phase 2 median | Phase 2 p95 | Departure lookups median | States median | Candidates median | CLI wall median / p95 |",
            "|---:|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for k, data in aggregate_by_k.items():
        for strategy in ("fixed", "union"):
            row = data["standalone"][strategy]
            phase2 = row["phase2_us"]
            wall = row["cli_wall_ms"]
            lines.append(
                f"| {k} | {strategy.title()} | {phase2['count']} | {ms(phase2['median'])} | {ms(phase2['p95'])} | "
                f"{row['departure_lookups_median']} | {row['states_median']} | {row['candidates_median']} | "
                f"{wall['median']:.3f} / {wall['p95']:.3f} ms |"
            )
    disagreements = {}
    for row in paired:
        stats = row["comparison"]
        if stats["fixed_candidate_count"] == 0 and stats["union_candidate_count"] == 0:
            continue
        if (
            stats["same_best_exact_signature"]
            and stats["same_best_structural_signature"]
            and not stats["recombination_used"]
            and stats["best_arrival_delta_seconds"] == 0
            and stats["fixed_candidate_count"] == stats["union_candidate_count"]
        ):
            continue
        disagreements.setdefault((row["case_id"], row["k"]), row)
    lines.extend(["", "## Disagreement cases", ""])
    if not disagreements:
        lines.append("No paired disagreement was observed in this corpus.")
    else:
        for (case_id, k), row in sorted(disagreements.items()):
            stats = row["comparison"]
            lines.extend(
                [
                    f"### {case_id}, K={k}",
                    "",
                    f"- Request: `{row['departure_at']}`, modes `{','.join(row['modes'])}`, OD `{row['from']}` → `{row['to']}`.",
                    f"- Fixed winner: {row['fixed_best'] or 'no route'}.",
                    f"- Union winner: {row['union_best'] or 'no route'}.",
                    f"- `recombination_used={stats['recombination_used']}`, arrival delta `{stats['best_arrival_delta_seconds']}` seconds.",
                    "- Retained Phase 1 corridors:",
                ]
            )
            lines.extend(f"  - `{corridor}`" for corridor in row["retained_corridors"])
            lines.append("")
    lines.extend(
        [
            "## Interpretation and limits",
            "",
            "These results are implementation and fixture checks, not evidence about Taipei's current live network. The fixture contains six ODs and does not represent the unavailable current-format transit artifact. It can establish that both strategies run on the same snapshot, that Fixed preserves retained structures, and how the two implementations behave on these schedules. It cannot establish production recombination frequency or justify replacing Union.",
            "",
            "A production decision needs a current-format GTFS-built artifact and a working local Valhalla instance. Then rerun this corpus with 30–50 meaningful ODs and multiple departure times, and run paired Compare with live TDX when credentials are available. Keep CPU-only Phase 2 latency separate from snapshot-loading and route-process wall time.",
            "",
            f"Raw parsed observations: [`phase2-comparison-raw.json`](phase2-comparison-raw.json). Re-run with `python3 server/benchmarks/phase2.py --binary server/target/release/via-route --report docs/phase2-comparison.md --raw docs/phase2-comparison-raw.json --repetitions {metadata['repetitions']}`.",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, default=CORPUS_PATH)
    parser.add_argument("--report", type=Path, default=ROOT / "docs/phase2-comparison.md")
    parser.add_argument("--raw", type=Path, default=ROOT / "docs/phase2-comparison-raw.json")
    parser.add_argument("--repetitions", type=int, default=5)
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("--repetitions must be at least one")
    binary = args.binary.resolve()
    corpus = json.loads(args.corpus.read_text())
    network_path = ROOT / corpus["network"]
    network_artifact = json.loads(network_path.read_text())
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    allowed_dirty_paths = {
        path for output in (args.report, args.raw) if (path := repo_relative(output)) is not None
    }
    dirty_lines = subprocess.check_output(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"], cwd=ROOT, text=True
    ).splitlines()
    unexpected_dirty_paths = [
        line[3:]
        for line in dirty_lines
        if line[3:] not in allowed_dirty_paths
    ]
    if unexpected_dirty_paths:
        raise RuntimeError(
            "benchmark requires committed source; unexpected dirty paths: "
            + ", ".join(unexpected_dirty_paths)
        )
    paired: list[dict[str, Any]] = []
    standalone: list[dict[str, Any]] = []

    for case_index, case in enumerate(corpus["cases"]):
        for k_index, k in enumerate(K_VALUES):
            compare_warmup = run_route(
                binary, network_path, case, k, "compare", corpus["departure_at"]
            )
            union_warmup = run_route(binary, network_path, case, k, "union", corpus["departure_at"])
            fixed_warmup = run_route(binary, network_path, case, k, "fixed", corpus["departure_at"])
            del compare_warmup, union_warmup, fixed_warmup

            for repetition in range(args.repetitions):
                paired_run = run_route(
                    binary, network_path, case, k, "compare", corpus["departure_at"]
                )
                comparison, union_best, fixed_best, retained = parse_comparison(
                    paired_run["diagnostics"]
                )
                paired.append(
                    {
                        "case_id": case["id"],
                        "fixture_support": case["fixture_support"],
                        "k": k,
                        "repetition": repetition + 1,
                        "departure_at": corpus["departure_at"],
                        "from": case["from"],
                        "to": case["to"],
                        "modes": case["modes"],
                        "wall_ms": paired_run["wall_ms"],
                        "comparison": comparison,
                        "union_best": union_best,
                        "fixed_best": fixed_best,
                        "retained_corridors": retained
                        if comparison["recombination_used"]
                        or not comparison["same_best_structural_signature"]
                        or comparison["best_arrival_delta_seconds"] not in (0, None)
                        else [],
                    }
                )

            for repetition in range(args.repetitions):
                order = ("union", "fixed")
                if (repetition + case_index + k_index) % 2:
                    order = tuple(reversed(order))
                for strategy in order:
                    result = run_route(
                        binary, network_path, case, k, strategy, corpus["departure_at"]
                    )
                    standalone.append(
                        {
                            "case_id": case["id"],
                            "fixture_support": case["fixture_support"],
                            "k": k,
                            "repetition": repetition + 1,
                            "strategy": strategy,
                            "modes": case["modes"],
                            "wall_ms": result["wall_ms"],
                            "phase2": result["phase2"],
                            "options": result["options"],
                        }
                    )

    metadata = {
        "commit": commit,
        "experiment_time_utc": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "static_network_revision": network_artifact.get("static_network_revision", "unknown"),
        "schedule_revision": network_artifact.get("schedule_revision", "unknown"),
        "departure_at": corpus["departure_at"],
        "repetitions": args.repetitions,
        "warmups_per_case_k_strategy": 1,
        "cases": corpus["cases"],
        "binary": str(binary),
    }
    result = {
        "metadata": metadata,
        "paired": paired,
        "standalone": standalone,
        "aggregate": aggregate(paired, standalone),
    }
    args.raw.parent.mkdir(parents=True, exist_ok=True)
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.raw.write_text(json.dumps(result, indent=2) + "\n")
    args.report.write_text(render_report(metadata, paired, result["aggregate"]))
    print(f"wrote report: {args.report}")
    print(f"wrote raw observations: {args.raw}")
    for k, data in result["aggregate"].items():
        count = data["requests"]
        print(
            f"K={k}: fixed={percent(data['fixed_successes'], count)} "
            f"union={percent(data['union_successes'], count)} "
            f"recombination={percent(data['recombinations'], count)} "
            f"fixed/union p2 median="
            f"{ms(data['standalone']['fixed']['phase2_us']['median'])}/"
            f"{ms(data['standalone']['union']['phase2_us']['median'])}"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
