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
