import lldb,time,json

def sample(debugger, command, result, internal_dict):
    pid=int(command.strip()); debugger.SetAsync(True)
    target=debugger.CreateTarget(None); error=lldb.SBError()
    proc=target.AttachToProcessWithID(debugger.GetListener(),pid,error)
    if not error.Success(): print('ATTACH ERROR',error); return
    out=open('/tmp/via-d3-lldb-samples.jsonl','w')
    proc.Continue()
    for i in range(600):
        time.sleep(.1)
        if proc.GetState() in (lldb.eStateExited,lldb.eStateDetached): break
        err=proc.Stop()
        if not err.Success(): break
        time.sleep(.005)
        rows=[]
        for t in proc:
            frames=[]
            for f in t:
                frames.append(f.GetFunctionName() or f.GetSymbol().GetName() or '?')
            rows.append(frames)
        with open('/tmp/via-d3-sampled.txt') as log:
            lines=log.read().splitlines()
        stage=next((x for x in reversed(lines) if x.startswith('P2 bike=') or x.startswith('CAPTURE ')), '')
        out.write(json.dumps({'stage':stage,'threads':rows})+'\n'); out.flush()
        if not proc.Continue().Success(): break
    proc.Detach(); out.close()

def __lldb_init_module(debugger, internal_dict):
    debugger.HandleCommand('command script add -f via_lldb_sample.sample samplevia')
