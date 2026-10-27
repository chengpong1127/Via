import concurrent.futures
import os
import pathlib
import resource
import statistics
import subprocess
import time

root = pathlib.Path(__file__).resolve().parent
reference = pathlib.Path(os.environ.get("VIA_REFERENCE_BINARY", "/tmp/via-phase-b-reference.bin"))
current = pathlib.Path(os.environ.get("VIA_CURRENT_BINARY", "/tmp/via-phase-b-current.bin"))
for binary in (reference, current):
    if not binary.is_file():
        raise SystemExit(f"missing benchmark binary: {binary}")


def run_io(variant):
    for label, binary, concurrency in [("reference", reference, 1)] + [
        ("current", current, count) for count in (1, 2, 4, 8)
    ]:
        destination = root / f"io-{variant}-{label}-{concurrency}.txt"
        environment = os.environ.copy()
        environment.update(VIA_PHASE_B_VARIANT=variant, VIA_PHASE_B_CONCURRENCY=str(concurrency))
        print(f"starting {destination.name}", flush=True)
        with destination.open("w") as output:
            subprocess.run([str(binary), "phase_b_io_benchmark", "--ignored", "--nocapture"],
                           env=environment, stdout=output, stderr=subprocess.STDOUT,
                           check=True, timeout=300)
        destination.write_bytes(destination.read_bytes().rstrip(b"\n") + b"\n")
        print(f"finished {destination.name}", flush=True)


def run_matrix(concurrency, raw):
    label = "raw" if raw else "shared"
    destination = root / f"matrix-{label}-{concurrency}.txt"
    environment = os.environ.copy()
    environment.pop("VIA_PHASE_B_RAW_MATRIX", None)
    environment["VIA_PHASE_B_CONCURRENCY"] = str(concurrency)
    if raw:
        environment["VIA_PHASE_B_RAW_MATRIX"] = "1"
    print(f"starting {destination.name}", flush=True)
    with destination.open("w") as output:
        subprocess.run([str(current), "phase_b_matrix_benchmark", "--ignored", "--nocapture"],
                       env=environment, stdout=output, stderr=subprocess.STDOUT,
                       check=True, timeout=400)
    destination.write_bytes(destination.read_bytes().rstrip(b"\n") + b"\n")
    print(f"finished {destination.name}", flush=True)


with concurrent.futures.ThreadPoolExecutor(max_workers=3) as workers:
    list(workers.map(run_io, ("walking", "mixed", "bike")))
with concurrent.futures.ThreadPoolExecutor(max_workers=3) as workers:
    futures = [workers.submit(run_matrix, 1, True)]
    futures.extend(workers.submit(run_matrix, count, False) for count in (1, 2, 4, 8))
    for future in futures:
        future.result()

cpu_log = root / "cpu-zero-delay.txt"
with cpu_log.open("w") as output:
    for label, binary in (("reference", reference), ("current", current)):
        samples = []
        for _ in range(10):
            before = resource.getrusage(resource.RUSAGE_CHILDREN)
            start = time.perf_counter()
            result = subprocess.run([str(binary), "phase_a_deterministic_benchmark", "--ignored", "--nocapture"],
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                    text=True, check=True, timeout=90)
            after = resource.getrusage(resource.RUSAGE_CHILDREN)
            samples.append((time.perf_counter() - start,
                            after.ru_utime - before.ru_utime,
                            after.ru_stime - before.ru_stime))
            assert "phase_a count=2400 median_us=" in result.stdout
        print(f"{label} runs=10 median_wall_s={statistics.median(x[0] for x in samples):.6f} "
              f"median_user_s={statistics.median(x[1] for x in samples):.6f} "
              f"median_system_s={statistics.median(x[2] for x in samples):.6f} "
              f"total_cpu_s={sum(x[1] + x[2] for x in samples):.6f}", file=output, flush=True)
        print(f"finished CPU measurement {label}", flush=True)
