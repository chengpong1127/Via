import os,json,time,urllib.request,urllib.parse,subprocess,fcntl
ENV='/Users/chengpong/Documents/Via/server/.env.experiment'
TOK='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/.tok'
LAST='/private/tmp/claude-501/-Users-chengpong--claude-spares-spare-3iGH7G/bf7b6f26-03b4-4dfe-ab20-0e635a962ce6/scratchpad/.last'
def env():
    e={}
    for l in open(ENV):
        l=l.strip()
        if '=' in l and not l.startswith('#'):
            k,v=l.split('=',1); e[k.replace('export ','')]=v.strip('"\'')
    return e
def token():
    try:
        t=json.load(open(TOK))
        if t['exp']>time.time()+300: return t['tok']
    except Exception: pass
    e=env()
    data=urllib.parse.urlencode({'grant_type':'client_credentials','client_id':e['TDX_CLIENT_ID'],'client_secret':e['TDX_CLIENT_SECRET']}).encode()
    r=json.load(urllib.request.urlopen('https://tdx.transportdata.tw/auth/realms/TDXConnect/protocol/openid-connect/token',data,timeout=20))
    json.dump({'tok':r['access_token'],'exp':time.time()+r['expires_in']},open(TOK,'w')); os.chmod(TOK,0o600)
    return r['access_token']
def get(path,min_gap=0.4):
    try: last=float(open(LAST).read())
    except Exception: last=0
    w=last+min_gap-time.time()
    if w>0: time.sleep(w)
    open(LAST,'w').write(str(time.time()))
    req=urllib.request.Request('https://tdx.transportdata.tw/api/basic/'+path,headers={'Authorization':'Bearer '+token(),'Accept':'application/json'})
    try:
        r=urllib.request.urlopen(req,timeout=30); return json.load(r)
    except urllib.error.HTTPError as ex:
        return {'_err':ex.code,'retry':ex.headers.get('Retry-After')}
