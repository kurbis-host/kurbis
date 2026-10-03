#!/usr/bin/env python3
"""Root-only respond5 verifier used by the Kurbis Dokploy release wrapper."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import tempfile
import time
import uuid
from pathlib import Path
from typing import Sequence

ROOT = Path("/srv/kurbis")
CHECKOUT = Path("/etc/dokploy/compose/kurbis-hoster-fbikvd/code")
ENV = CHECKOUT / ".env"
WORKER = Path("/srv/kurbis-hoster/target/release/worker")
WORKER_ENV = ROOT / "worker.env"
WORKER_UNIT = Path("/etc/systemd/system/kurbis-worker.service")
WORKER_LAUNCHER = Path("/usr/local/bin/kurbis-worker")
WORKER_SERVICE = "kurbis-worker.service"
PROJECT = "kurbis-hoster-fbikvd"
SERVICES = ("control-plane", "gateway", "panel", "waiting-pumpkin", "postgres")
PUMPKIN_PREFIX = "melon-pumpkin:host-control-"
LOCK = Path("/run/lock/kurbis-production-deploy.lock")
CHECKOUT_BACKUP = ROOT / "deploy-checkout-overrides"
WORKER_BACKUP = ROOT / "worker-rollbacks"
ENV_BACKUP = ROOT / "worker-bundle-rollbacks"
MARKER = ROOT / "deployed-release.json"
LEGACY = (b"identity_not_linked", b"Link this Minecraft account")
ADMISSION = b"Join with online Minecraft authentication enabled to create it"


class Error(RuntimeError):
    pass


def run(args: Sequence[str], *, input_text: str | None = None, cwd: Path | None = None) -> str:
    p = subprocess.run(list(args), input=input_text, cwd=cwd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if p.returncode:
        raise Error(f"{args[0]} failed: {p.stderr.strip() or p.stdout.strip()}")
    return p.stdout.strip()


def root_only() -> None:
    if os.geteuid() != 0:
        raise Error("respond5 release verification requires root")


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def identity() -> str:
    root_only()
    if WORKER_ENV.is_symlink() or not WORKER_ENV.is_file():
        raise Error("worker credentials are not a regular file")
    m = WORKER_ENV.stat()
    if m.st_uid != 0 or stat.S_IMODE(m.st_mode) != 0o600:
        raise Error("worker credentials must be root-owned mode 0600")
    for line in WORKER_ENV.read_text(encoding="utf-8").splitlines():
        match = re.fullmatch(r"\s*(?:export\s+)?WORKER_ID\s*=\s*(.*?)\s*", line)
        if match:
            try:
                return str(uuid.UUID(match.group(1).strip("\"'")))
            except ValueError:
                break
    raise Error("invalid worker identity")


def worker_image() -> str:
    values = [line.split("=", 1)[1].strip().strip("\"'") for line in WORKER_ENV.read_text(encoding="utf-8").splitlines() if re.match(r"^export PUMPKIN_IMAGE=", line)]
    if len(values) != 1 or not re.fullmatch(re.escape(PUMPKIN_PREFIX) + r"[0-9a-f]{40}", values[0]):
        raise Error("worker PUMPKIN_IMAGE is not a full SHA-pinned Kurbis image")
    return values[0]


def service_valid() -> None:
    root_only()
    if WORKER.resolve() != WORKER or "ExecStart=/usr/local/bin/kurbis-worker" not in WORKER_UNIT.read_text(encoding="utf-8") or f"exec {WORKER}" not in WORKER_LAUNCHER.read_text(encoding="utf-8").splitlines():
        raise Error("worker unit/launcher does not target the approved Kurbis path")


def states() -> dict[str, list[dict[str, str]]]:
    out = {}
    for service in SERVICES:
        ids = run(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={PROJECT}", "--filter", f"label=com.docker.compose.service={service}"]).splitlines()
        items = json.loads(run(["docker", "inspect", *ids])) if ids else []
        out[service] = [{"id": x["Id"], "image": x["Image"], "status": x["State"]["Status"], "health": x["State"].get("Health", {}).get("Status", "none"), "revision": x.get("Config", {}).get("Labels", {}).get("org.opencontainers.image.revision", "")} for x in items]
    return out


def single_worker() -> int:
    service_valid()
    if run(["systemctl", "is-active", WORKER_SERVICE]) != "active":
        raise Error("worker systemd service is not active")
    pid = int(run(["systemctl", "show", "--property=MainPID", "--value", WORKER_SERVICE]))
    paths = []
    for proc in Path("/proc").iterdir():
        if proc.name.isdigit():
            try:
                exe = (proc / "exe").resolve()
            except (FileNotFoundError, PermissionError, OSError):
                continue
            if exe.name == "worker":
                paths.append(exe)
    if paths != [WORKER.resolve()] or digest(Path(f"/proc/{pid}/exe")) != digest(WORKER):
        raise Error(f"worker process set is unsafe: {paths}")
    return pid


def lease(db_id: str) -> None:
    wid = identity()
    sql = """SELECT wc.worker_id::text,(wc.revoked_at IS NULL AND wc.lease_expires_at > now()),(w.heartbeat_at > now()-interval '30 seconds'),(SELECT count(*) FROM worker_credentials c WHERE c.worker_id=wc.worker_id AND c.revoked_at IS NULL AND c.lease_expires_at > now()) FROM worker_credentials wc JOIN workers w ON w.id=wc.worker_id WHERE wc.worker_id=:'worker_id'::uuid AND wc.revoked_at IS NULL;"""
    got = run(["docker", "exec", "-i", "--env", f"KURBIS_WORKER_ID={wid}", db_id, "sh", "-lc", 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -At -F "|" -v worker_id="$KURBIS_WORKER_ID" -f -'], input_text=sql)
    if got.strip() != f"{wid}|t|t|1":
        raise Error("worker lease is not uniquely active and renewing")


def image_for_app(state: dict[str, list[dict[str, str]]], revision: str | None = None) -> str:
    cp, gw = state["control-plane"], state["gateway"]
    if len(cp) != 1 or len(gw) != 1 or cp[0]["id"] == gw[0]["id"] or cp[0]["image"] != gw[0]["image"]:
        raise Error("control-plane/gateway are not two running containers on one image")
    if revision and (cp[0]["revision"] != revision or gw[0]["revision"] != revision):
        raise Error("running app OCI revision differs from requested SHA")
    if cp[0]["status"] != "running" or gw[0]["status"] != "running" or cp[0]["health"] != "healthy":
        raise Error("app is not healthy/running")
    return cp[0]["image"]


def baseline() -> dict[str, list[dict[str, str]]]:
    root_only(); service_valid(); identity(); state = states()
    for service in SERVICES:
        if len(state[service]) != 1 or state[service][0]["status"] != "running" or (service != "gateway" and state[service][0]["health"] != "healthy"):
            raise Error(f"baseline service is not unique/healthy: {service}")
    single_worker(); lease(state["postgres"][0]["id"]); return state


def lock(token: str, acquire: bool) -> dict[str, str]:
    root_only()
    if not re.fullmatch(r"[0-9a-f]{32}", token): raise Error("invalid rollout token")
    if acquire:
        LOCK.parent.mkdir(mode=0o700, exist_ok=True)
        if LOCK.exists(): raise Error("another rollout lock exists")
        LOCK.mkdir(mode=0o700); (LOCK / "owner").write_text(token + "\n", encoding="ascii"); os.chmod(LOCK / "owner", 0o600); return {"status":"locked"}
    if LOCK.is_symlink() or not (LOCK / "owner").is_file() or (LOCK / "owner").read_text().strip() != token: raise Error("rollout lock ownership mismatch")
    (LOCK / "owner").unlink(); LOCK.rmdir(); return {"status":"released"}


def stage(revision: str, token: str, expected: dict[str, str]) -> dict[str, str]:
    root_only(); identity();
    if CHECKOUT.is_symlink() or not (CHECKOUT / ".git").is_dir(): raise Error("invalid Dokploy checkout")
    CHECKOUT_BACKUP.mkdir(mode=0o700, exist_ok=True)
    backup = CHECKOUT_BACKUP / token
    if backup.exists(): raise Error("checkout backup already exists")
    backup.mkdir(mode=0o700)
    status = run(["git","-C",str(CHECKOUT),"status","--porcelain","-z","--untracked-files=all"])
    if any(record.startswith(b"?? ") for record in status.encode().split(b"\0") if record): raise Error("untracked Dokploy files require review")
    changed = run(["git","-C",str(CHECKOUT),"diff","--name-only","-z","HEAD"]).split("\0")
    manifest=[]
    for relative in filter(None, changed):
        path=CHECKOUT/relative
        if not path.is_file() or path.is_symlink() or Path(relative).is_absolute() or ".." in Path(relative).parts: raise Error(f"unsafe override: {relative}")
        m=path.stat(); content=path.read_bytes(); manifest.append({"path":relative,"content":base64.b64encode(content).decode(),"mode":stat.S_IMODE(m.st_mode),"uid":m.st_uid,"gid":m.st_gid,"target":expected.get(relative,"missing")})
    (backup/"manifest.json").write_text(json.dumps(manifest),encoding="utf-8"); os.chmod(backup/"manifest.json",0o600)
    run(["git","-C",str(CHECKOUT),"fetch","origin","main"])
    if run(["git","-C",str(CHECKOUT),"rev-parse",f"{revision}^{{commit}}"] ) != revision: raise Error("release SHA unavailable on respond5")
    run(["git","-C",str(CHECKOUT),"reset","--hard",revision]); run(["git","-C",str(CHECKOUT),"clean","-fd","--exclude=.env"])
    if run(["git","-C",str(CHECKOUT),"status","--porcelain","--untracked-files=all"]): raise Error("staged checkout remains dirty")
    for path, blob in expected.items():
        if run(["git","-C",str(CHECKOUT),"hash-object","--",path]) != blob: raise Error(f"source mismatch: {path}")
    return {"status":"staged","revision":revision}


def restore(token: str) -> dict[str, str]:
    root_only(); backup=CHECKOUT_BACKUP/token; manifest=json.loads((backup/"manifest.json").read_text(encoding="utf-8")); restored=[]; preserved=[]
    for entry in manifest:
        path=CHECKOUT/entry["path"]
        current = run(["git","-C",str(CHECKOUT),"hash-object","--",entry["path"]]) if path.is_file() else "missing"
        if current != entry["target"]: preserved.append(entry["path"]); continue
        content=base64.b64decode(entry["content"]); atomic(path,content,int(entry["mode"]),int(entry["uid"]),int(entry["gid"])); restored.append(entry["path"])
    shutil.rmtree(backup)
    return {"status":"restored","restored":json.dumps(restored),"preserved":json.dumps(preserved)}


def atomic(path: Path, content: bytes, mode: int, uid: int=0, gid: int=0) -> None:
    fd,tmp=tempfile.mkstemp(prefix=f".{path.name}.",dir=path.parent); temp=Path(tmp)
    try:
        with os.fdopen(fd,"wb") as f: f.write(content); f.flush(); os.fsync(f.fileno())
        os.chown(temp,uid,gid); os.chmod(temp,mode); os.replace(temp,path)
    except BaseException: temp.unlink(missing_ok=True); raise


def extract(image: str, directory: Path) -> tuple[Path,Path,Path]:
    cid=run(["docker","create","--entrypoint","/bin/true",image])
    try:
        cp,gw,w=directory/"control",directory/"gateway",directory/"worker"
        for source,target in (("control-plane",cp),("gateway",gw),("worker",w)): run(["docker","cp",f"{cid}:/usr/local/bin/{source}",str(target)])
        return cp,gw,w
    finally: run(["docker","rm",cid])


def build_pumpkin(revision: str, expected: dict[str,str]) -> dict[str,str]:
    root_only();
    for path,blob in expected.items():
        if run(["git","-C",str(CHECKOUT),"hash-object","--",path]) != blob: raise Error(f"source mismatch before Pumpkin build: {path}")
    tag=f"{PUMPKIN_PREFIX}{revision}"
    run(["docker","build","--target","host-control","--build-arg",f"BUILD_REVISION={revision}","-f","runtime/pumpkin/Dockerfile","-t",tag,"."],cwd=CHECKOUT)
    data=run(["docker","image","inspect",tag,"--format",'{{.Id}}|{{index .Config.Labels "org.opencontainers.image.revision"}}']).split("|")
    if len(data)!=2 or data[1]!=revision: raise Error("Pumpkin image revision label mismatch")
    return {"status":"built","pumpkin_image":tag,"pumpkin_image_id":data[0],"pumpkin_revision":revision}


def set_worker_image(tag: str) -> bool:
    if not re.fullmatch(re.escape(PUMPKIN_PREFIX)+r"[0-9a-f]{40}",tag): raise Error("invalid pinned Pumpkin image")
    lines=WORKER_ENV.read_text(encoding="utf-8").splitlines(); matches=[i for i,l in enumerate(lines) if re.match(r"^export PUMPKIN_IMAGE=",l)]
    if len(matches)!=1: raise Error("worker env must have one PUMPKIN_IMAGE")
    current=lines[matches[0]].split("=",1)[1].strip().strip('"\'')
    if current==tag:return False
    ENV_BACKUP.mkdir(mode=0o700,exist_ok=True); old=digest(WORKER_ENV); backup=ENV_BACKUP/f"worker-env-{old}"
    if not backup.exists(): atomic(backup,WORKER_ENV.read_bytes(),0o600)
    lines[matches[0]]=f"export PUMPKIN_IMAGE={tag}"; atomic(WORKER_ENV,("\n".join(lines)+"\n").encode(),0o600)
    if worker_image()!=tag: raise Error("worker image update failed")
    return True

def image_revision_files(image: str, revision: str) -> None:
    cid=run(["docker","create","--entrypoint","/bin/true",image]); tmp=Path(tempfile.mkdtemp(prefix="kurbis-image-"))
    try:
        for target in ("control-plane","gateway","worker"): run(["docker","cp",f"{cid}:/usr/local/bin/{target}",str(tmp/target)])
        cp=(tmp/"control-plane").read_bytes(); gw=(tmp/"gateway").read_bytes(); w=(tmp/"worker").read_bytes()
        if any(marker in cp or marker in gw for marker in LEGACY) or ADMISSION not in gw: raise Error("application binaries failed admission marker scan")
        if any(revision.encode() not in data for data in (cp,gw,w)): raise Error("application binary lacks embedded release SHA")
        return
    finally: run(["docker","rm",cid]); shutil.rmtree(tmp,ignore_errors=True)


def wait_sync(revision: str, pumpkin: str, baseline_state: dict[str,list[dict[str,str]]], expected: dict[str,str]) -> dict[str,str]:
    require_production_paths(); deadline=time.monotonic()+900
    while time.monotonic()<deadline:
        state=states(); cp,gw=state["control-plane"],state["gateway"]
        if len(cp)==1 and len(gw)==1 and cp[0]["id"] not in {x["id"] for x in baseline_state["control-plane"]} and gw[0]["id"] not in {x["id"] for x in baseline_state["gateway"]} and cp[0]["image"]==gw[0]["image"] and cp[0]["revision"]==revision and gw[0]["revision"]==revision and cp[0]["health"]=="healthy" and all(len(state[s])==1 and state[s][0]["status"]=="running" and state[s][0]["health"]=="healthy" for s in ("panel","waiting-pumpkin","postgres")):
            break
        time.sleep(5)
    else: raise Error("Dokploy containers did not become exact/healthy before timeout")
    image=cp[0]["image"]; image_revision_files(image,revision)
    cpbin,gwbin,wbin=extract(image,Path(tempfile.mkdtemp(prefix="kurbis-extract-")))
    old=digest(WORKER); new=digest(wbin); old_env=digest(WORKER_ENV); env_changed=set_worker_image(pumpkin); backup=None; env_backup=ENV_BACKUP/f"worker-env-{old_env}"
    if old != new:
        WORKER_BACKUP.mkdir(mode=0o700,exist_ok=True); backup=WORKER_BACKUP/f"worker-{old}"; atomic(backup,WORKER.read_bytes(),stat.S_IMODE(WORKER.stat().st_mode)); atomic(WORKER,wbin.read_bytes(),stat.S_IMODE(WORKER.stat().st_mode))
    if old != new or env_changed: run(["systemctl","restart",WORKER_SERVICE])
    try:
        pid=single_worker(); time.sleep(35); pid=single_worker(); lease(state["postgres"][0]["id"])
        result={"status":"verified","image_id":image,"control_plane_container_id":cp[0]["id"],"gateway_container_id":gw[0]["id"],"control_plane_sha256":digest(cpbin),"gateway_sha256":digest(gwbin),"worker_sha256":new,"worker_pid":str(pid),"worker_id":identity(),"worker_lease_verified":"true","pumpkin_image":pumpkin}
        return result
    except BaseException:
        if backup:
            atomic(WORKER,backup.read_bytes(),stat.S_IMODE(WORKER.stat().st_mode))
        if env_changed and env_backup.exists(): atomic(WORKER_ENV,env_backup.read_bytes(),0o600)
        if backup or env_changed: run(["systemctl","restart",WORKER_SERVICE])
        raise


def mark(receipt: str) -> dict[str,str]:
    root_only(); data=json.loads(receipt)
    if data.get("status")!="verified": raise Error("cannot mark unverified release")
    required=("source_revision","deployment_id","image_id","control_plane_sha256","gateway_sha256","worker_sha256","worker_lease_verified")
    if any(not data.get(k) for k in required): raise Error("release receipt incomplete")
    atomic(MARKER,(json.dumps(data,sort_keys=True)+"\n").encode(),0o600)
    return {"status":"marked"}


def main() -> int:
    parser=argparse.ArgumentParser(); parser.add_argument("--remote",required=True); parser.add_argument("--token"); parser.add_argument("--revision"); parser.add_argument("--expected"); parser.add_argument("--baseline"); parser.add_argument("--pumpkin"); parser.add_argument("--receipt"); args=parser.parse_args()
    try:
        expected=json.loads(args.expected) if args.expected else None
        if args.remote=="lock": result=lock(args.token,True)
        elif args.remote=="unlock": result=lock(args.token,False)
        elif args.remote=="baseline": result={"status":"snapshot","snapshot":baseline()}
        elif args.remote=="stage": result=stage(args.revision,args.token,expected)
        elif args.remote=="restore": result=restore(args.token)
        elif args.remote=="build-pumpkin": result=build_pumpkin(args.revision,expected)
        elif args.remote=="wait": result=wait_sync(args.revision,args.pumpkin,json.loads(args.baseline),expected)
        elif args.remote=="mark": result=mark(args.receipt)
        else: raise Error("unknown rollout operation")
        print(json.dumps(result,sort_keys=True)); return 0
    except (Error, OSError, subprocess.TimeoutExpired, json.JSONDecodeError) as e:
        print(f"rollout rejected: {e}",file=__import__('sys').stderr); return 1

if __name__=="__main__": raise SystemExit(main())
