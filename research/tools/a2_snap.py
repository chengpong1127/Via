# EXP-012: snapshot TDX Bus RealTimeNearStop (A2) per city, 4 rounds, ~45 s between requests (<= 1.4 req/min).
import json, os, sys, time, urllib.request, urllib.parse
env = dict(l.strip().split("=", 1) for l in open("/Users/chengpong/Documents/Via/server/.env") if "=" in l and not l.startswith("#"))
cid = next(v for k, v in env.items() if k.endswith("CLIENT_ID")); sec = next(v for k, v in env.items() if k.endswith("CLIENT_SECRET"))
tok = json.load(urllib.request.urlopen(urllib.request.Request("https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token",
      data=urllib.parse.urlencode({"grant_type": "client_credentials", "client_id": cid, "client_secret": sec}).encode(), method="POST"), timeout=30))["access_token"]
OUT = sys.argv[1]
for rnd in range(4):
    for city in ("Taipei", "NewTaipei"):
        t = time.time()
        req = urllib.request.Request(f"https://tdx.transportdata.tw/api/basic/v2/Bus/RealTimeNearStop/City/{city}?%24format=JSON",
              headers={"Authorization": "Bearer " + tok, "Accept-Encoding": "identity"})
        try: body = urllib.request.urlopen(req, timeout=60).read().decode(); st = 200
        except Exception as e: body = str(e); st = getattr(e, "code", 599)
        open(OUT, "a").write(json.dumps({"t": t, "city": city, "round": rnd, "status": st, "body": body}, ensure_ascii=False) + "\n")
        print(rnd, city, st, len(body), flush=True)
        time.sleep(45)
