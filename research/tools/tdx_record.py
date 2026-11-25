# Recording pass-through proxy for TDX. usage: tdx_record.py <out.jsonl> <port>
# Forwards Via's TDX requests unchanged and appends {t, method, path, query, status, body} per response.
import json, sys, time, threading, urllib.request, urllib.error
OUT, PORT = sys.argv[1], int(sys.argv[2])
lock = threading.Lock()
UP = "https://tdx.transportdata.tw/api"
TOKEN = "https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token"
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def log_message(self, *a): pass
    def _go(self):
        n = int(self.headers.get("Content-Length") or 0)
        data = self.rfile.read(n) if n else None
        is_token = self.path.startswith("/token")
        url = TOKEN if is_token else UP + self.path
        h = {k: v for k, v in self.headers.items() if k.lower() in ("authorization", "content-type", "accept")}
        h["Accept-Encoding"] = "identity"
        req = urllib.request.Request(url, data=data, headers=h, method=self.command)
        t = time.time()
        try:
            r = urllib.request.urlopen(req, timeout=60); status, body, hdrs = r.status, r.read(), r.headers
        except urllib.error.HTTPError as e:
            status, body, hdrs = e.code, e.read(), e.headers
        except Exception as e:
            status, body, hdrs = 599, str(e).encode(), {}
        if not is_token:
            path, _, q = self.path.partition("?")
            with lock:
                open(OUT, "a").write(json.dumps({"t": t, "method": self.command, "path": path, "query": q, "status": status, "retry_after": hdrs.get("Retry-After") if hdrs else None, "body": body.decode("utf-8", "replace")}, ensure_ascii=False) + "\n")
        self.send_response(status)
        self.send_header("Content-Type", (hdrs.get("Content-Type") if hdrs else None) or "application/json")
        if hdrs and hdrs.get("Retry-After"): self.send_header("Retry-After", hdrs["Retry-After"])
        self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    do_GET = do_POST = _go
ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
