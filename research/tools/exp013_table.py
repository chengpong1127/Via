# EXP-013: replay results on the scale-290 artifact vs the earlier recorded A/B and the Google column.
# usage: exp013_table.py <results dir> <replay-a.jsonl> <replay-b.jsonl> <replay-c.jsonl>
# Compares, per scenario: old live base/est (artifact scale 100), old replay est on Bus x2.5 (a, b only), new est/noest (scale 290),
# and the Google door-to-door gap. Arrival minutes are measured from the pinned planning time.
import json, re, sys, datetime, statistics as st

R = sys.argv[1].rstrip("/") + "/"
REPLAYS = dict(zip("abc", sys.argv[2:5]))
LIVE = {"a": "exp011-live-20261125.jsonl", "b": "exp011b-live.jsonl", "c": "exp011c-live.jsonl"}
BUS25 = {"a": "exp011-replay-bus25-20261125.jsonl", "b": "exp011b-replay-bus25-20261125.jsonl"}
GOOGLE = {"a": "maps-google-aligned-20261125.jsonl", "b": "maps-google-aligned-b-20261125.jsonl", "c": "maps-google-aligned-c-20261125.jsonl"}


def load(path, key):
    out = {}
    for line in open(path):
        row = json.loads(line)
        out[row[key]] = row
    return out


def epoch(text):
    m = re.match(r"(\d+)-(\d+)-(\d+) (\d+):(\d+):(\d+)", text)
    y, mo, d, h, mi, s = map(int, m.groups())
    return datetime.datetime(y, mo, d, h, mi, s, tzinfo=datetime.timezone.utc).timestamp()


def minutes(clock):
    return int(clock[:2]) * 60 + int(clock[3:5])


def first(options):
    return options[0] if isinstance(options, list) and options else None


def duration(row, variant):
    option = first(row.get(variant))
    if not option:
        return None
    return (option["arrival"] - epoch(row["planned_at"])) / 60


rows = []
for key in "abc":
    live = load(R + "results/" + LIVE[key], "name")
    new = load(REPLAYS[key], "name")
    old25 = load(R + "results/" + BUS25[key], "name") if key in BUS25 else {}
    google = load(R + "results/" + GOOGLE[key], "id")
    for name, row in new.items():
        g = google.get(name)
        google_min = minutes(g["arrive"]) - minutes(g["query_time"]) if g else None
        rows.append(
            {
                "set": key,
                "name": name,
                "live_est": duration(live[name], "est"),
                "live_base": duration(live[name], "base"),
                "x25_est": duration(old25[name], "est") if name in old25 else None,
                "x25_base": duration(old25[name], "base") if name in old25 else None,
                "est": duration(row, "est"),
                "noest": duration(row, "noest"),
                "google": google_min,
            }
        )


def fmt(value):
    return "-" if value is None else f"{value:.0f}"


print("| set | scenario | scale 100 est | x2.5 est | scale 290 noest | scale 290 est | Google | gap (290 est - Google) |")
print("| --- | --- | --- | --- | --- | --- | --- | --- |")
for r in rows:
    gap = None if r["google"] is None or r["est"] is None else r["est"] - r["google"]
    print(f"| {r['set']} | {r['name']} | {fmt(r['live_est'])} | {fmt(r['x25_est'])} | {fmt(r['noest'])} | {fmt(r['est'])} | {fmt(r['google'])} | {'-' if gap is None else format(gap, '+.0f')} |")


def summary(label, select, est_key, base_key):
    gaps = [r[est_key] - r["google"] for r in rows if select(r) and r[est_key] is not None and r["google"] is not None]
    faster = sum(g < -10 for g in gaps)
    within = sum(abs(g) <= 10 for g in gaps)
    wins = [r for r in rows if select(r) and r[est_key] is not None and r[base_key] is not None and r[base_key] - r[est_key] > 0.5]
    later = [r for r in rows if select(r) and r[est_key] is not None and r[base_key] is not None and r[est_key] - r[base_key] > 0.5]
    print(
        f"{label}: n={len(gaps)} median gap {st.median(gaps):+.1f} min, mean {st.mean(gaps):+.1f}, "
        f"Via >10 min faster than Google {faster}, within 10 min {within}; est earlier than base by >30 s: {len(wins)}, later: {len(later)}"
    )


every = lambda r: True
summary("scale 100 live (a+b+c)", every, "live_est", "live_base")
summary("scale 290 replay (a+b+c)", every, "est", "noest")
both = lambda r: r["set"] in "ab"
summary("scale 100 live (a+b)", both, "live_est", "live_base")
summary("x2.5 replay (a+b)", both, "x25_est", "x25_base")
summary("scale 290 replay (a+b)", both, "est", "noest")
