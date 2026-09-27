#!/usr/bin/env python3
"""Run paced paired Phase 2 comparisons against live TDX timing data."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import statistics
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
K_VALUES = (20, 50, 100)
CORPUS_DEFAULT = ROOT / "server/fixtures/routing/phase2-realtime-taipei.json"
RAW_DEFAULT = ROOT / "docs/phase2-realtime-comparison-raw.json"
REPORT_DEFAULT = ROOT / "docs/phase2-realtime-comparison.md"


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds")


def configured(name: str) -> bool:
    if os.environ.get(name, "").strip():
        return True
    dotenv_path = ROOT / "server/.env"
    if not dotenv_path.is_file():
        return False
    for line in dotenv_path.read_text().splitlines():
        key, separator, value = line.partition("=")
        if separator and key.strip() == name and value.strip().strip("'\" "):
            return True
    return False


def configured_value(name: str, default: str) -> str:
    if os.environ.get(name, "").strip():
        return os.environ[name].strip()
    dotenv_path = ROOT / "server/.env"
    if dotenv_path.is_file():
        for line in dotenv_path.read_text().splitlines():
            key, separator, value = line.partition("=")
            if separator and key.strip() == name:
                return value.strip().strip("'\" ") or default
    return default


def parse_json_result(stdout: str) -> dict[str, Any] | None:
    start = stdout.find("\n{\n")
    if start >= 0:
        stdout = stdout[start + 1 :]
    try:
        result = json.loads(stdout)
    except json.JSONDecodeError:
        return None
    return result if isinstance(result, dict) else None


def number(line: str, name: str) -> int | None:
    match = re.search(rf"\b{name}=(\d+)", line)
    return int(match.group(1)) if match else None


def optional_number(line: str, name: str) -> int | None:
    match = re.search(rf"\b{name}=(None|\d+)", line)
    if not match or match.group(1) == "None":
        return None
    return int(match.group(1))


def parse_phase1(diagnostics: str) -> dict[str, Any]:
    line = next((row for row in diagnostics.splitlines() if "static Top-K corridor search" in row), "")
    if not line:
        return {"executed": False}
    groups_line = next(
        (row for row in diagnostics.splitlines() if "retained static corridor mode-sequence groups" in row),
        "",
    )
    group_pattern = re.compile(
        r"RetainedModeSequenceStats \{ mode_sequence: \[([^]]*)\], corridors: (\d+), "
        r"best_duration_seconds: (\d+), worst_duration_seconds: (\d+) \}"
    )
    groups = [
        {
            "mode_sequence": [mode.strip() for mode in match.group(1).split(",") if mode.strip()],
            "corridors": int(match.group(2)),
            "best_duration_seconds": int(match.group(3)),
            "worst_duration_seconds": int(match.group(4)),
        }
        for match in group_pattern.finditer(groups_line)
    ]
    return {
        "executed": True,
        "latency_ms": number(line, "static_search_latency_ms"),
        "states_expanded": number(line, "static_search_states_expanded"),
        "corridors_retained": number(line, "static_corridors_retained"),
        "kth_static_duration_seconds": optional_number(line, "kth_static_duration_seconds"),
        "candidate_pattern_count": number(line, "static_corridor_patterns"),
        "candidate_position_count": number(line, "static_corridor_positions"),
        "mode_sequence_groups": groups,
    }


def parse_snapshot(diagnostics: str) -> dict[str, Any]:
    line = next((row for row in diagnostics.splitlines() if "timing snapshot positions_requested=" in row), "")
    if not line:
        return {"executed": False, "basis_counts": {}, "realtime_value_count": 0}
    match = re.search(r"snapshot_summary=\((\d+),\s*(\d+),\s*(\d+),\s*\[(.*?)\]\)", line)
    if not match:
        return {"executed": True, "parse_error": line, "basis_counts": {}, "realtime_value_count": 0}
    successful, empty, failed = (int(match.group(index)) for index in (1, 2, 3))
    basis_counts = {
        name: int(count)
        for name, count in re.findall(r"\((\w+),\s*(\d+)\)", match.group(4))
    }
    realtime_count = sum(
        count for name, count in basis_counts.items() if name in {"RealtimePrediction", "DelayAdjustedSchedule"}
    )
    return {
        "executed": True,
        "positions_requested": number(line, "positions_requested"),
        "successful": successful,
        "empty": empty,
        "failed": failed,
        "basis_counts": basis_counts,
        "realtime_value_count": realtime_count,
        "latency_ms": number(line, "latency_ms"),
    }


def parse_provider_metrics(diagnostics: str) -> dict[str, Any]:
    rows = [row for row in diagnostics.splitlines() if " timing snapshot " in row]
    metrics: dict[str, Any] = {"bus_http_requests": 0, "bus_rows_received": 0, "bus_rows_relevant": 0}
    for line in rows:
        if "bus timing snapshot" in line:
            metrics.update(
                {
                    "bus_relevant_stops": number(line, "relevant_stops"),
                    "bus_scopes": number(line, "scopes"),
                    "bus_http_requests": number(line, "http_requests") or 0,
                    "bus_rows_received": number(line, "rows_received") or 0,
                    "bus_rows_relevant": number(line, "rows_relevant") or 0,
                    "bus_outcome": re.search(r'outcome="([^"]+)"', line).group(1)
                    if re.search(r'outcome="([^"]+)"', line)
                    else None,
                }
            )
    return metrics


def parse_comparison(diagnostics: str) -> tuple[dict[str, Any], str | None, str | None, list[str]]:
    line = next((row for row in diagnostics.splitlines() if "Phase 2 strategy comparison" in row), "")
    if not line:
        return {"phase2_executed": False}, None, None, []
    fields: dict[str, Any] = {}
    for name, value in re.findall(r"\b([a-z_]+): (true|false|Some\(-?\d+\)|None|\d+)", line):
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
    fields["phase2_executed"] = True
    required = (
        "union_latency_us",
        "fixed_latency_us",
        "union_candidate_count",
        "fixed_candidate_count",
        "same_best_exact_signature",
        "same_best_structural_signature",
        "recombination_used",
        "best_arrival_delta_seconds",
    )
    missing = [name for name in required if name not in fields]
    if missing:
        raise RuntimeError(f"missing Compare fields {missing}: {line[:1200]}")
    union_best = debug_summary(line, "union_best")
    fixed_best = debug_summary(line, "fixed_best")
    retained = [
        row.split("DEBUG via_server::routing: retained static corridor ", 1)[1]
        for row in diagnostics.splitlines()
        if "DEBUG via_server::routing: retained static corridor corridor_rank=" in row
    ]
    return fields, union_best, fixed_best, retained


def debug_summary(line: str, name: str) -> str | None:
    match = re.search(rf'{name}=Some\("((?:\\.|[^"\\])*)"\)', line)
    if not match:
        return None
    return json.loads('"' + match.group(1) + '"')


def run_route(
    binary: Path,
    network: Path,
    case: dict[str, Any],
    k: int,
    strategy: str,
) -> dict[str, Any]:
    origin = case["origin"]
    destination = case["destination"]
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
        "--depart-now",
        "--modes",
        ",".join(case["selected_modes"]),
        "--service-conditions",
        "live",
        "--phase2-strategy",
        strategy,
        "--max-static-corridors",
        str(k),
        "--trace",
        "--json",
    ]
    started_utc = utc_now()
    started = time.perf_counter_ns()
    process = subprocess.run(command, cwd=ROOT / "server", capture_output=True, text=True, check=False)
    wall_ms = (time.perf_counter_ns() - started) / 1_000_000
    diagnostics = process.stdout + "\n" + process.stderr
    if process.returncode:
        return {
            "started_at_utc": started_utc,
            "wall_ms": round(wall_ms, 3),
            "returncode": process.returncode,
            "error_tail": diagnostics[-3000:],
        }
    result = parse_json_result(process.stdout)
    if result is None:
        return {
            "started_at_utc": started_utc,
            "wall_ms": round(wall_ms, 3),
            "returncode": process.returncode,
            "error_tail": "via-route returned no parseable JSON result",
        }
    comparison, union_best, fixed_best, retained = parse_comparison(diagnostics)
    structural_disagreement = comparison.get("phase2_executed") and not comparison.get(
        "same_best_structural_signature", False
    )
    return {
        "started_at_utc": started_utc,
        "effective_search_time": result.get("effective_search_time"),
        "wall_ms": round(wall_ms, 3),
        "returncode": process.returncode,
        "journey_option_count": len(result.get("journey_options") or []),
        "phase1": parse_phase1(diagnostics),
        "timing_snapshot": parse_snapshot(diagnostics),
        "provider_metrics": parse_provider_metrics(diagnostics),
        "comparison": comparison,
        "fixed_best": fixed_best,
        "union_best": union_best,
        "retained_corridors": retained if structural_disagreement or comparison.get("recombination_used") else [],
        "rate_limited": 'outcome="rate_limited"' in diagnostics or "TDX API rate limit reached" in diagnostics,
    }


def nearest_rank(values: list[float | int], percentile: float) -> float | int | None:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(percentile * len(ordered)) - 1)]


def describe(values: list[float | int]) -> dict[str, float | int | None]:
    return {
        "count": len(values),
        "mean": statistics.fmean(values) if values else None,
        "median": statistics.median(values) if values else None,
        "p90_nearest_rank": nearest_rank(values, 0.90),
        "p95_nearest_rank": nearest_rank(values, 0.95),
        "max": max(values) if values else None,
    }


def live_rows(rows: list[dict[str, Any]]) -> list[dict[str, Any]]:
    return [
        row
        for row in rows
        if row.get("comparison", {}).get("phase2_executed")
        and row.get("timing_snapshot", {}).get("realtime_value_count", 0) > 0
        and row.get("timing_snapshot", {}).get("failed", 0) == 0
    ]


def aggregate(paired: list[dict[str, Any]], standalone: list[dict[str, Any]]) -> dict[str, Any]:
    live = live_rows(paired)
    result: dict[str, Any] = {"paired_live_observations": len(live), "by_k": {}}
    for k in K_VALUES:
        rows = [row for row in live if row["k"] == k]
        comparisons = [row["comparison"] for row in rows]
        both = [
            comparison
            for comparison in comparisons
            if comparison["fixed_candidate_count"] > 0 and comparison["union_candidate_count"] > 0
        ]
        deltas = [
            -comparison["best_arrival_delta_seconds"]
            for comparison in both
            if comparison["best_arrival_delta_seconds"] is not None
        ]
        different_structure = [
            comparison
            for comparison in both
            if not comparison["same_best_structural_signature"]
        ]
        recombinations = [
            comparison
            for comparison in comparisons
            if comparison["recombination_used"]
            and comparison["best_arrival_delta_seconds"] is not None
        ]
        result["by_k"][str(k)] = {
            "observations": len(rows),
            "fixed_successes": sum(item["fixed_candidate_count"] > 0 for item in comparisons),
            "union_successes": sum(item["union_candidate_count"] > 0 for item in comparisons),
            "both_route": len(both),
            "fixed_only": sum(item["union_no_route_fixed_route"] for item in comparisons),
            "union_only": sum(item["fixed_no_route_union_route"] for item in comparisons),
            "neither": sum(item["fixed_candidate_count"] == item["union_candidate_count"] == 0 for item in comparisons),
            "same_best_exact": sum(item["same_best_exact_signature"] for item in both),
            "same_best_structural": sum(item["same_best_structural_signature"] for item in both),
            "union_winner_exactly_retained": sum(item["union_best_in_retained_topk_exact"] for item in comparisons),
            "union_winner_structurally_retained_pattern_substitution": sum(
                item["union_best_in_retained_topk_structural"] and not item["union_best_in_retained_topk_exact"]
                for item in comparisons
            ),
            "structurally_different": len(different_structure),
            "true_recombinations": sum(item["recombination_used"] for item in comparisons),
            "union_rescues": sum(item["fixed_no_route_union_route"] for item in comparisons),
            "arrival_delta_fixed_minus_union_seconds": describe(deltas),
            "structural_difference_delta_seconds": describe(
                [-item["best_arrival_delta_seconds"] for item in different_structure if item["best_arrival_delta_seconds"] is not None]
            ),
            "true_recombination_union_gain_seconds": describe(
                [-item["best_arrival_delta_seconds"] for item in recombinations]
            ),
            "union_gain_at_least_120_seconds": sum(value >= 120 for value in deltas),
            "union_gain_at_least_300_seconds": sum(value >= 300 for value in deltas),
            "phase2_fixed_latency_us": describe([item["comparison"]["fixed_latency_us"] for item in rows]),
            "phase2_union_latency_us": describe([item["comparison"]["union_latency_us"] for item in rows]),
            "fixed_departure_lookups": describe([item["comparison"]["fixed_departure_lookups"] for item in rows]),
            "union_departure_lookups": describe([item["comparison"]["union_departure_lookups"] for item in rows]),
            "fixed_states_evaluated": describe([item["comparison"]["fixed_states_evaluated"] for item in rows]),
            "union_states_expanded": describe([item["comparison"]["union_states_expanded"] for item in rows]),
            "fixed_candidates": describe([item["comparison"]["fixed_candidate_count"] for item in rows]),
            "union_candidates": describe([item["comparison"]["union_candidate_count"] for item in rows]),
        }
    result["standalone"] = {
        strategy: {
            str(k): {
                "phase2_latency_us": describe(
                    [
                        row["phase2"]["phase2_latency_us"]
                        for row in standalone
                        if row["strategy"] == strategy and row["k"] == k and row.get("phase2")
                    ]
                ),
                "departure_lookups": describe(
                    [row["phase2"]["departure_lookups"] for row in standalone if row["strategy"] == strategy and row["k"] == k and row.get("phase2")]
                ),
                "states_evaluated_or_expanded": describe(
                    [row["phase2"]["states_evaluated"] for row in standalone if row["strategy"] == strategy and row["k"] == k and row.get("phase2")]
                ),
                "candidates": describe(
                    [row["phase2"]["final_candidates"] for row in standalone if row["strategy"] == strategy and row["k"] == k and row.get("phase2")]
                ),
            }
            for k in K_VALUES
        }
        for strategy in ("fixed", "union")
    }
    return result


def write_raw(path: Path, result: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--network", type=Path)
    parser.add_argument("--corpus", type=Path, default=CORPUS_DEFAULT)
    parser.add_argument("--raw", type=Path, default=RAW_DEFAULT)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--interval-seconds", type=float, default=10.0)
    parser.add_argument("--standalone-repetitions", type=int, default=3)
    parser.add_argument("--skip-standalone", action="store_true")
    parser.add_argument("--resume", action="store_true", help="resume a compatible partial raw capture")
    args = parser.parse_args()
    if args.rounds < 1 or args.interval_seconds < 0 or args.standalone_repetitions < 1:
        parser.error("rounds/repetitions must be positive and interval must not be negative")

    binary = args.binary.resolve()
    corpus_path = args.corpus.resolve()
    credentials_present = configured("TDX_CLIENT_ID") and configured("TDX_CLIENT_SECRET")
    if not credentials_present:
        parser.error("TDX_CLIENT_ID and TDX_CLIENT_SECRET must be configured; no live experiment was started")
    corpus = json.loads(corpus_path.read_text())
    network = (args.network or (ROOT / corpus["network"])).resolve()
    manifest_path = network.parent / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    manifest_bytes = manifest_path.read_bytes()
    preparation_path = network.parent.parent / "gtfs-transit-compatible.preparation.json"
    preparation = json.loads(preparation_path.read_text()) if preparation_path.is_file() else None
    corpus_bytes = corpus_path.read_bytes()
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    bus_case = next((case for case in corpus["cases"] if case["case_id"] == "bus41_one_stop"), corpus["cases"][0])
    metadata = {
        "source_commit": commit,
        "experiment_started_at_utc": utc_now(),
        "experiment_completed_at_utc": None,
        "experiment_status": "running",
        "stop_reason": None,
        "artifact_path": network.relative_to(ROOT).as_posix() if network.is_relative_to(ROOT) else str(network),
        "artifact_manifest_sha256": hashlib.sha256(manifest_bytes).hexdigest(),
        "artifact_source_hashes": manifest.get("sources", {}),
        "gtfs_preparation": preparation,
        "gtfs_preparation_sha256": hashlib.sha256(preparation_path.read_bytes()).hexdigest()
        if preparation_path.is_file()
        else None,
        "static_network_revision": manifest.get("revisions", {}).get("static_network_revision"),
        "schedule_revision": manifest.get("revisions", {}).get("schedule_revision"),
        "artifact_counts": manifest.get("counts", {}),
        "tdx_timing_mode": "live; each Compare request loads one TimingSnapshot shared by Fixed and Union",
        "street_router": {"endpoint": "http://valhalla:8002/", "version": "3.9.0"},
        "tdx_api_base_url": configured_value("VIA_TDX_BASE_URL", "https://tdx.transportdata.tw/api/basic/"),
        "service_conditions": "live",
        "tdx_credentials_present": credentials_present,
        "tdx_credentials_logged": False,
        "routing_limits": {
            "max_transfers": 4,
            "max_candidates": 16,
            "max_static_search_expansions": 20000,
            "max_temporal_search_expansions": 2000,
            "max_departures_per_query": 3,
            "max_journey_duration_seconds": 14400,
            "max_access_stops": 24,
            "max_egress_stops": 24,
            "per_run_static_corridor_limit": "K (20, 50, or 100)",
        },
        "corpus_path": corpus_path.relative_to(ROOT).as_posix() if corpus_path.is_relative_to(ROOT) else str(corpus_path),
        "corpus_sha256": hashlib.sha256(corpus_bytes).hexdigest(),
        "runner_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "cases": corpus["cases"],
        "k_values": list(K_VALUES),
        "rounds_requested": args.rounds,
        "separate_live_snapshot_per_route_invocation": True,
        "interval_seconds": args.interval_seconds,
        "standalone_repetitions_requested": 0 if args.skip_standalone else args.standalone_repetitions,
        "binary": str(binary),
        "mode_support_note": "Only Compare observations whose snapshot has RealtimePrediction or DelayAdjustedSchedule values count toward realtime quality aggregates.",
        "aggregation_percentiles": "nearest rank",
    }
    if args.resume:
        if not args.raw.is_file():
            parser.error("--resume requires an existing --raw file")
        previous = json.loads(args.raw.read_text())
        previous_metadata = previous.get("metadata", {})
        for key in ("artifact_manifest_sha256", "corpus_sha256", "static_network_revision", "schedule_revision"):
            if previous_metadata.get(key) != metadata.get(key):
                parser.error(f"cannot resume: {key} differs from the existing raw capture")
        metadata["previous_experiment_status"] = previous_metadata.get("experiment_status")
        metadata["previous_stop_reason"] = previous_metadata.get("stop_reason")
        metadata["resumed_at_utc"] = utc_now()
        result: dict[str, Any] = {
            "metadata": metadata,
            "paired": previous.get("paired", []),
            "standalone": previous.get("standalone", []),
            "aggregate": previous.get("aggregate", {}),
        }
    else:
        result = {"metadata": metadata, "paired": [], "standalone": [], "aggregate": {}}
    expected = len(corpus["cases"]) * len(K_VALUES) * args.rounds
    completed_keys = {
        (row["case_id"], row["round"], row["k"])
        for row in result["paired"]
        if not row.get("rate_limited")
        and row.get("returncode", 0) == 0
        and not row.get("error_tail")
    }
    completed = len(completed_keys)
    write_raw(args.raw, result)

    def execute(case: dict[str, Any], k: int, strategy: str) -> dict[str, Any]:
        diagnostic_cmd = [
            str(binary),
            "--network",
            str(network),
            "--from-lat",
            str(case["origin"]["latitude"]),
            "--from-lon",
            str(case["origin"]["longitude"]),
            "--to-lat",
            str(case["destination"]["latitude"]),
            "--to-lon",
            str(case["destination"]["longitude"]),
            "--depart-now",
            "--modes",
            ",".join(case["selected_modes"]),
            "--service-conditions",
            "live",
            "--phase2-strategy",
            strategy,
            "--max-static-corridors",
            str(k),
            "--trace",
            "--json",
        ]
        # run_route's Compare fields are not used for standalone output; execute
        # the requested strategy separately and parse its own Phase 2 counters.
        started_utc = utc_now()
        started = time.perf_counter_ns()
        process = subprocess.run(diagnostic_cmd, cwd=ROOT / "server", capture_output=True, text=True, check=False)
        wall_ms = (time.perf_counter_ns() - started) / 1_000_000
        diagnostics = process.stdout + "\n" + process.stderr
        from phase2 import parse_phase2  # Reuse the fixture harness's stable trace parser.

        result_json = parse_json_result(process.stdout)
        return {
            "started_at_utc": started_utc,
            "effective_search_time": result_json.get("effective_search_time") if result_json else None,
            "wall_ms": round(wall_ms, 3),
            "returncode": process.returncode,
            "phase1": parse_phase1(diagnostics),
            "timing_snapshot": parse_snapshot(diagnostics),
            "provider_metrics": parse_provider_metrics(diagnostics),
            "phase2": parse_phase2(diagnostics, strategy) if process.returncode == 0 else None,
            "journey_option_count": len((result_json or {}).get("journey_options") or []),
            "rate_limited": 'outcome="rate_limited"' in diagnostics or "TDX API rate limit reached" in diagnostics,
        }

    try:
        for round_number in range(1, args.rounds + 1):
            for k in K_VALUES:
                for case in corpus["cases"]:
                    key = (case["case_id"], round_number, k)
                    if key in completed_keys:
                        continue
                    run = run_route(binary, network, case, k, "compare")
                    row = {
                        "case_id": case["case_id"],
                        "scenario_category": case["scenario_category"],
                        "round": round_number,
                        "k": k,
                        "origin": case["origin"],
                        "destination": case["destination"],
                        "selected_modes": case["selected_modes"],
                        **run,
                    }
                    existing_index = next(
                        (
                            index
                            for index, existing in enumerate(result["paired"])
                            if (existing["case_id"], existing["round"], existing["k"]) == key
                        ),
                        None,
                    )
                    if existing_index is None:
                        result["paired"].append(row)
                    else:
                        result["paired"][existing_index] = row
                    completed_keys.add(key)
                    completed += 1
                    result["aggregate"] = aggregate(result["paired"], result["standalone"])
                    write_raw(args.raw, result)
                    snapshot = row.get("timing_snapshot", {})
                    comparison = row.get("comparison", {})
                    print(
                        f"paired {completed}/{expected} round={round_number} K={k} {case['case_id']} "
                        f"realtime={snapshot.get('realtime_value_count', 0)} "
                        f"fixed={comparison.get('fixed_candidate_count', 'n/a')} "
                        f"union={comparison.get('union_candidate_count', 'n/a')} wall={row.get('wall_ms', 0):.0f}ms",
                        flush=True,
                    )
                    if row.get("rate_limited") or row.get("provider_metrics", {}).get("bus_outcome") == "rate_limited":
                        metadata["experiment_status"] = "halted_rate_limit"
                        metadata["stop_reason"] = f"TDX rate limit on round {round_number}, K={k}, {case['case_id']}"
                        raise StopIteration
                    if row.get("timing_snapshot", {}).get("failed", 0) > 0:
                        metadata["experiment_status"] = "halted_provider_failure"
                        metadata["stop_reason"] = f"TimingSnapshot had failed values on round {round_number}, K={k}, {case['case_id']}"
                        raise StopIteration
                    if "error_tail" in row:
                        metadata["experiment_status"] = "halted_route_error"
                        metadata["stop_reason"] = row["error_tail"][-1000:]
                        raise StopIteration
                    if args.interval_seconds:
                        time.sleep(args.interval_seconds)

        if not args.skip_standalone:
            for k in K_VALUES:
                for repetition in range(1, args.standalone_repetitions + 1):
                    for strategy in ("fixed", "union"):
                        run = execute(bus_case, k, strategy)
                        row = {
                            "case_id": bus_case["case_id"],
                            "scenario_category": bus_case["scenario_category"],
                            "repetition": repetition,
                            "k": k,
                            "strategy": strategy,
                            "origin": bus_case["origin"],
                            "destination": bus_case["destination"],
                            "selected_modes": bus_case["selected_modes"],
                            **run,
                        }
                        result["standalone"].append(row)
                        result["aggregate"] = aggregate(result["paired"], result["standalone"])
                        write_raw(args.raw, result)
                        phase2 = row.get("phase2") or {}
                        print(
                            f"standalone {strategy} K={k} rep={repetition} "
                            f"realtime={row.get('timing_snapshot', {}).get('realtime_value_count', 0)} "
                            f"phase2_us={phase2.get('phase2_latency_us')} wall={row.get('wall_ms', 0):.0f}ms",
                            flush=True,
                        )
                        if row.get("rate_limited") or row.get("provider_metrics", {}).get("bus_outcome") == "rate_limited":
                            metadata["experiment_status"] = "halted_rate_limit"
                            metadata["stop_reason"] = f"TDX rate limit during standalone {strategy} K={k}"
                            raise StopIteration
                        if row.get("timing_snapshot", {}).get("failed", 0) > 0:
                            metadata["experiment_status"] = "halted_provider_failure"
                            metadata["stop_reason"] = f"TimingSnapshot failed during standalone {strategy} K={k}"
                            raise StopIteration
                        if args.interval_seconds:
                            time.sleep(args.interval_seconds)
    except StopIteration:
        pass
    except KeyboardInterrupt:
        metadata["experiment_status"] = "halted_interrupted"
        metadata["stop_reason"] = "capture interrupted by operator"

    metadata["experiment_completed_at_utc"] = utc_now()
    if metadata["experiment_status"] == "running":
        metadata["experiment_status"] = "complete"
    result["aggregate"] = aggregate(result["paired"], result["standalone"])
    write_raw(args.raw, result)
    print(
        f"wrote {len(result['paired'])} paired rows and {len(result['standalone'])} standalone rows to {args.raw}",
        flush=True,
    )
    return 0 if metadata["experiment_status"] == "complete" else 2


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    sys.exit(main())
