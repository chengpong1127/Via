#!/usr/bin/env python3
import csv
import json
import pathlib
import statistics
import sys

base = pathlib.Path(__file__).resolve().parent
rows = []
for filename in sys.argv[1:]:
    for line in pathlib.Path(filename).read_text().splitlines():
        if line.startswith('REPRESENTATIVE '):
            row = json.loads(line.removeprefix('REPRESENTATIVE '))
            row['source_log'] = pathlib.Path(filename).name
            row['scenario_id'] = 'taipei_main_to_xinyi_2026_10_01_1000_taipei'
            row['candidate_pool_id'] = row['workload'] + ':' + str(row['pool_size']) + ':' + row['identity_digest'][:16]
            row['valhalla_max_concurrency'] = 8
            row['phase2_concurrency'] = 4
            rows.append(row)
if not rows:
    raise SystemExit('Usage: summarize-representative.py LOG [LOG ...]')
keys = list(rows[0]) + ['source_log'] if 'source_log' not in rows[0] else list(rows[0])
with (base / 'benchmarks/reconstruction-representative-raw.csv').open('w', newline='') as f:
    writer = csv.DictWriter(f, fieldnames=keys, lineterminator='\n')
    writer.writeheader()
    for row in rows:
        row = row.copy()
        row['top3_keys'] = json.dumps(row['top3_keys'], separators=(',', ':'))
        writer.writerow(row)
groups = {}
for row in rows:
    groups.setdefault((row['workload'], row['pool_size'], row['full'], row['concurrency']), []).append(row)
def pct(values, q):
    values = sorted(values)
    x = (len(values) - 1) * q
    i = int(x)
    return values[i] + (values[min(i + 1, len(values) - 1)] - values[i]) * (x - i)
summary = []
for (workload, size, full, concurrency), samples in sorted(groups.items()):
    times = [r['wall_ms'] for r in samples]
    baseline = groups.get((workload, size, full, 1), [])
    summary.append({
        'workload': workload, 'pool_size': size, 'full': full, 'concurrency': concurrency,
        'samples': len(samples), 'p50_ms': pct(times, 0.5),
        'p90_ms': pct(times, 0.9) if len(times) >= 20 else None,
        'p95_ms': pct(times, 0.95) if len(times) >= 20 else None,
        'mean_ms': statistics.mean(times), 'stddev_ms': statistics.stdev(times) if len(times) > 1 else None,
        'p50_speedup': pct([r['wall_ms'] for r in baseline], 0.5) / pct(times, 0.5) if baseline else None,
        'mean_http_attempts': statistics.mean(r['physical_http_attempts'] for r in samples),
        'throughput_candidates_per_second': size * 1000 / statistics.mean(times),
        'operation_timeout_rate': sum(r['operation_timeouts'] for r in samples) / max(1, sum(r['logical_operations'] for r in samples)),
        'candidate_failure_rate': sum(r['failed'] for r in samples) / (size * len(samples)),
        'operation_timeouts': sum(r['operation_timeouts'] for r in samples),
        'failed_candidates': sum(r['failed'] for r in samples),
        'mean_semaphore_wait_total_ms': statistics.mean(r['wait_total_ms'] for r in samples),
        'max_http_active': max(r['max_http_active'] for r in samples),
        'candidate_output_digests': sorted({r['candidate_output_digest'] for r in samples}),
        'top3_output_digests': sorted({r['top3_output_digest'] for r in samples}),
        'identity_digests': sorted({r['identity_digest'] for r in samples}),
    })
output = {'source_logs': [pathlib.Path(p).name for p in sys.argv[1:]], 'sample_count': len(rows), 'percentile_method': 'linear interpolation; p90/p95 withheld with fewer than 20 samples', 'groups': summary}
(base / 'benchmarks/reconstruction-representative-summary.json').write_text(json.dumps(output, indent=2) + '\n')
print(f'wrote {len(rows)} rows across {len(summary)} groups')
