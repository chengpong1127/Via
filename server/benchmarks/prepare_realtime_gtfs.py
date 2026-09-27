#!/usr/bin/env python3
"""Prepare the checked-in GTFS archive for the current via-data importer."""

from __future__ import annotations

import argparse
import csv
import hashlib
import io
import json
import re
import tempfile
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_INPUT = ROOT / "server/data/dev/gtfs.zip"
DEFAULT_OUTPUT = ROOT / "server/data/dev/phase2-realtime/gtfs-transit-compatible.zip"
TRANSIT_TABLES = (
    "agency.txt",
    "routes.txt",
    "stops.txt",
    "stop_times.txt",
    "calendar.txt",
    "calendar_dates.txt",
    "frequencies.txt",
    "trips.txt",
    "shapes.txt",
)
TIME_WITHOUT_SECONDS = re.compile(r"^\d{1,3}:\d{2}$")
GTFS_TIME = re.compile(r"^\d{1,3}:\d{2}:\d{2}$")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def output_info(source: ZipInfo) -> ZipInfo:
    target = ZipInfo(source.filename, date_time=source.date_time)
    target.compress_type = ZIP_DEFLATED
    target.external_attr = source.external_attr
    target.create_system = source.create_system
    target.comment = source.comment
    return target


def copy_table(source: ZipFile, target: ZipFile, entry: ZipInfo, counters: dict[str, int]) -> None:
    if entry.filename == "trips.txt":
        with source.open(entry) as raw, target.open(output_info(entry), "w") as compressed:
            reader = csv.DictReader(io.TextIOWrapper(raw, encoding="utf-8-sig", newline=""))
            if reader.fieldnames is None:
                raise ValueError("trips.txt has no header")
            writer = csv.DictWriter(compressed_text := io.TextIOWrapper(compressed, encoding="utf-8", newline=""), fieldnames=reader.fieldnames, lineterminator="\r\n")
            writer.writeheader()
            for row in reader:
                direction = row.get("direction_id", "")
                if direction and direction not in {"0", "1"}:
                    row["direction_id"] = ""
                    counters["unsupported_direction_values_cleared"] += 1
                writer.writerow(row)
            compressed_text.flush()
            compressed_text.detach()
        return

    if entry.filename == "stop_times.txt":
        with source.open(entry) as raw, target.open(output_info(entry), "w") as compressed:
            reader = csv.DictReader(io.TextIOWrapper(raw, encoding="utf-8-sig", newline=""))
            if reader.fieldnames is None:
                raise ValueError("stop_times.txt has no header")
            writer_text = io.TextIOWrapper(compressed, encoding="utf-8", newline="")
            writer = csv.DictWriter(writer_text, fieldnames=reader.fieldnames, lineterminator="\r\n")
            writer.writeheader()
            for row in reader:
                for name in ("arrival_time", "departure_time"):
                    value = row.get(name, "") or ""
                    if TIME_WITHOUT_SECONDS.fullmatch(value):
                        row[name] = f"{value}:00"
                        counters["minute_only_times_canonicalized"] += 1
                    elif value and not GTFS_TIME.fullmatch(value):
                        raise ValueError(f"unsupported {name} value {value!r} in stop_times.txt")
                writer.writerow(row)
            writer_text.flush()
            writer_text.detach()
        return

    with source.open(entry) as raw, target.open(output_info(entry), "w") as output:
        while chunk := raw.read(1024 * 1024):
            output.write(chunk)


def prepare(input_path: Path, output_path: Path) -> dict[str, object]:
    output_path.parent.mkdir(parents=True, exist_ok=True)
    temporary_path: Path | None = None
    counters = {
        "unsupported_direction_values_cleared": 0,
        "minute_only_times_canonicalized": 0,
    }
    try:
        with tempfile.NamedTemporaryFile(
            prefix=f".{output_path.name}.", suffix=".tmp", dir=output_path.parent, delete=False
        ) as temporary:
            temporary_path = Path(temporary.name)
        with ZipFile(input_path) as source, ZipFile(
            temporary_path, "w", compression=ZIP_DEFLATED, compresslevel=1
        ) as target:
            entries = {entry.filename: entry for entry in source.infolist() if entry.filename in TRANSIT_TABLES}
            missing = sorted(set(TRANSIT_TABLES) - entries.keys())
            if missing:
                raise ValueError(f"GTFS archive is missing required transit tables: {missing}")
            for name in TRANSIT_TABLES:
                copy_table(source, target, entries[name], counters)
        temporary_path.replace(output_path)
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)

    preparation = {
        "source_archive": str(input_path),
        "source_sha256": sha256_file(input_path),
        "prepared_archive": str(output_path),
        "prepared_sha256": sha256_file(output_path),
        "transit_tables": list(TRANSIT_TABLES),
        "transformations": {
            "unsupported_direction_values_cleared": counters["unsupported_direction_values_cleared"],
            "minute_only_times_canonicalized": counters["minute_only_times_canonicalized"],
            "other_transit_fields_changed": 0,
        },
        "reason": (
            "Via ignores trip direction_id; the GTFS parser only accepts 0 or 1. "
            "Minute-only stop times are expressed as the same minute with explicit zero seconds."
        ),
    }
    metadata_path = output_path.with_suffix(".preparation.json")
    metadata_path.write_text(json.dumps(preparation, indent=2) + "\n")
    return preparation


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, default=DEFAULT_INPUT)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    result = prepare(args.input.resolve(), args.output.resolve())
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
