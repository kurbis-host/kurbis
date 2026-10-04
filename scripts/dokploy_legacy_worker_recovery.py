from __future__ import annotations

import datetime as dt
import time
from pathlib import Path
from typing import Any

def read_process_start_ticks(pid: int, error_cls=RuntimeError) -> str:
    raw=Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
    closing=raw.rfind(")")
    if closing<0:raise error_cls("worker process stat record is malformed")
    fields=raw[closing+1:].split()
    if len(fields)<=19:raise error_cls("worker process start time is unavailable")
    return fields[19]  # proc stat field 22 (starttime), after pid/comm.


def _parse_utc_timestamp(value: str, error_cls=RuntimeError) -> dt.datetime:
    try:return dt.datetime.fromisoformat(value.replace("Z","+00:00"))
    except ValueError as error:raise error_cls("worker lease heartbeat timestamp is malformed") from error


def verify_restarted_worker_impl(worker_id: str, postgres_container_id: str, *, previous_pid: int,
                            previous_start_ticks: str, previous_heartbeat_at: str,
                            expected_worker_sha256: str, expected_env_sha256: str,
                            expected_pumpkin_image: str, systemd_worker_pid,
                            service_active, worker_process_start_ticks, worker_process_environment,
                            executable_sha256, env_sha256, worker_lease_snapshot, error_cls=RuntimeError,
                            timeout_seconds: int = 120, observation_seconds: int = 35,
                            poll_seconds: int = 3, monotonic=None, sleeper=None) -> dict[str, Any]:
    """Require a genuinely new worker and renewing lease across a full stale-lease window."""
    monotonic=monotonic or time.monotonic;sleeper=sleeper or time.sleep
    started=monotonic();deadline=started+timeout_seconds
    baseline_heartbeat=_parse_utc_timestamp(previous_heartbeat_at,error_cls)
    new_identity=None;first_new_heartbeat=None
    while monotonic()<deadline:
        pid=systemd_worker_pid()
        if pid>1 and service_active():
            start_ticks=worker_process_start_ticks(pid)
            identity=(pid,start_ticks)
            if identity!=(previous_pid,previous_start_ticks):
                if new_identity is None:new_identity=identity
                elif identity!=new_identity:raise error_cls("worker process restarted again during lease verification")
                process_env=worker_process_environment(pid)
                if (executable_sha256(pid)!=expected_worker_sha256
                    or process_env.get("PUMPKIN_IMAGE")!=expected_pumpkin_image
                    or process_env.get("WORKER_ID")!=worker_id
                    or env_sha256()!=expected_env_sha256):
                    raise error_cls("restarted worker executable or environment differs from restored baseline")
                try:
                    lease=worker_lease_snapshot(worker_id,postgres_container_id)
                except error_cls:
                    # A service restart can briefly outlast the previous lease; keep
                    # polling until the new process renews a unique active lease.
                    sleeper(poll_seconds)
                    continue
                heartbeat=_parse_utc_timestamp(lease["heartbeat_at"],error_cls)
                database_now=_parse_utc_timestamp(lease["database_now"],error_cls)
                if database_now-heartbeat>dt.timedelta(seconds=30):
                    sleeper(poll_seconds)
                    continue
                if heartbeat>baseline_heartbeat:
                    if first_new_heartbeat is None:first_new_heartbeat=heartbeat
                    elif heartbeat>first_new_heartbeat and monotonic()-started>=observation_seconds:
                        return {"pid":pid,"start_ticks":start_ticks,"lease":lease["raw"],
                                "heartbeat_at":lease["heartbeat_at"],"database_now":lease["database_now"]}
        sleeper(poll_seconds)
    raise error_cls("restarted worker did not prove a fresh PID/start time and renewing lease beyond the 30-second heartbeat window")


def reconcile_worker_bootstrap(manifest: dict[str, Any], token: str) -> dict[str, Any]:
    require_lock(token)
    attestation=load_attestation(manifest)
    expected=attestation["inventory"];baseline_sha=attestation["manifest_sha256"]
    migration=migration_record(manifest)
    if migration is None:
        current=collect_inventory()
        verify_attested_legacy_services(current,expected,baseline_sha)
        return {"status":"no-mutation","worker_id":expected["worker"]["worker_id"]}
    if migration.get("status")=="rolled_back":
        current=collect_inventory()
        verify_attested_legacy_services(current,expected,baseline_sha)
        return {"status":"rolled-back","worker_id":expected["worker"]["worker_id"]}
    if migration.get("status")=="migrated":
        checked=verify_worker_compatibility(manifest,migration["candidate_image_id"],migration["candidate_revision"],token)
        return {**migration,"status":"migrated-reconciled","worker_lease":checked["worker_lease"]}
    if migration.get("status")=="pending":
        return bootstrap_worker(manifest,migration["candidate_image_id"],migration["candidate_revision"],token)
    raise BaselineError("unrecognized worker bootstrap journal state; retaining deployment lock")


def authorized_worker_images_for_inventory(manifest: dict[str, Any]) -> set[str]:
    migration=migration_record(manifest)
    if migration is None or migration.get("status")=="rolled_back":
        return {PUMPKIN_LEGACY}
    if migration.get("status") not in {"pending","migrated"}:
        raise BaselineError("worker migration journal is not in a retryable state")
    revision=migration.get("candidate_revision");tag=migration.get("candidate_tag")
    image_id=migration.get("candidate_image_id")
    if (not isinstance(revision,str) or not valid_sha(revision)
        or tag!=f"melon-pumpkin:host-control-{revision}"
        or not isinstance(image_id,str) or not re.fullmatch(r"sha256:[0-9a-f]{64}",image_id)):
        raise BaselineError("worker migration journal candidate identity is invalid")
    return {PUMPKIN_LEGACY,tag}


def verify_attested_candidate_source(candidate_revision: str, expected_sources: dict[str, str]) -> str:
    if not valid_sha(candidate_revision) or not isinstance(expected_sources,dict) or not expected_sources:
        raise BaselineError("exact release SHA and tracked source manifest are required for refreshed baseline verification")
    if CHECKOUT.is_symlink() or not (CHECKOUT/".git").is_dir():
        raise BaselineError("refreshed Dokploy release checkout is unsafe")
    branch=run_text(["git","-C",str(CHECKOUT),"branch","--show-current"])
    checkout_head=run_text(["git","-C",str(CHECKOUT),"rev-parse","HEAD"])
    if branch!="main":
        raise BaselineError("Dokploy checkout must be on the configured main source branch")
    tag=f"kurbis-release-{candidate_revision}"
    resolved=subprocess.run(["git","-C",str(CHECKOUT),"rev-parse",f"refs/tags/{tag}^{{commit}}"],
        stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,check=False)
    if resolved.returncode:
        try:run(["git","-C",str(CHECKOUT),"fetch","origin",f"refs/tags/{tag}:refs/tags/{tag}"])
        except BaselineError as error:raise BaselineError("could not fetch the exact immutable release tag for attested inventory verification") from error
        resolved=subprocess.run(["git","-C",str(CHECKOUT),"rev-parse",f"refs/tags/{tag}^{{commit}}"],
            stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,check=False)
    if resolved.returncode or resolved.stdout.strip()!=candidate_revision:
        raise BaselineError("refreshed Dokploy checkout lacks the exact immutable release tag")
    ancestry=subprocess.run(["git","-C",str(CHECKOUT),"merge-base","--is-ancestor",checkout_head,candidate_revision],
                           stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=False)
    if ancestry.returncode!=0:
        raise BaselineError("Dokploy checkout HEAD is not an ancestor of the exact requested release")
    tracked=set(run_text(["git","-C",str(CHECKOUT),"ls-tree","-r","--name-only",candidate_revision]).splitlines())
    if tracked!=set(expected_sources):
        raise BaselineError("refreshed release manifest does not cover the exact tracked source tree")
    for path,blob in expected_sources.items():
        if Path(path).is_absolute() or ".." in Path(path).parts or not re.fullmatch(r"[0-9a-f]{40}",str(blob)):
            raise BaselineError("refreshed release manifest contains an unsafe path/blob")
        if run_text(["git","-C",str(CHECKOUT),"rev-parse",f"{candidate_revision}:{path}"])!=blob:
            raise BaselineError(f"refreshed release source mismatch: {path}")
    status=checkout_status(checkout_head)
    if status!=[{"code":"!!","path":".env"}]:
        raise BaselineError("refreshed release checkout is dirty beyond the protected ignored Dokploy .env")
    return checkout_head


def verify_attested_legacy_services(current: dict[str, Any], expected: dict[str, Any], manifest_sha: str,
                                    *, candidate_revision: str | None = None,
                                    inventory_checkout_head: str | None = None) -> None:
    if current.get("compose_id")!=expected.get("compose_id") or current.get("project")!=expected.get("project"):
        raise BaselineError("legacy Compose identity changed after attestation")
    current_source=current.get("source",{});expected_source=expected.get("source",{})
    if candidate_revision is None:
        source_keys=("checkout_head","modified_inputs")
    else:
        if (not valid_sha(candidate_revision) or not valid_sha(inventory_checkout_head or "")
            or current_source.get("checkout_head")!=inventory_checkout_head):
            raise BaselineError("Dokploy inventory checkout HEAD differs from the verified release history")
        if current_source.get("modified_inputs")!=[{"code":"!!","path":".env"}]:
            raise BaselineError("refreshed release checkout contains unexpected tracked/ignored build inputs")
        source_keys=("build_revision_env_present","compose_env_file_sha256","compose_env_file_mode",
                     "compose_env_file_uid","compose_env_file_gid","safe_compose_settings")
    for key in source_keys:
        if current_source.get(key)!=expected_source.get(key):
            raise BaselineError(f"legacy checkout source/environment differs from approved state: {key}")
    for name,before in expected["services"].items():
        after=current["services"].get(name,{})
        for key in ("image_id","image_tag","status","health","image","payloads","ports"):
            if after.get(key)!=before.get(key):raise BaselineError(f"legacy service changed after attestation: {name}/{key}")
    before=expected["worker"];after=current["worker"]
    fixed=("service","service_active","worker_id","worker_path","worker_mode","worker_uid","worker_gid","worker_sha256","worker_process_sha256",
           "worker_process_count","service_unit_sha256","launcher_sha256","worker_env_mode","worker_env_uid","legacy_pumpkin_image","lease")
    for key in fixed:
        if after.get(key)!=before.get(key):raise BaselineError(f"legacy worker changed after attestation: {key}")
    migration=migration_record_from_hash(manifest_sha)
    config=after.get("configured_pumpkin_image");process=after.get("process_pumpkin_image")
    env_sha=after.get("worker_env_sha256")
    if config==PUMPKIN_LEGACY and process==PUMPKIN_LEGACY:
        if env_sha==before.get("worker_env_sha256"):return
        if migration and migration.get("status")=="rolled_back" and env_sha==migration.get("old_worker_env_sha256"):return
        raise BaselineError("legacy worker env digest changed without a matching migration/rollback record")
    if not migration or migration.get("status") not in {"pending","migrated"}:
        raise BaselineError("worker moved from legacy config without a protected migration record")
    allowed={PUMPKIN_LEGACY,migration.get("candidate_tag")}
    if config not in allowed or process not in allowed:
        raise BaselineError("current worker config/process differs from protected migration journal")
    if env_sha not in {before.get("worker_env_sha256"),migration.get("new_worker_env_sha256")}:
        raise BaselineError("worker config digest differs from baseline or migration journal")
