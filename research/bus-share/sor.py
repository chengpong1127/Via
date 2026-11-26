import re,sys,json,urllib.parse,collections
sys.path.insert(0,'.')
import tdx,cover
def norm(x): return re.sub(r'\(.*?\)','',x).replace('捷運','').replace('站','').strip()
legs={}
sec=None
for line in open('google_log.txt'):
    if line.startswith('##'): sec=line[3:].split()[0]; continue
    for m in re.finditer(r'B\[(.*?)\] (\S*?)>(\S+?) \d+m',line):
        lab,f,t=m.groups(); legs.setdefault((sec,lab,f,t),None)
cache={}
def stop_of_route(label):
    for k in range(len(label),0,-1):
        pre=label[:k]
        if not re.fullmatch(r'[0-9A-Za-z一-鿿]+',pre): continue
        if pre in cache: r=cache[pre]
        else:
            r=None
            for city in ('NewTaipei','Taipei'):
                q=tdx.get(f"v2/Bus/StopOfRoute/City/{city}/{urllib.parse.quote(pre)}?$select=RouteName,Direction,Stops&$format=JSON")
                if isinstance(q,list) and q: r=[x for x in q if x['RouteName']['Zh_tw']==pre]; 
                if r: break
            cache[pre]=r
        if r: return pre,r
    return None,None
res=collections.Counter(); rows=[]
for (sec,lab,f,t),_ in legs.items():
    before=cover.check(lab,None if f=='?' else f,t)
    pre,r=stop_of_route(lab)
    after='NO-TDX'
    if r:
        after='NOT-FOUND'
        for x in r:
            names=[norm(s['StopName']['Zh_tw']) for s in x['Stops']]
            fi=[i for i,n in enumerate(names) if f!='?' and (norm(f)==n or norm(f) in n or n in norm(f))]
            ti=[i for i,n in enumerate(names) if (norm(t)==n or norm(t) in n or n in norm(t))]
            if fi and ti and min(fi)<max(ti): after='OK-with-StopOfRoute'; break
            if not fi and f=='?' and ti: after='OK?(from unknown)'
    res[(before=='OK',after)]+=1
    rows.append((sec,lab,f,t,before,pre,after))
for r in rows: print(r)
print(dict(res))
