# Replay server for TDX responses recorded by tdx_record.py.
# usage: tdx_replay.py <record.jsonl> <vt_file> <port> [miss_log]
# Serves, for the virtual time in <vt_file> (unix seconds of the scenario's departure), the responses recorded
# in [vt-5, vt+300]. Exact (path, query) match first; Bus ETA requests are answered from the union of that
# window's records for the same city, filtered locally by the requested StopUID list (so changed fan-out
# still finds data). Anything else unmatched returns [] and is logged as a miss.
import json, re, sys, urllib.parse, threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
REC, VT, PORT = sys.argv[1], sys.argv[2], int(sys.argv[3])
MISS = sys.argv[4] if len(sys.argv) > 4 else None
recs = [json.loads(l) for l in open(REC)]
lock = threading.Lock()
def window(vt):
    return [r for r in recs if vt - 5 <= r["t"] <= vt + 300 and r["status"] == 200]
def stops(q):
    f = urllib.parse.parse_qs(q).get("$filter", [""])[0]
    return set(re.findall(r"'([^']+)'", f)) if "StopUID" in f else None
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def log_message(self, *a): pass
    def _send(self, status, body):
        b = body.encode()
        self.send_response(status); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def _go(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n: self.rfile.read(n)
        if self.path.startswith("/token"):
            return self._send(200, '{"access_token":"replay","expires_in":3600}')
        path, _, q = self.path.partition("?")
        vt = int(open(VT).read().strip())
        w = window(vt)
        for r in w:
            if r["path"] == path and r["query"] == q:
                return self._send(200, r["body"])
        want = stops(q)
        if want is not None and "Bus/EstimatedTimeOfArrival" in path:
            seen, out = set(), []
            for r in w:
                if r["path"] != path: continue
                try: arr = json.loads(r["body"])
                except Exception: continue
                for a in arr:
                    if a.get("StopUID") in want:
                        k = (a.get("StopUID"), a.get("SubRouteUID"), a.get("Direction"))
                        if k not in seen: seen.add(k); out.append(a)
            if any(r["path"] == path for r in w):
                with lock:
                    if MISS: open(MISS, "a").write(json.dumps({"vt": vt, "kind": "merged", "path": path, "n": len(out)}) + "\n")
                return self._send(200, json.dumps(out, ensure_ascii=False))
        if "Metro/LiveBoard" in path:
            f = urllib.parse.parse_qs(q).get("$filter", [""])[0]
            wantp = set(re.findall(r"LineID eq '([^']+)' and StationID eq '([^']+)'", f))
            out = []
            for r in w:
                if r["path"] != path: continue
                try: arr = json.loads(r["body"])
                except Exception: continue
                for a in arr:
                    if (a.get("LineID"), a.get("StationID")) in wantp and a not in out: out.append(a)
            if any(r["path"] == path for r in w):
                return self._send(200, json.dumps(out, ensure_ascii=False))
        if "TrainLiveBoard" in path:
            f = urllib.parse.parse_qs(q).get("$filter", [""])[0]
            wantn = set(re.findall(r"TrainNo eq '([^']+)'", f))
            out, seen = [], set()
            for r in w:
                if r["path"] != path: continue
                try: body = json.loads(r["body"])
                except Exception: continue
                arr = body.get("TrainLiveBoards", body) if isinstance(body, dict) else body
                for a in arr:
                    if a.get("TrainNo") in wantn and a.get("TrainNo") not in seen: seen.add(a["TrainNo"]); out.append(a)
            if any(r["path"] == path for r in w):
                tmpl = next(json.loads(r["body"]) for r in w if r["path"] == path)
                if isinstance(tmpl, dict): tmpl = dict(tmpl); tmpl["TrainLiveBoards"] = out; return self._send(200, json.dumps(tmpl, ensure_ascii=False))
                return self._send(200, json.dumps(out, ensure_ascii=False))
        with lock:
            if MISS: open(MISS, "a").write(json.dumps({"vt": vt, "kind": "miss", "path": path, "q": q[:200]}) + "\n")
        self._send(200, "[]")
    do_GET = do_POST = _go
ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
