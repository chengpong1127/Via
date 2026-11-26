# usage: rescale_bus_artifact.py <in .via-static.json> <out dir> <percent>
# Applies TransitNetwork::scale_bus_segment_estimates(percent) (Bus routes, non-Schedule segments, ceil(s*percent/100))
# to an artifact built with percent=100. Equivalent to `via-data build` with the same scale because the scaling runs before
# the walking-transfer and bike-access steps, which do not read segment seconds. Avoids the Valhalla-bound rebuild.
import json, sys, os
src, out, percent = sys.argv[1], sys.argv[2], int(sys.argv[3])
d = json.load(open(src))
bus = {r["id"] for r in d["transit"]["routes"] if r["mode"] == "bus"}
changed = total = 0
for p in d["transit"]["patterns"]:
    if p["route_id"] not in bus:
        continue
    for s in p["static_profile"]["segments"]:
        total += 1
        if s["source"] != "schedule":
            s["seconds"] = min(-(-s["seconds"] * percent // 100), 2**32 - 1)
            changed += 1
os.makedirs(out, exist_ok=True)
json.dump(d, open(os.path.join(out, ".via-static.json"), "w"), separators=(",", ":"), ensure_ascii=False)
print("bus segments", total, "scaled", changed)
