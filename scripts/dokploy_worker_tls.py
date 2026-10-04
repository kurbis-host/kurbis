import hashlib
import json
import re
import stat
import subprocess
from pathlib import Path

def verify_gateway_tls_configuration(services:dict,host_directory:str,bedrock_port:str,*,error_cls=RuntimeError)->None:
    gateway=services.get("gateway",{})
    environment=gateway.get("environment",{}) or {}
    if gateway.get("user") not in {"2613:2613",2613}:
        raise error_cls("gateway must run as the approved TLS-reader UID/GID 2613:2613")
    if environment.get("BEDROCK_TLS_CERT_FILE")!="/run/kurbis-tls/fullchain.pem" or environment.get("BEDROCK_TLS_KEY_FILE")!="/run/kurbis-tls/privkey.pem":
        raise error_cls("gateway TLS certificate/key paths do not match the read-only Compose mount")
    tls_mounts=[volume for volume in gateway.get("volumes",[]) if isinstance(volume,dict) and volume.get("target")=="/run/kurbis-tls"]
    if len(tls_mounts)!=1:
        raise error_cls("gateway must have exactly one TLS certificate directory mount")
    mount=tls_mounts[0]
    if mount.get("type")!="bind" or Path(str(mount.get("source",""))).resolve()!=Path(host_directory).resolve() or mount.get("read_only") is not True:
        raise error_cls("gateway TLS directory must be the configured host path mounted read-only")
    bedrock_ports={(str(port.get("published")),int(port.get("target",0)),str(port.get("protocol","tcp")))
                   for port in gateway.get("ports",[]) if isinstance(port,dict)}
    published=str(bedrock_port)
    if not {(published,19132,"tcp"),(published,19132,"udp")}.issubset(bedrock_ports):
        raise error_cls("gateway must publish Bedrock TLS TCP and UDP ports")


def verify_bedrock_tls_assets(host_directory:str,root_domain:str,*,error_cls=RuntimeError,run_command,project:str)->None:
    """Fail closed unless the current gateway mount contains a valid, private, readable TLS key."""
    tls_dir=Path(host_directory)
    if not tls_dir.is_absolute() or tls_dir.is_symlink() or not tls_dir.is_dir():
        raise error_cls("configured Bedrock TLS host directory is missing, relative, or unsafe")
    cert=tls_dir/"fullchain.pem";key=tls_dir/"privkey.pem"
    for path in (cert,key):
        if path.is_symlink() or not path.is_file():raise error_cls("configured Bedrock TLS certificate/key file is missing or unsafe")
    gateway_ids=run_command(["docker","ps","-q","--filter",f"label=com.docker.compose.project={project}",
                     "--filter","label=com.docker.compose.service=gateway"]).splitlines()
    if len(gateway_ids)!=1 or not re.fullmatch(r"[0-9a-f]{64}",gateway_ids[0]):
        raise error_cls("cannot verify TLS access using the unique running gateway container")
    inspected=json.loads(run_command(["docker","inspect",gateway_ids[0]]))[0]
    mounts=[m for m in inspected.get("Mounts",[]) if m.get("Destination")=="/run/kurbis-tls"]
    if len(mounts)!=1 or mounts[0].get("Source")!=str(tls_dir) or mounts[0].get("RW") is not False:
        raise error_cls("running gateway does not expose the configured TLS directory through an exact read-only mount")
    try:
        st=key.stat()
        if stat.S_IMODE(st.st_mode)&0o007:
            raise error_cls("Bedrock private key must not be accessible to other host/container users")
        checked=subprocess.run(["openssl","x509","-in",str(cert),"-noout","-checkend","0","-checkhost",root_domain],
            stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=False)
        if checked.returncode:raise error_cls("Bedrock certificate is expired or does not cover ROOT_DOMAIN")
        cert_key=subprocess.run(["openssl","x509","-in",str(cert),"-pubkey","-noout"],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,check=False)
        private_key=subprocess.run(["openssl","pkey","-in",str(key),"-pubout"],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,check=False)
        if cert_key.returncode or private_key.returncode or hashlib.sha256(cert_key.stdout).digest()!=hashlib.sha256(private_key.stdout).digest():
            raise error_cls("Bedrock certificate does not match its configured private key")
        run_command(["docker","exec","--user","2613:2613",gateway_ids[0],"sh","-lc",
             "test -r /run/kurbis-tls/fullchain.pem && test -r /run/kurbis-tls/privkey.pem"])
    except (OSError,subprocess.SubprocessError) as error:
        raise error_cls("could not verify Bedrock TLS certificate/key access safely") from error
