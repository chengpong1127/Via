# Reference scenarios and external comparison

Standing rule (owner, 2026-11-24): every recommendation compares Via with Google Maps and Apple Maps on the same
origin/destination pairs, by place name (not bare coordinates), with TDX live as the ETA ground truth.

- Named set: `scenarios-named.json` (20 pairs, names + coordinates; first 10 are the old `live-scenarios.json` named pairs,
  last 10 new). `live-scenarios-named.json` is the same set in the `live_compare` input format.
- Google Maps: built-in browser, transit, leave now. First pass 2026-11-25 08:48-08:53
  (`results/maps-compare-20261125.jsonl`), time-aligned pass 09:11-10:29, each query minutes after Via planned the same pair
  (`results/maps-google-aligned-20261125.jsonl`, used in EXP-011).
- Apple Maps: **not available**. The owner's computer-use grant for Maps was denied twice on 2026-11-25
  (once for a helper, once for the researcher); not retried. Needs a grant in `request_access` to continue.
- Comparison metric: door-to-door time from "now" (Via: arrival - planned_at; Google: arrive - query time, top-listed option).
  Google's own "duration" excludes the wait for the first vehicle, so it is not used.
- Google quirks: top-listed option is often not the fastest (4 of 20); 象山->木柵 shows 6 min (5 min walk + 1 min on bus 647), unreliable.
