Prototype and measurement scripts for issue #66 (why Via rarely offers bus plans). Throwaway; never merge.
Paths inside the scripts point at the researcher's scratchpad and the shared `server/data/build` dir; edit them to rerun.
- `plan.py`, `runvia.py`, `pairs.py`: plan against the local server (`/v1/journey-plans`) and print compact summaries.
- `vr.sh` + `rk.py` + `corr.py`: `via-route --trace` run, retained corridors, and the ranking dump (needs the `VIA_DUMP_RANK` patch in `server/src/domain/ranking.rs` on this branch).
- `cover.py`, `sor.py`, `google_log.txt`, `gscrape.js`: Google Maps bus legs versus Via's static network (and versus TDX StopOfRoute).
- `eta_near.py`, `live_cov.py`: live TDX ETA rows near an origin versus static patterns (experiment key via `tdx.py`; never print the key).
- `nodead.py`: prototype fix 1 (drop non-bus patterns with no trips or frequencies) as an artifact rewrite.
- `inject.py`: prototype fix 2 (add the missing direction of each Taipei/New Taipei route from TDX StopOfRoute, derived seconds).
- `rp.sh`, `rpx.sh`, `b28.py`, `st.py`: daytime replay of the EXP-013 recordings (sets a, b, c) on variant artifacts/configs and the B28 table.
