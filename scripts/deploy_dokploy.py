#!/usr/bin/env python3
"""Canonical Kurbis production rollout: Dokploy CLI plus respond5 worker sync."""
from __future__ import annotations
import fcntl, json, os, secrets, shlex, subprocess, sys, time, urllib.parse, urllib.request
from pathlib import Path

ROOT=Path(__file__).resolve().parents[1]
REMOTE=ROOT/"scripts/dokploy_worker_sync.py"
COMPOSE_ID="KZyAZl_v1hgoqWi-W77ot"; PROJECT="kurbis-hoster-fbikvd"
EXPECTED_REPO=("github","kurbis-host","kurbis","main","./compose.yaml")
ENV_KEYS={"BUILD_REVISION":None,"BEDROCK_TLS_HOST_DIRECTORY":"/srv/kurbis/certs","PANEL_BIND_ADDRESS":"127.0.0.1"}
TERMINAL={"done","error","failed","cancelled","canceled"}

class DeploymentError(RuntimeError): pass

def run(cmd, *, input_text=None, cwd=None, timeout=None):
 p=subprocess.run(cmd,input=input_text,cwd=cwd,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=False,timeout=timeout)
 if p.returncode: raise DeploymentError(f"{cmd[0]} failed: {p.stderr.strip() or p.stdout.strip()}")
 return p.stdout.strip()

def credentials():
 url=os.getenv("DOKPLOY_URL"); token=os.getenv("DOKPLOY_API_KEY") or os.getenv("DOKPLOY_AUTH_TOKEN")
 if not url or not token:
  cli=Path(run(["which",os.getenv("DOKPLOY_CLI","dokploy")])).resolve(); cfg=json.loads((cli.parent.parent/"config.json").read_text())
  url,token=cfg.get("url"),cfg.get("token")
 if not url or not token: raise DeploymentError("Dokploy CLI auth is unavailable")
 return url.rstrip("/"),token

def api(endpoint, params, method="GET"):
 base,token=credentials(); body=json.dumps({"json":params},separators=(",",":")).encode()
 headers={"x-api-key":token,"User-Agent":"axios/1.8.4","Content-Type":"application/json"}
 if method=="GET":
  url=f"{base}/api/trpc/{endpoint}?{urllib.parse.urlencode({'input':body.decode()})}"; req=urllib.request.Request(url,headers=headers)
 else: req=urllib.request.Request(f"{base}/api/trpc/{endpoint}",data=body,headers=headers,method=method)
 try:
  result=json.loads(urllib.request.urlopen(req,timeout=30).read())
 except Exception as e: raise DeploymentError(f"Dokploy API failed: {endpoint}") from e
 if "error" in result: raise DeploymentError(f"Dokploy rejected {endpoint}: {result['error']}")
 return result.get("result",{}).get("data",{}).get("json",result)

def revision():
 head=run(["git","-C",str(ROOT),"rev-parse","HEAD"])
 if len(head)!=40: raise DeploymentError("release SHA is not full")
 if run(["git","-C",str(ROOT),"status","--porcelain","--untracked-files=all"]): raise DeploymentError("release checkout is dirty")
 if run(["git","-C",str(ROOT),"ls-remote","origin","refs/heads/main"]).split()[0]!=head: raise DeploymentError("HEAD differs from origin/main")
 return head

def input_hashes(commit):
 return {path:run(["git","-C",str(ROOT),"rev-parse",f"{commit}:{path}"]) for path in run(["git","-C",str(ROOT),"ls-tree","-r","--name-only",commit]).splitlines()}

def env_parse(text):
 values={}
 for line in text.splitlines():
  if "=" in line and not line.lstrip().startswith("#"):
   k,v=line.split("=",1); values[k]=v.strip().strip("\"'")
 return values

def configure(commit):
 data=api("compose.one",{"composeId":COMPOSE_ID})
 if not isinstance(data,dict) or data.get("composeId")!=COMPOSE_ID or data.get("appName")!=PROJECT: raise DeploymentError("wrong Dokploy production app")
 if (data.get("sourceType"), data.get("owner"), data.get("repository"), data.get("branch"), data.get("composePath")) != EXPECTED_REPO: raise DeploymentError("Dokploy source config mismatch")
 if data.get("environment",{}).get("name")!="production": raise DeploymentError("Dokploy environment mismatch")
 if data.get("autoDeploy") is not False: api("compose.update",{"composeId":COMPOSE_ID,"autoDeploy":False},"POST")
 env=env_parse(str(data.get("env",""))); ENV_KEYS["BUILD_REVISION"]=commit; env.update(ENV_KEYS)
 api("compose.saveEnvironment",{"composeId":COMPOSE_ID,"env":"\n".join(f"{k}={v}" for k,v in env.items())+"\n"},"POST")
 data=api("compose.one",{"composeId":COMPOSE_ID}); final=env_parse(str(data.get("env","")))
 if data.get("autoDeploy") is not False or any(final.get(k)!=v for k,v in ENV_KEYS.items()): raise DeploymentError("Dokploy release environment did not persist")

def deployments(): return api("deployment.allByCompose",{"composeId":COMPOSE_ID})

def remote(op, token=None, rev=None, expected=None, baseline=None, pumpkin=None, receipt=None):
 cmd=["python3","-","--remote",op]
 if token:cmd+=['--token',token]
 if rev:cmd+=['--revision',rev]
 if expected is not None:cmd+=['--expected',json.dumps(expected,separators=(",",":"))]
 if baseline is not None:cmd+=['--baseline',json.dumps(baseline,separators=(",",":"))]
 if pumpkin:cmd+=['--pumpkin',pumpkin]
 if receipt is not None:cmd+=['--receipt',json.dumps(receipt,separators=(",",":"))]
 out=run(["ssh","-o","BatchMode=yes","-o","ConnectTimeout=10","respond5",shlex.join(cmd)],input_text=REMOTE.read_text(),timeout=1800)
 try:return json.loads(out)
 except json.JSONDecodeError as e:raise DeploymentError("respond5 verifier returned invalid JSON") from e

def deploy_cli(commit):
 prior=deployments(); prior_ids={str(x.get("deploymentId")) for x in prior if isinstance(x,dict)}
 if any(str(x.get("status","")).lower() not in TERMINAL for x in prior):raise DeploymentError("Dokploy deployment already running")
 queued=json.loads(run(["dokploy","compose","deploy","--composeId",COMPOSE_ID,"--title",f"Deploy {commit[:12]}","--description",f"Deploy exact origin/main revision {commit}","--json"],timeout=60))
 if queued.get("success") is not True:raise DeploymentError("Dokploy did not queue deployment")
 deadline=time.monotonic()+900
 while time.monotonic()<deadline:
  current=[x for x in deployments() if str(x.get("deploymentId")) not in prior_ids]
  if len(current)>1:raise DeploymentError("superseding Dokploy deployment detected")
  if current:
   d=current[0]; status=str(d.get("status","")).lower()
   if commit not in str(d.get("description","")):raise DeploymentError("deployment SHA provenance mismatch")
   if status in {"error","failed","cancelled","canceled"}:raise DeploymentError(f"Dokploy deployment failed: {status}")
   if status=="done":
    logs=api("deployment.readLogs",{"deploymentId":d["deploymentId"],"tail":4000})
    if "Docker Compose Deployed:" not in str(logs) or "target=/source/target" in str(logs):raise DeploymentError("Dokploy build log failed")
    return str(d["deploymentId"]),prior_ids
  time.sleep(5)
 raise DeploymentError("Dokploy deployment timeout")

def main():
 token=secrets.token_hex(16); locked=False; staged=False
 try:
  commit=revision(); expected=input_hashes(commit)
  run(["python3","-m","unittest","discover","-s","tools","-p","test_dokploy_worker_rollout.py"],cwd=ROOT,timeout=300)
  run(["cargo","test","--locked","--test","first_join_flow","--","--test-threads=1"],cwd=ROOT,timeout=900)
  remote("lock",token=token);locked=True
  configure(commit)
  base=remote("baseline")
  remote("stage",token=token,rev=commit,expected=expected);staged=True
  pumpkin=remote("build-pumpkin",rev=commit,expected=expected)["pumpkin_image"]
  deployment_id,prior=deploy_cli(commit)
  receipt=remote("wait",rev=commit,pumpkin=pumpkin,baseline=base["snapshot"],expected=expected)
  if receipt.get("status")!="verified":raise DeploymentError("respond5 runtime verification failed")
  receipt.update(status="verified",source_revision=commit,deployment_id=deployment_id)
  remote("mark",receipt=receipt)
  print(json.dumps(receipt,sort_keys=True)); return 0
 except (DeploymentError,subprocess.TimeoutExpired,OSError) as e:
  print(f"deployment not accepted: {e}",file=sys.stderr); return 1
 finally:
  if staged:
   try:remote("restore",token=token)
   except Exception as e:print(f"checkout restore failed: {e}",file=sys.stderr)
  if locked:
   try:remote("unlock",token=token)
   except Exception as e:print(f"remote lock retained: {e}",file=sys.stderr)

if __name__=="__main__":raise SystemExit(main())
