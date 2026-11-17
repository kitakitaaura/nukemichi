import json,sys
t=sys.stdin.read()
try:
  d=json.loads(t[t.index("{"):]); r=d.get("results",{}) or {}
  print(d["browser"], d["transport"], "boot=%.0fms"%(d.get("boot") or {}).get("bootstrapMs",-1), "IsTor=",(r.get("checkTorproject") or {}).get("IsTor"), "| plain:",r.get("plainFetch"), "| tor:",r.get("torFetch"), "| tcp:",r.get("tcpEcho"), "| err:",d.get("error"), (d.get("tail") or "")[-1200:])
except Exception as ex: print("PARSE", ex, t[-1500:])
