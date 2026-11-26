import json,sys,urllib.request,time
def plan(olat,olon,dlat,dlon,base="http://127.0.0.1:3000",modes=("bus","metro","tra","thsr","bike_share"),at=None,label=("O","D")):
    dep={"type":"depart_now"} if not at else {"type":"depart_at","time":at}
    body={"origin":{"coordinate":{"latitude":olat,"longitude":olon},"display_text":label[0]},
          "destination":{"coordinate":{"latitude":dlat,"longitude":dlon},"display_text":label[1]},
          "selected_modes":list(modes),"departure_setting":dep}
    req=urllib.request.Request(base+"/v1/journey-plans",json.dumps(body).encode(),{"Content-Type":"application/json"})
    t=time.time()
    try:
        r=urllib.request.urlopen(req,timeout=60); d=json.load(r)
    except urllib.error.HTTPError as e:
        return {"error":e.code,"body":e.read()[:300].decode()},time.time()-t
    return d,time.time()-t
def summ(d):
    out=[]
    for o in d.get("journey_options",[]):
        segs=[]
        for l in o["legs"]:
            m=l["mode"]; mins=round(l["duration_seconds"]/60); det=l["details"]; t=det.get("type")
            if t=="walking": segs.append(f"walk {mins}")
            elif t=="bike_share": segs.append(f"bike {mins}")
            else:
                est="~" if det.get("estimated") else ""
                segs.append(f"{det['transit_mode']}:{det['service_label']}{est} {mins} [{l['from']['display_name']}>{l['to']['display_name']} @{l['departure_time'][11:16]}]")
        out.append(f"{round(o['duration_seconds']/60)}m dep {o['departure_time'][11:16]} walk {round(o['walking_seconds']/60)} | "+" > ".join(segs))
    return out
if __name__=="__main__":
    a=list(map(float,sys.argv[1:5]))
    d,t=plan(*a)
    print(f"{t:.1f}s", d.get("error") or d.get("reason") or "")
    if "journey_options" in d: print("\n".join(summ(d)))
    else: print(json.dumps(d)[:500])
    if len(sys.argv)>5: json.dump(d,open(sys.argv[5],"w"))
