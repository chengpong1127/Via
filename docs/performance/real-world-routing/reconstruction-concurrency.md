# Candidate reconstruction concurrency

`routing.max_concurrent_reconstruction_candidates` defaults to 4 and accepts
values from 1 through 16. It limits in-flight candidate reconstruction futures
per planning request independently of Phase 2 state concurrency. Each candidate
still processes its own movements in order. The shared Valhalla semaphore and
its timeout, including time waiting for a permit, remain unchanged. Larger
values can increase contention and timeouts; 4 is an initial setting rather than
a measured optimum.

Results are consumed in candidate input order. Cancellation drops outstanding
futures. Parallel execution can start later candidates before a request deadline
that would have stopped a sequential execution; consequently, the amount of
provider work performed before cancellation can differ. The completed
reconstruction log is emitted only after the result stream has finished.

## Measurement status

No Phase C.1 synthetic or live comparison has been collected in this checkout.
The real-world routing pilot and its harness referenced in the task are absent
from the checkout. The earlier pilot observations in the task do not establish
speedup, provider contention, timeout rates, or production latency percentiles.
A live comparison requires verified TDX quota and physical-attempt accounting,
including retries, as well as request-correlated Route and semaphore metrics.
