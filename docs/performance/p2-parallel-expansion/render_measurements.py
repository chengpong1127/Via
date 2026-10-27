import pathlib

root = pathlib.Path(__file__).resolve().parent

def rows(path, prefix):
    return [dict(token.split('=', 1) for token in line.split()[1:] if '=' in token)
            for line in path.read_text().splitlines() if line.startswith(prefix + ' ')]

def lookup(items, **fields):
    found = [item for item in items if all(item.get(key) == str(value) for key, value in fields.items())]
    if len(found) != 1:
        raise ValueError((fields, len(found)))
    return found[0]

def duration(item):
    return f"{int(item['p50_us']) / 1000:.2f} / {int(item['p95_us']) / 1000:.2f}"

def improvement(item, reference, field):
    base = int(reference[field])
    return 0.0 if base == 0 else (base - int(item[field])) * 100.0 / base

io = {}
for variant in ('walking', 'mixed', 'bike'):
    for label, concurrency in [('reference', 1)] + [('current', c) for c in (1, 2, 4, 8)]:
        io[variant, label, concurrency] = rows(root / f'io-{variant}-{label}-{concurrency}.txt', 'phase_b')
    for delay in (0, 10, 50, 100):
        for case in ('success', 'unreachable', 'timeout'):
            reference = lookup(io[variant, 'reference', 1], delay=delay, case=case)
            for concurrency in (1, 2, 4, 8):
                current = lookup(io[variant, 'current', concurrency], delay=delay, case=case)
                for field in ('fingerprints', 'successors', 'candidates'):
                    if reference[field] != current[field]:
                        raise ValueError((variant, delay, case, concurrency, field, reference[field], current[field]))

matrix = {('raw', 1): rows(root / 'matrix-raw-1.txt', 'matrix')}
for concurrency in (1, 2, 4, 8):
    matrix['shared', concurrency] = rows(root / f'matrix-shared-{concurrency}.txt', 'matrix')

lines = [
    '# Release measurements', '',
    'Measured on the same workspace host using release binaries. Each case has two warmups and 20 measured iterations; p50, p95 and worst are observed wall times. The provider is deterministic and sleeps asynchronously for each Matrix call; its semaphore permits eight physical operations. Three independent benchmark processes ran concurrently, one per workload, so timings include host scheduling noise. No live TDX or Valhalla traffic was used.', '',
    'The reference binary was built from the original Phase A commit with only the benchmark fixture added. The current binary uses the request-local resolver and the indicated expansion concurrency. The fixture evaluates a transit corridor with a resolving walking egress; the mixed and BikeShare variants use the real BikeShare planner and real asynchronous walking and bicycle Matrix mock calls. The BikeShare variant has an empty transit snapshot. These are deterministic stress fixtures, not production latency estimates.', '',
    '## Phase 2 latency', '',
    'The table shows p50 / p95 in milliseconds for successful queries with a 50 ms provider delay. Physical calls are mock provider operations per evaluation.', '',
    '| Workload | Phase A | New 1 | New 2 | New 4 | New 8 | Calls: Phase A → new | Max active provider operations |',
    '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |',
]
for variant in ('walking', 'mixed', 'bike'):
    ref = lookup(io[variant, 'reference', 1], delay=50, case='success')
    vals = [lookup(io[variant, 'current', c], delay=50, case='success') for c in (1, 2, 4, 8)]
    calls = ', '.join(f"{c}:{v['physical']}" for c, v in zip((1,2,4,8), vals))
    peak = max(int(v['peak']) for v in vals)
    lines.append(f"| {variant} | {duration(ref)} | " + ' | '.join(duration(v) for v in vals) + f" | {ref['physical']} → {calls} | {peak} |")
lines += ['', 'P50 / p95 changes relative to Phase A (positive is faster):', '',
          '| Workload | New 1 | New 2 | New 4 | New 8 |', '| --- | ---: | ---: | ---: | ---: |']
for variant in ('walking', 'mixed', 'bike'):
    ref = lookup(io[variant, 'reference', 1], delay=50, case='success')
    vals = [lookup(io[variant, 'current', c], delay=50, case='success') for c in (1,2,4,8)]
    lines.append(f'| {variant} | ' + ' | '.join(f"{improvement(v, ref, 'p50_us'):.1f}% / {improvement(v, ref, 'p95_us'):.1f}%" for v in vals) + ' |')
lines += ['', 'Failure and unreachable cases at 50 ms:', '',
          '| Workload / result | Phase A p50 / p95 ms | New 4 p50 / p95 ms | Calls: Phase A → new 4 | Candidates |',
          '| --- | ---: | ---: | ---: | ---: |']
for variant in ('walking', 'mixed', 'bike'):
    for case in ('unreachable', 'timeout'):
        ref = lookup(io[variant, 'reference', 1], delay=50, case=case)
        cur = lookup(io[variant, 'current', 4], delay=50, case=case)
        lines.append(f"| {variant} / {case} | {duration(ref)} | {duration(cur)} | {ref['physical']} → {cur['physical']} | {cur['candidates']} |")
lines += ['', 'Zero-delay successful cases (p50 / p95 in microseconds):', '',
          '| Workload | Phase A | New 1 | New 2 | New 4 | New 8 |',
          '| --- | ---: | ---: | ---: | ---: | ---: |']
for variant in ('walking', 'mixed', 'bike'):
    reference = lookup(io[variant, 'reference', 1], delay=0, case='success')
    values = [lookup(io[variant, 'current', c], delay=0, case='success') for c in (1, 2, 4, 8)]
    micros = lambda value: f"{value['p50_us']} / {value['p95_us']}"
    lines.append(f"| {variant} | {micros(reference)} | " + ' | '.join(micros(value) for value in values) + ' |')
lines += ['', 'All 0, 10, 50 and 100 ms cases, including failures, produced identical ordered journey fingerprints, successor counts and candidate counts across Phase A and all new concurrency settings. The integration test separately compares reconstructed ordered Top-3 results at 1, 2, 4 and 8. The raw `io-*.txt` files include every delay, p50, p95, worst, logical requests, physical calls, completed/in-flight hits, provider waiting and fingerprints.', '',
          '## Exact-query reuse and provider contention', '',
          'The following separate Matrix microbenchmark runs ten logical queries per request at 50 ms. Identical queries use one exact key; distinct queries vary their target coordinates. Multiple requests have independent caches but share one mock provider semaphore. Raw bypasses the new resolver; it is a Matrix-layer comparison and does not model the old movement caches.', '',
          '| Workload | Resolver / state limit | p50 / p95 ms | Logical / physical calls | Completed / in-flight hits | Peak active | Wait µs |',
          '| --- | --- | ---: | ---: | ---: | ---: | ---: |']
for identical, requests in ((True,1),(False,1),(False,4),(True,4)):
    for label,c in [('raw',1),('shared',1),('shared',2),('shared',4),('shared',8)]:
        v=lookup(matrix[label,c], delay=50, identical=str(identical).lower(), requests=requests)
        title=f"{'identical' if identical else 'distinct'}, {requests} request{'s' if requests > 1 else ''}"
        lines.append(f"| {title} | {label} / {c} | {duration(v)} | {v['logical']} / {v['physical']} | {v['completed_hits']} / {v['inflight_hits']} | {v['peak']} | {v['wait_us']} |")
raw_identical = lookup(matrix['raw', 1], delay=50, identical='true', requests=1)
lines += ['', 'The mock peak is counted after semaphore acquisition; it is independent of the resolver’s delegated-operation peak. `wait_us` is the accumulated semaphore wait in the final iteration, not a percentile. Waiting and overlap can vary by scheduling. Identical queries eliminate nine of ten provider operations per request; separate requests do not share results.', '',
          f"The raw identical-query series had a {int(raw_identical['p95_us']) / 1_000_000:.2f} s p95 and a {int(raw_identical['worst_us']) / 1_000_000:.2f} s worst case despite a 50 ms simulated delay. These host scheduling outliers make its latency comparison unreliable; the operation counts remain deterministic.", '',
          '## Zero-delay CPU', '',
          'The CPU measurement uses ten separate runs of the retained zero-delay Phase A stress benchmark, including process startup and both its 32- and 2,400-corridor fixtures. It exercises mostly completed Walking cache hits, so it is a coarse CPU overhead indicator rather than the asynchronous-provider workload above. CPU time is measured from child-process user and system time.', '',
          '```text']
cpu_lines = (root/'cpu-zero-delay.txt').read_text().splitlines()
lines.extend(cpu_lines)
cpu = {line.split()[0]: dict(token.split('=', 1) for token in line.split()[1:]) for line in cpu_lines}
ratio = float(cpu['current']['total_cpu_s']) / float(cpu['reference']['total_cpu_s'])
lines += ['```', '', f'Total measured CPU time for this stress test increased by {(ratio - 1) * 100:.0f}% ({ratio:.2f}×). This includes process startup and is specific to this mostly cached fixture.', '', '## Reproduction', '',
          'Run `python3 docs/performance/p2-parallel-expansion/prepare_reference.py` to create a temporary reference checkout and add the fixture; it prints the reference manifest path. Run `cargo test --release --manifest-path <reference-manifest> --lib --no-run` and copy the executable path printed by Cargo to `/tmp/via-phase-b-reference.bin`. Run `cargo test --release --manifest-path server/Cargo.toml --lib --no-run` and copy its printed executable to `/tmp/via-phase-b-current.bin`. Then run:', '',
          '```bash',
          'python3 docs/performance/p2-parallel-expansion/run_comparison.py',
          'python3 docs/performance/p2-parallel-expansion/render_measurements.py',
          '```', '',
          'The comparison script accepts `VIA_REFERENCE_BINARY` and `VIA_CURRENT_BINARY` to select other paths. It limits concurrent benchmark processes to three and runs the CPU measurements separately. The original pre-change release logs are also retained as `phase-a-*.txt` and `io-baseline.txt`.', '']
(root/'measurements.md').write_text('\n'.join(lines))
print(root/'measurements.md')
