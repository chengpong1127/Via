"""Calibrate preprocessing.bus_derived_segment_scale_percent from TDX Bus/RealTimeNearStop.

collect <out.jsonl> [City ...]   one GET per city, 45 s apart (TDX allows 5 requests per minute per key)
analyze <artifact.json> <a2.jsonl> ...
    For each bus on duty at its 7th stop or later, compares the time since its trip started
    (GPSTime - TripStartTime) with the sum of the artifact's segment seconds up to that stop.
    Prints the median ratio overall, per city prefix and per hour; use the pooled median as the percent.

Needs TDX_CLIENT_ID and TDX_CLIENT_SECRET in the environment. Python 3 standard library only.
"""
import collections
import datetime
import json
import os
import statistics
import sys
import time
import urllib.parse
import urllib.request

TOKEN_URL = "https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token"
A2_URL = "https://tdx.transportdata.tw/api/basic/v2/Bus/RealTimeNearStop/City/{city}?%24format=JSON"
CITIES = ("Taipei", "NewTaipei")


def collect(out_path, cities):
    form = urllib.parse.urlencode(
        {
            "grant_type": "client_credentials",
            "client_id": os.environ["TDX_CLIENT_ID"],
            "client_secret": os.environ["TDX_CLIENT_SECRET"],
        }
    ).encode()
    token = json.load(urllib.request.urlopen(urllib.request.Request(TOKEN_URL, data=form), timeout=30))["access_token"]
    for index, city in enumerate(cities or CITIES):
        if index:
            time.sleep(45)
        request = urllib.request.Request(
            A2_URL.format(city=city), headers={"Authorization": "Bearer " + token, "Accept-Encoding": "identity"}
        )
        body = urllib.request.urlopen(request, timeout=60).read().decode()
        with open(out_path, "a") as out:
            out.write(json.dumps({"city": city, "status": 200, "body": body}, ensure_ascii=False) + "\n")


def analyze(artifact_path, snapshot_paths):
    network = json.load(open(artifact_path))["transit"]
    patterns = collections.defaultdict(list)
    for pattern in network["patterns"]:
        key = (pattern["id"].split("::")[0].replace("pattern:", ""), pattern["direction"])
        patterns[key].append(pattern)
    observations = []
    for path in snapshot_paths:
        for line in open(path):
            snapshot = json.loads(line)
            if snapshot["status"] != 200:
                continue
            for bus in json.loads(snapshot["body"]):
                if bus.get("DutyStatus") != 1 or not bus.get("TripStartTime"):
                    continue
                gps = datetime.datetime.fromisoformat(bus["GPSTime"])
                elapsed = gps.timestamp() - datetime.datetime.fromisoformat(bus["TripStartTime"]).timestamp()
                if not 60 < elapsed <= 3 * 3600:
                    continue
                for pattern in patterns.get((bus["SubRouteUID"], bus["Direction"]), []):
                    if bus["StopUID"] not in pattern["stops"]:
                        continue
                    stop = pattern["stops"].index(bus["StopUID"])
                    if stop >= 6 and abs(stop + 1 - bus["StopSequence"]) <= 1:
                        segments = pattern["static_profile"]["segments"][:stop]
                        sources = {s["source"] for s in segments}
                        kind = "derived" if sources <= {"derived", "estimated"} else "schedule" if sources == {"schedule"} else "mixed"
                        model = sum(s["seconds"] for s in segments)
                        observations.append((elapsed / model, bus["SubRouteUID"][:3], gps.hour, kind))
                        break
    for kind in ("schedule", "mixed"):
        rows = sorted(r for r, _, _, k in observations if k == kind)
        if rows:
            print(f"{kind} segments (not scaled): n={len(rows)} median {statistics.median(rows):.2f}")
    observations = [row for row in observations if row[3] == "derived"]
    if not observations:
        sys.exit("no usable observations")
    ratios = sorted(r for r, _, _, _ in observations)
    count = len(ratios)
    print(f"derived/estimated segments: observations {count}")
    print(f"median {statistics.median(ratios):.2f} p10 {ratios[count // 10]:.2f} p90 {ratios[9 * count // 10]:.2f}")
    for label, index in (("city prefix", 1), ("hour", 2)):
        groups = collections.defaultdict(list)
        for row in observations:
            groups[row[index]].append(row[0])
        for name in sorted(groups):
            if len(groups[name]) >= 30:
                print(f"{label} {name}: n={len(groups[name])} median {statistics.median(groups[name]):.2f}")
    print(f"suggested bus_derived_segment_scale_percent = {round(statistics.median(ratios) * 100)}")


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else ""
    if command == "collect" and len(sys.argv) >= 3:
        collect(sys.argv[2], sys.argv[3:])
    elif command == "analyze" and len(sys.argv) >= 4:
        analyze(sys.argv[2], sys.argv[3:])
    else:
        sys.exit(__doc__)
