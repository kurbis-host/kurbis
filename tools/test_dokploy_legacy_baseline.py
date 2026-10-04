from __future__ import annotations

import importlib.util
import datetime as dt
import json
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("legacy_baseline", ROOT / "scripts/dokploy_legacy_baseline.py")
assert spec and spec.loader
legacy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(legacy)
deploy_spec = importlib.util.spec_from_file_location("deploy", ROOT / "scripts/deploy_dokploy.py")
assert deploy_spec and deploy_spec.loader
deploy = importlib.util.module_from_spec(deploy_spec)
deploy_spec.loader.exec_module(deploy)
FIXTURE = json.loads((ROOT / "tests/fixtures/respond5_legacy_baseline_inventory.json").read_text())


class NoopLock:
    def __enter__(self):
        return self

    def __exit__(self, *args):
        return False


def _with_new_container_ids(inventory):
    changed=json.loads(json.dumps(inventory))
    for index,service in enumerate(changed["services"].values(),10):
        service["container_id"]=f"{index:064x}"
    changed["worker"]["main_pid"]+=100
    return changed


class LegacyBaselineTests(unittest.TestCase):
    def test_fixture_matches_captured_unlabelled_respond5_runtime(self):
        inventory = FIXTURE["expected_inventory"]
        self.assertEqual(inventory["source"]["checkout_head"], "f6b05712bb130747060e5cfa049d84ed27020835")
        self.assertEqual(inventory["worker"]["configured_pumpkin_image"], "melon-pumpkin:host-control")
        self.assertTrue(all(service["image"]["revision_label"] is None for service in inventory["services"].values()))
        self.assertEqual(inventory["services"]["control-plane"]["image_id"], inventory["services"]["gateway"]["image_id"])
        self.assertEqual(inventory["services"]["waiting-pumpkin"]["image_tag"], "melon-pumpkin:waiting")
        self.assertEqual(inventory["worker"]["worker_sha256"], inventory["worker"]["worker_process_sha256"])
        self.assertEqual(legacy.validate_manifest(FIXTURE, inventory), legacy.canonical_hash({
            "schema": FIXTURE["schema"],
            "source_revision_claim": FIXTURE["source_revision_claim"],
            "operator": FIXTURE["operator"],
            "reason": FIXTURE["reason"],
            "expected_inventory": inventory,
        }))

    def test_deploy_baseline_adapter_preserves_legacy_inventory_without_fake_revision(self):
        snapshot=deploy.legacy_wait_baseline(FIXTURE)
        expected=FIXTURE["expected_inventory"]["services"]
        self.assertEqual(snapshot["control-plane"][0]["id"],expected["control-plane"]["container_id"])
        self.assertEqual(snapshot["control-plane"][0]["image"],expected["control-plane"]["image_id"])
        self.assertEqual(snapshot["control-plane"][0]["revision"],"")
        self.assertEqual(snapshot["waiting-pumpkin"][0]["health"],"healthy")
        self.assertEqual(snapshot,deploy.legacy_wait_baseline(FIXTURE["expected_inventory"]))
        self.assertEqual(deploy.legacy_revision(FIXTURE),legacy.LEGACY_REVISION_CLAIM)
        stale=json.loads(json.dumps(FIXTURE));stale["source_revision_claim"]="a"*40
        with self.assertRaisesRegex(deploy.DeploymentError,"does not claim"):
            deploy.legacy_revision(stale)

    def test_checked_in_operator_manifest_is_the_exact_captured_legacy_inventory(self):
        operator_manifest=json.loads((ROOT/"deployment/legacy-baseline-respond5.json").read_text())
        self.assertEqual(operator_manifest["expected_inventory"],FIXTURE["expected_inventory"])
        self.assertEqual(operator_manifest["schema"],"kurbis.respond5.legacy-baseline.v1")
        self.assertEqual(operator_manifest["expected_inventory"]["provenance_status"],"legacy-unproven")
        self.assertNotIn("INTERNAL_TOKEN",json.dumps(operator_manifest))
        self.assertNotIn("POSTGRES_PASSWORD",json.dumps(operator_manifest))
        self.assertNotIn("OAUTH_CLIENT_SECRET",json.dumps(operator_manifest))
        legacy.validate_manifest(operator_manifest,operator_manifest["expected_inventory"])

    def test_reattest_accepts_recreated_containers_only_with_exact_old_digests_and_worker_journal(self):
        expected=FIXTURE["expected_inventory"]
        current=_with_new_container_ids(expected)
        manifest_sha=legacy.canonical_hash({"schema":FIXTURE["schema"],"source_revision_claim":FIXTURE["source_revision_claim"],
            "operator":FIXTURE["operator"],"reason":FIXTURE["reason"],"expected_inventory":expected})
        journal={"baseline_manifest_sha256":manifest_sha,"status":"rolled_back","candidate_tag":"melon-pumpkin:host-control-"+"a"*40,
                 "old_worker_env_sha256":expected["worker"]["worker_env_sha256"]}
        with patch.object(legacy,"migration_record_from_hash",return_value=journal):
            legacy.verify_attested_legacy_services(current,expected,manifest_sha)
        current["services"]["panel"]["image_id"]="sha256:"+"a"*64
        with patch.object(legacy,"migration_record_from_hash",return_value=journal), self.assertRaisesRegex(legacy.BaselineError,"service changed"):
            legacy.verify_attested_legacy_services(current,expected,manifest_sha)

    def test_stale_or_unlabelled_inventory_mismatch_is_rejected(self):
        changed = json.loads(json.dumps(FIXTURE["expected_inventory"]))
        changed["services"]["control-plane"]["image_id"] = "sha256:" + "f" * 64
        changed["services"]["control-plane"]["image"]["image_id"] = changed["services"]["control-plane"]["image_id"]
        with self.assertRaisesRegex(legacy.BaselineError, "differs from exact operator-reviewed"):
            legacy.validate_manifest(FIXTURE, changed)
        labeled = json.loads(json.dumps(FIXTURE["expected_inventory"]))
        labeled["services"]["waiting-pumpkin"]["image"]["revision_label"] = "a" * 40
        altered_manifest = json.loads(json.dumps(FIXTURE))
        altered_manifest["expected_inventory"] = labeled
        with self.assertRaisesRegex(legacy.BaselineError, "unlabelled"):
            legacy.validate_manifest(altered_manifest, labeled)

    def test_refreshed_release_checkout_is_accepted_only_for_exact_tag_tree_and_clean_inputs(self):
        revision=""
        with tempfile.TemporaryDirectory() as directory:
            checkout=Path(directory)
            subprocess.run(["git","-c","init.defaultBranch=main","init","-q",str(checkout)],check=True)
            subprocess.run(["git","-C",str(checkout),"config","user.name","Test"],check=True)
            subprocess.run(["git","-C",str(checkout),"config","user.email","test@example.invalid"],check=True)
            (checkout/".gitignore").write_text(".env\n",encoding="utf-8")
            (checkout/"compose.yaml").write_text("services: {}\n",encoding="utf-8")
            (checkout/"src").mkdir();(checkout/"src/app.rs").write_text("fn main() {}\n",encoding="utf-8")
            (checkout/".env").write_text("BUILD_REVISION=old\n",encoding="utf-8")
            subprocess.run(["git","-C",str(checkout),"add",".gitignore","compose.yaml","src/app.rs"],check=True)
            subprocess.run(["git","-C",str(checkout),"commit","-qm","legacy base"],check=True)
            checkout_head=subprocess.check_output(["git","-C",str(checkout),"rev-parse","HEAD"],text=True).strip()
            (checkout/"compose.yaml").write_text("services: {release: {}}\n",encoding="utf-8")
            subprocess.run(["git","-C",str(checkout),"add","compose.yaml"],check=True)
            subprocess.run(["git","-C",str(checkout),"commit","-qm","release"],check=True)
            revision=subprocess.check_output(["git","-C",str(checkout),"rev-parse","HEAD"],text=True).strip()
            subprocess.run(["git","-C",str(checkout),"tag",f"kurbis-release-{revision}"],check=True)
            expected={path:subprocess.check_output(["git","-C",str(checkout),"rev-parse",f"HEAD:{path}"],text=True).strip()
                      for path in (".gitignore","compose.yaml","src/app.rs")}
            subprocess.run(["git","-C",str(checkout),"reset","--hard",checkout_head],stdout=subprocess.DEVNULL,check=True)
            with patch.object(legacy,"CHECKOUT",checkout):
                self.assertEqual(legacy.verify_attested_candidate_source(revision,expected),checkout_head)
                wrong={**expected,"src/app.rs":"0"*40}
                with self.assertRaisesRegex(legacy.BaselineError,"source mismatch"):
                    legacy.verify_attested_candidate_source(revision,wrong)
                (checkout/"compose.yaml").write_text("services: {changed: {}}\n",encoding="utf-8")
                with self.assertRaisesRegex(legacy.BaselineError,"source mismatch|unexpected checkout change"):
                    legacy.verify_attested_candidate_source(revision,expected)
            with patch.object(legacy,"CHECKOUT",checkout),self.assertRaisesRegex(legacy.BaselineError,"main history|immutable release tag"):
                legacy.verify_attested_candidate_source("b"*40,expected)

    def test_candidate_tag_is_fetched_before_ancestry_check_on_fresh_main_checkout(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);bare=root/"origin.git";author=root/"author";checkout=root/"checkout"
            subprocess.run(["git","init","--bare","-q",str(bare)],check=True)
            subprocess.run(["git","-c","init.defaultBranch=main","init","-q",str(author)],check=True)
            subprocess.run(["git","-C",str(author),"config","user.name","Test"],check=True)
            subprocess.run(["git","-C",str(author),"config","user.email","test@example.invalid"],check=True)
            (author/".gitignore").write_text(".env\n",encoding="utf-8")
            (author/"compose.yaml").write_text("services: {}\n",encoding="utf-8")
            (author/"src").mkdir();(author/"src/app.rs").write_text("fn main() {}\n",encoding="utf-8")
            subprocess.run(["git","-C",str(author),"add",".gitignore","compose.yaml","src/app.rs"],check=True)
            subprocess.run(["git","-C",str(author),"commit","-qm","base"],check=True)
            subprocess.run(["git","-C",str(author),"remote","add","origin",str(bare)],check=True)
            subprocess.run(["git","-C",str(author),"push","-q","origin","main"],check=True)
            checkout_head=subprocess.check_output(["git","-C",str(author),"rev-parse","HEAD"],text=True).strip()
            (author/"compose.yaml").write_text("services: {release: {}}\n",encoding="utf-8")
            subprocess.run(["git","-C",str(author),"add","compose.yaml"],check=True)
            subprocess.run(["git","-C",str(author),"commit","-qm","candidate"],check=True)
            candidate=subprocess.check_output(["git","-C",str(author),"rev-parse","HEAD"],text=True).strip()
            tag=f"kurbis-release-{candidate}"
            subprocess.run(["git","-C",str(author),"tag",tag,candidate],check=True)
            subprocess.run(["git","-C",str(author),"push","-q","origin",f"refs/tags/{tag}"],check=True)
            expected={path:subprocess.check_output(["git","-C",str(author),"rev-parse",f"{candidate}:{path}"],text=True).strip()
                      for path in (".gitignore","compose.yaml","src/app.rs")}
            subprocess.run(["git","clone","-q","--branch","main","--single-branch",str(bare),str(checkout)],check=True)
            (checkout/".env").write_text("BUILD_REVISION=old\n",encoding="utf-8")
            subprocess.run(["git","-C",str(checkout),"tag","-d",tag],stdout=subprocess.DEVNULL,check=True)
            self.assertNotEqual(subprocess.run(["git","-C",str(checkout),"rev-parse",f"refs/tags/{tag}"],
                stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=False).returncode,0)
            with patch.object(legacy,"CHECKOUT",checkout):
                self.assertEqual(legacy.verify_attested_candidate_source(candidate,expected),checkout_head)
            self.assertEqual(subprocess.check_output(["git","-C",str(checkout),"rev-parse",f"refs/tags/{tag}^{{commit}}"],text=True).strip(),candidate)

    def test_existing_legacy_attestation_revalidates_exact_refreshed_release_without_rewriting_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);audit=root/"legacy-baseline.json"
            expected=FIXTURE["expected_inventory"]
            manifest_sha=legacy.canonical_hash({"schema":FIXTURE["schema"],"source_revision_claim":FIXTURE["source_revision_claim"],
                "operator":FIXTURE["operator"],"reason":FIXTURE["reason"],"expected_inventory":expected})
            saved={"schema":FIXTURE["schema"],"source_revision_claim":FIXTURE["source_revision_claim"],
                   "provenance_status":"legacy-unproven","operator":FIXTURE["operator"],"reason":FIXTURE["reason"],
                   "manifest_sha256":manifest_sha,"inventory":expected}
            audit.write_text("preserve existing audit\n",encoding="utf-8")
            candidate="a"*40;tracked={"compose.yaml":"1"*40,"web/tsconfig.json":"2"*40}
            current=json.loads(json.dumps(expected));current["source"]["checkout_head"]=candidate
            current["source"]["modified_inputs"]=[{"code":"!!","path":".env"}]
            with patch.object(legacy,"require_lock"),patch.object(legacy,"OperationLock",NoopLock), \
                 patch.object(legacy,"AUDIT",audit),patch.object(legacy,"load_attestation",return_value=saved), \
                 patch.object(legacy,"authorized_worker_images_for_inventory",return_value={legacy.PUMPKIN_LEGACY}), \
                 patch.object(legacy,"verify_attested_candidate_source",return_value=candidate) as verify_source, \
                 patch.object(legacy,"collect_inventory",return_value=current) as collect, \
                 patch.object(legacy,"verify_attested_legacy_services") as compare, \
                 patch.object(legacy,"restore_baseline_compose_env") as restore:
                result=legacy.bootstrap(FIXTURE,"a"*32,candidate_revision=candidate,expected_sources=tracked)
            self.assertEqual(result["status"],"already-recorded")
            verify_source.assert_called_once_with(candidate,tracked)
            collect.assert_called_once_with(allowed_worker_images={legacy.PUMPKIN_LEGACY},expected_checkout_head=candidate)
            compare.assert_called_once_with(current,expected,manifest_sha,candidate_revision=candidate,inventory_checkout_head=candidate)
            restore.assert_not_called()
            self.assertEqual(audit.read_text(encoding="utf-8"),"preserve existing audit\n")

    def test_refreshed_checkout_cannot_create_a_new_legacy_attestation(self):
        with patch.object(legacy,"require_lock"),patch.object(legacy,"OperationLock",NoopLock), \
             patch.object(legacy,"AUDIT",Path("/tmp/kurbis-missing-legacy-attestation")), \
             patch.object(legacy,"collect_inventory") as collect, \
             self.assertRaisesRegex(legacy.BaselineError,"cannot create a legacy attestation from refreshed"):
            legacy.bootstrap(FIXTURE,"a"*32,candidate_revision="a"*40,expected_sources={"compose.yaml":"1"*40})
        collect.assert_not_called()

    def test_one_time_audit_bootstrap_is_idempotent_and_does_not_rewrite_live_state(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audit = root / "legacy-baseline.json"
            original_stat = Path.stat

            def root_owned_stat(path, *args, **kwargs):
                if path == root:
                    return type("RootDirectoryStat", (), {"st_uid": 0, "st_mode": stat.S_IFDIR | 0o700})()
                if path == audit:
                    return type("RootAuditStat", (), {"st_uid": 0, "st_mode": stat.S_IFREG | 0o600})()
                return original_stat(path, *args, **kwargs)

            writes = []

            def write_audit(path, data, mode):
                writes.append(path)
                data["compose_env_backup"]={"path": str(root/"compose-env"), "sha256": FIXTURE["expected_inventory"]["source"]["compose_env_file_sha256"], "mode": 0o644, "uid": 0, "gid": 0}
                path.write_text(json.dumps(data, sort_keys=True), encoding="utf-8")
                path.chmod(mode)

            with patch.object(legacy, "OperationLock", NoopLock), patch.object(legacy, "root_only"), patch.object(legacy, "require_lock"), \
                 patch.object(legacy, "ROOT", root), patch.object(legacy, "AUDIT", audit), \
                 patch.object(legacy, "collect_inventory", side_effect=[FIXTURE["expected_inventory"], _with_new_container_ids(FIXTURE["expected_inventory"]), FIXTURE["expected_inventory"]]), \
                 patch.object(legacy, "migration_record_from_hash", return_value=None), \
                 patch.object(legacy, "save_baseline_compose_env", return_value={"path": str(root/"compose-env"), "sha256": FIXTURE["expected_inventory"]["source"]["compose_env_file_sha256"], "mode": 0o644, "uid": 0, "gid": 0}), \
                 patch.object(legacy, "load_attestation", side_effect=lambda manifest: json.loads(audit.read_text())), \
                 patch.object(legacy, "restore_baseline_compose_env", return_value=False), \
                 patch.object(legacy, "atomic_json", side_effect=write_audit), patch.object(Path, "stat", root_owned_stat):
                first = legacy.bootstrap(FIXTURE,"a"*32)
                record_after_first = audit.read_bytes()
                second = legacy.bootstrap(FIXTURE,"a"*32)
                self.assertEqual(audit.read_bytes(), record_after_first)
            self.assertEqual(first["status"], "recorded")
            self.assertEqual(second["status"], "already-recorded")
            self.assertEqual(len(writes), 1)
            saved = json.loads(record_after_first)
            self.assertEqual(saved["provenance_status"], "legacy-unproven")
            self.assertIsNone(saved["inventory"]["services"]["control-plane"]["image"]["revision_label"])

    def test_audit_bootstrap_retry_accepts_only_journaled_worker_migration_and_recreated_containers(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);audit=root/"legacy-baseline.json"
            original_stat=Path.stat
            def root_owned_stat(path,*args,**kwargs):
                if path==root:return type("RootDirectoryStat",(),{"st_uid":0,"st_mode":stat.S_IFDIR|0o700})()
                if path==audit:return type("RootAuditStat",(),{"st_uid":0,"st_mode":stat.S_IFREG|0o600})()
                return original_stat(path,*args,**kwargs)
            saved={"schema":FIXTURE["schema"],"source_revision_claim":FIXTURE["source_revision_claim"],
                   "provenance_status":"legacy-unproven","operator":FIXTURE["operator"],"reason":FIXTURE["reason"],
                   "manifest_sha256":legacy.canonical_hash({"schema":FIXTURE["schema"],"source_revision_claim":FIXTURE["source_revision_claim"],
                    "operator":FIXTURE["operator"],"reason":FIXTURE["reason"],"expected_inventory":FIXTURE["expected_inventory"]}),
                   "inventory":FIXTURE["expected_inventory"]}
            audit.write_text(json.dumps(saved),encoding="utf-8");audit.chmod(0o600)
            migrated=_with_new_container_ids(FIXTURE["expected_inventory"])
            revision="a"*40;tag=f"melon-pumpkin:host-control-{revision}"
            migrated["worker"]["configured_pumpkin_image"]=tag
            migrated["worker"]["process_pumpkin_image"]=tag
            migrated["worker"]["worker_env_sha256"]="8"*64
            journal={"baseline_manifest_sha256":saved["manifest_sha256"],"status":"migrated","candidate_tag":tag,
                     "candidate_revision":revision,"candidate_image_id":"sha256:"+"9"*64,
                     "new_worker_env_sha256":"8"*64}
            with patch.object(legacy,"OperationLock",NoopLock),patch.object(legacy,"root_only"),patch.object(legacy,"require_lock"),patch.object(legacy,"ROOT",root),patch.object(legacy,"AUDIT",audit), \
                 patch.object(legacy,"WORKER_MIGRATION",root/"worker-migration.json"),patch.object(legacy,"collect_inventory",return_value=migrated) as collect, \
                 patch.object(legacy,"load_attestation",return_value=saved),patch.object(legacy,"restore_baseline_compose_env",return_value=False), \
                 patch.object(legacy,"migration_record",return_value=journal),patch.object(legacy,"migration_record_from_hash",return_value=journal),patch.object(Path,"stat",root_owned_stat):
                result=legacy.bootstrap(FIXTURE,"a"*32)
            self.assertEqual(result["status"],"already-recorded")
            for call in collect.call_args_list:
                self.assertIn(tag,call.kwargs["allowed_worker_images"])
            self.assertEqual(json.loads(audit.read_text())["manifest_sha256"],saved["manifest_sha256"])

    def test_bootstrap_failure_on_stale_live_inventory_writes_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            audit = root / "legacy-baseline.json"
            stale = json.loads(json.dumps(FIXTURE["expected_inventory"]))
            stale["services"]["panel"]["image_id"] = "sha256:" + "1" * 64
            stale["services"]["panel"]["image"]["image_id"] = stale["services"]["panel"]["image_id"]
            original_stat = Path.stat

            def root_owned_stat(path, *args, **kwargs):
                if path == root:
                    return type("RootDirectoryStat", (), {"st_uid": 0, "st_mode": stat.S_IFDIR | 0o700})()
                if path == audit:
                    return type("RootAuditStat", (), {"st_uid": 0, "st_mode": stat.S_IFREG | 0o600})()
                return original_stat(path, *args, **kwargs)

            with patch.object(legacy, "OperationLock", NoopLock), patch.object(legacy, "root_only"), patch.object(legacy, "require_lock"), \
                 patch.object(legacy, "ROOT", root), patch.object(legacy, "AUDIT", audit), \
                 patch.object(legacy, "collect_inventory", return_value=stale), patch.object(legacy, "atomic_json") as write, \
                 patch.object(Path, "stat", root_owned_stat), self.assertRaises(legacy.BaselineError):
                legacy.bootstrap(FIXTURE,"a"*32)
            write.assert_not_called()
            self.assertFalse(audit.exists())

    def test_candidate_image_must_match_active_legacy_pumpkin_wasm_and_entrypoint_bytes(self):
        expected=FIXTURE["expected_inventory"]
        revision="a"*40
        payloads=expected["worker"]["legacy_pumpkin_image"]["payloads"]
        self.assertEqual(legacy.validate_candidate_runtime(expected,revision,revision,payloads),
                         {key:payloads[key] for key in ("pumpkin","wasm","hosted_entrypoint")})
        for key,mutated in (("pumpkin","0"*64),("wasm","1"*64),("hosted_entrypoint","2"*64)):
            with self.subTest(key=key):
                candidate=json.loads(json.dumps(payloads));candidate[key]["sha256"]=mutated
                with self.assertRaisesRegex(legacy.BaselineError,"differs"):
                    legacy.validate_candidate_runtime(expected,revision,revision,candidate)
        with self.assertRaisesRegex(legacy.BaselineError,"image label"):
            legacy.validate_candidate_runtime(expected,revision,"unknown-label",payloads)

    def test_legacy_app_rollback_scope_accepts_only_attested_or_candidate_labeled_images(self):
        expected=FIXTURE["expected_inventory"]
        candidate_revision="a"*40
        current={name:{"image_id":item["image_id"],"status":"running","health":item["health"]}
                 for name,item in expected["services"].items()}
        current["control-plane"]["image_id"]="sha256:"+"1"*64
        candidate_id=current["control-plane"]["image_id"]
        def candidate_label(args, **kwargs):
            return candidate_revision if candidate_id in args else "unknown-label"
        with patch.object(legacy,"run_text",side_effect=candidate_label), patch.object(legacy,"image_info",return_value={"revision_label":candidate_revision}), patch.object(legacy,"assert_candidate_app_image"):
            allowed=legacy.validate_rollback_scope(expected,current,candidate_revision)
        self.assertEqual(allowed,{"control-plane":candidate_id})
        current["panel"]["image_id"]="sha256:"+"2"*64
        def mixed_label(args, **kwargs):
            return candidate_revision if candidate_id in args else "unknown-label"
        with patch.object(legacy,"run_text",side_effect=mixed_label), patch.object(legacy,"image_info",return_value={"revision_label":candidate_revision}), patch.object(legacy,"assert_candidate_app_image"), self.assertRaisesRegex(legacy.BaselineError,"non-baseline/non-candidate"):
            legacy.validate_rollback_scope(expected,current,candidate_revision)
        current["panel"]["image_id"]=expected["services"]["panel"]["image_id"]
        current["postgres"]["image_id"]="sha256:"+"3"*64
        with patch.object(legacy,"run_text",side_effect=mixed_label), patch.object(legacy,"image_info",return_value={"revision_label":candidate_revision}), patch.object(legacy,"assert_candidate_app_image"), self.assertRaisesRegex(legacy.BaselineError,"database image"):
            legacy.validate_rollback_scope(expected,current,candidate_revision)

    def test_legacy_app_rollback_uses_no_build_no_deps_and_never_deletes_volumes(self):
        command=legacy.legacy_rollback_compose_command(Path("/srv/kurbis/code/.env"),Path("/srv/kurbis/code/compose.yaml"))
        self.assertEqual(command[0:2],["docker","compose"])
        self.assertIn("--no-build",command)
        self.assertIn("--no-deps",command)
        self.assertNotIn("down",command)
        self.assertNotIn("-v",command)
        self.assertNotIn("--volumes",command)
        self.assertEqual(command[-4:], ["control-plane","gateway","panel","waiting-pumpkin"])

    def test_baseline_environment_backup_is_private_and_restores_exact_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);backup_dir=root/"env-backups";env_path=root/"code/.env"
            env_path.parent.mkdir();content=b"POSTGRES_PASSWORD=secret-value\nPANEL_BIND_ADDRESS=127.0.0.1\n"
            env_path.write_bytes(content);env_path.chmod(0o640)
            inventory={"source":{"compose_env_file_sha256":legacy.sha256_bytes(content),"compose_env_file_mode":0o640,
                                  "compose_env_file_uid":0,"compose_env_file_gid":0}}
            real_stat=Path.stat
            def fake_atomic(path,data,mode,uid,gid):
                Path(path).write_bytes(data);Path(path).chmod(mode)
            def fake_stat(path,*args,**kwargs):
                result=real_stat(path,*args,**kwargs)
                if path==backup_dir:
                    return type("RootDirStat",(),{"st_uid":0,"st_mode":stat.S_IFDIR|0o700})()
                if path==backup_dir/f"compose-env-{legacy.sha256_bytes(content)}":
                    return type("RootFileStat",(),{"st_uid":0,"st_mode":stat.S_IFREG|0o600})()
                return result
            with patch.object(legacy,"ROOT",root),patch.object(legacy,"COMPOSE_ENV_BACKUPS",backup_dir), \
                 patch.object(legacy,"ENV_FILE_CHECKOUT",env_path),patch.object(legacy,"atomic_file",side_effect=fake_atomic), \
                 patch.object(Path,"stat",fake_stat):
                metadata=legacy.save_baseline_compose_env(inventory)
                self.assertEqual(metadata["sha256"],legacy.sha256_bytes(content))
                env_path.write_bytes(b"BUILD_REVISION=changed\n")
                self.assertTrue(legacy.restore_baseline_compose_env({"compose_env_backup":metadata}))
                self.assertEqual(env_path.read_bytes(),content)
                self.assertFalse(legacy.restore_baseline_compose_env({"compose_env_backup":metadata}))
                self.assertEqual(Path(metadata["path"]).read_bytes(),content)
                self.assertEqual(stat.S_IMODE(real_stat(Path(metadata["path"])).st_mode),0o600)

    def test_rollback_candidate_image_requires_release_label_and_embedded_binary_revision(self):
        revision="a"*40
        candidate_id="sha256:"+"f"*64
        with patch.object(legacy,"image_info",return_value={"revision_label":revision}), patch.object(legacy,"extract_image_payload",return_value=b"binary contains "+revision.encode()):
            legacy.assert_candidate_app_image(candidate_id,"control-plane",revision)
        with patch.object(legacy,"image_info",return_value={"revision_label":"b"*40}), self.assertRaisesRegex(legacy.BaselineError,"revision label"):
            legacy.assert_candidate_app_image(candidate_id,"gateway",revision)
        with patch.object(legacy,"image_info",return_value={"revision_label":revision}), patch.object(legacy,"extract_image_payload",return_value=b"old binary"), \
             self.assertRaisesRegex(legacy.BaselineError,"compiled release revision"):
            legacy.assert_candidate_app_image(candidate_id,"control-plane",revision)

    def test_worker_bootstrap_rejects_runtime_mismatch_before_any_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            world = Path(directory) / "world.db"
            world.write_bytes(b"server-world-must-remain-untouched")
            before = world.read_bytes()
            with patch.object(legacy, "OperationLock", NoopLock), patch.object(legacy, "require_lock"), \
                 patch.object(legacy, "load_attestation", return_value={"inventory": FIXTURE["expected_inventory"]}), \
                 patch.object(legacy, "_verify_worker_compatibility_locked", side_effect=legacy.BaselineError("plugin payload mismatch")), \
                 patch.object(legacy, "atomic_file") as atomic, patch.object(legacy.subprocess, "run") as process:
                with self.assertRaisesRegex(legacy.BaselineError, "plugin payload mismatch"):
                    legacy.bootstrap_worker(FIXTURE, "sha256:candidate", "a" * 40, "a" * 32)
            atomic.assert_not_called()
            process.assert_not_called()
            self.assertEqual(world.read_bytes(), before)

    def test_worker_bootstrap_post_restart_failure_restores_original_config_and_process(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            worker=root/"worker";env=root/"worker.env";backup_dir=root/"worker-backups"
            migration_path=root/"migration.json";world=root/"world.db"
            worker.write_bytes(b"baseline-worker-binary");worker.chmod(0o755)
            world.write_bytes(b"persistent-server-world")
            wid=FIXTURE["expected_inventory"]["worker"]["worker_id"]
            old_env=f"export WORKER_ID={wid}\nexport PUMPKIN_IMAGE={legacy.PUMPKIN_LEGACY}\n".encode()
            env.write_bytes(old_env);env.chmod(0o600)
            manifest=json.loads(json.dumps(FIXTURE))
            expected_worker=manifest["expected_inventory"]["worker"]
            worker_hash=legacy.sha256_bytes(worker.read_bytes());env_hash=legacy.sha256_bytes(old_env)
            expected_worker.update(worker_sha256=worker_hash,worker_process_sha256=worker_hash,
                                   worker_env_sha256=env_hash,worker_mode=0o755,worker_uid=0,worker_gid=0)
            candidate_revision="a"*40;candidate_tag=f"melon-pumpkin:host-control-{candidate_revision}"
            process={"PUMPKIN_IMAGE":legacy.PUMPKIN_LEGACY}
            original_file_hash=legacy.sha256_file
            def file_hash(path):
                if str(path)=="/proc/4321/exe":return original_file_hash(worker)
                return original_file_hash(Path(path))
            def write_atomic(path,content,mode,uid,gid):
                Path(path).write_bytes(content);Path(path).chmod(mode)
            def fake_run_text(args,**kwargs):
                if args[:3]==["docker","image","inspect"]:return "sha256:verified-candidate"
                if args[:3]==["systemctl","show","--property=MainPID"]:return "4321"
                if args[:2]==["systemctl","is-active"]:return "active"
                raise AssertionError(args)
            def fake_subprocess(args,**kwargs):
                if args[:3]==["docker","image","tag"]:
                    return type("Completed",(),{"returncode":0,"stdout":b"","stderr":b""})()
                if args[:2]==["systemctl","restart"]:
                    value=next((line.split("=",1)[1] for line in env.read_text().splitlines() if line.startswith("export PUMPKIN_IMAGE=")),"")
                    process["PUMPKIN_IMAGE"]=value
                    return type("Completed",(),{"returncode":0,"stdout":b"","stderr":b""})()
                raise AssertionError(args)
            lease=[legacy.BaselineError("simulated lease verification failure"),"t"]
            def fake_journal(path,value,mode):Path(path).write_text(json.dumps(value,sort_keys=True));Path(path).chmod(mode)
            original_path_stat=Path.stat
            def fake_path_stat(path,*args,**kwargs):
                if path==backup_dir:return type("RootBackupDirectoryStat",(),{"st_uid":0,"st_mode":stat.S_IFDIR|0o700})()
                return original_path_stat(path,*args,**kwargs)
            with patch.object(legacy,"OperationLock",NoopLock),patch.object(legacy,"require_lock"), \
                 patch.object(legacy,"ENV_FILE",env),patch.object(legacy,"WORKER",worker),patch.object(legacy,"WORKER_BACKUP",backup_dir), \
                 patch.object(Path,"stat",fake_path_stat), \
                 patch.object(legacy,"WORKER_MIGRATION",migration_path),patch.object(legacy,"load_attestation",return_value={"inventory":manifest["expected_inventory"],"manifest_sha256":"f"*64}), \
                 patch.object(legacy,"_verify_worker_compatibility_locked",return_value={"status":"compatible"}),patch.object(legacy,"migration_record",return_value=None), \
                 patch.object(legacy,"run_text",side_effect=fake_run_text),patch.object(legacy,"subprocess") as subprocess_module, \
                 patch.object(legacy,"atomic_file",side_effect=write_atomic),patch.object(legacy,"atomic_json",side_effect=fake_journal), \
                 patch.object(legacy,"worker_process_environment",side_effect=lambda pid:dict(process)),patch.object(legacy,"worker_lease",side_effect=lease), \
                 patch.object(legacy,"sha256_file",side_effect=file_hash),patch.object(legacy.time,"sleep"):
                subprocess_module.run.side_effect=fake_subprocess
                with self.assertRaisesRegex(legacy.BaselineError,"restored"):
                    legacy.bootstrap_worker(manifest,"sha256:verified-candidate",candidate_revision,"a"*32)
            self.assertEqual(env.read_bytes(),old_env)
            self.assertEqual(process["PUMPKIN_IMAGE"],legacy.PUMPKIN_LEGACY)
            self.assertEqual(worker.read_bytes(),b"baseline-worker-binary")
            self.assertEqual(world.read_bytes(),b"persistent-server-world")
            self.assertEqual(json.loads(migration_path.read_text())["status"],"rolled_back")


    def test_legacy_rollback_requires_new_process_and_heartbeat_renewal_across_stale_window(self):
        worker_id=FIXTURE["expected_inventory"]["worker"]["worker_id"]
        base=dt.datetime(2026,10,4,tzinfo=dt.timezone.utc)
        clock=[0.0]
        def lease_snapshot(_worker_id,_postgres_id):
            if clock[0]<6:raise legacy.BaselineError("previous lease expired while worker restarts")
            elapsed=int(clock[0]//8)*8
            heartbeat=(base+dt.timedelta(seconds=elapsed)).isoformat().replace("+00:00","Z")
            now=(base+dt.timedelta(seconds=clock[0])).isoformat(timespec="microseconds").replace("+00:00","Z")
            return {"raw":f"{worker_id}|t|t|1","heartbeat_at":heartbeat,"database_now":now}
        def sleep(seconds):clock[0]+=seconds
        with patch.object(legacy,"systemd_worker_pid",return_value=9876), \
             patch.object(legacy,"run_text",return_value="active"), \
             patch.object(legacy,"read_process_start_ticks",return_value="new-start-ticks"), \
             patch.object(legacy,"worker_process_environment",return_value={"WORKER_ID":worker_id,"PUMPKIN_IMAGE":legacy.PUMPKIN_LEGACY}), \
             patch.object(legacy,"sha256_file",return_value="a"*64), \
             patch.object(legacy,"worker_lease_snapshot",side_effect=lease_snapshot), \
             patch.object(legacy,"ENV_FILE",Path("/fake/worker.env")), \
             patch.object(legacy.time,"monotonic",side_effect=lambda:clock[0]), \
             patch.object(legacy.time,"sleep",side_effect=sleep):
            verified=legacy.verify_restarted_worker(worker_id,"postgres",previous_pid=1234,
                previous_start_ticks="old-start-ticks",previous_heartbeat_at=base.isoformat().replace("+00:00","Z"),
                expected_worker_sha256="a"*64,expected_env_sha256="a"*64,
                expected_pumpkin_image=legacy.PUMPKIN_LEGACY)
        self.assertEqual(verified["pid"],9876)
        self.assertEqual(verified["start_ticks"],"new-start-ticks")
        self.assertGreaterEqual(clock[0],35)
        self.assertGreater(dt.datetime.fromisoformat(verified["heartbeat_at"].replace("Z","+00:00")),base)

    def test_legacy_rollback_rejects_stale_pre_restart_heartbeat_even_with_new_pid(self):
        worker_id=FIXTURE["expected_inventory"]["worker"]["worker_id"]
        base=dt.datetime(2026,10,4,tzinfo=dt.timezone.utc)
        clock=[0.0]
        timestamp=base.isoformat().replace("+00:00","Z")
        def sleep(seconds):clock[0]+=seconds
        def stale_snapshot(_worker_id,_postgres_id):
            now=(base+dt.timedelta(seconds=clock[0])).isoformat(timespec="microseconds").replace("+00:00","Z")
            return {"raw":f"{worker_id}|t|t|1","heartbeat_at":timestamp,"database_now":now}
        with patch.object(legacy,"systemd_worker_pid",return_value=9876), \
             patch.object(legacy,"run_text",return_value="active"), \
             patch.object(legacy,"read_process_start_ticks",return_value="new-start-ticks"), \
             patch.object(legacy,"worker_process_environment",return_value={"WORKER_ID":worker_id,"PUMPKIN_IMAGE":legacy.PUMPKIN_LEGACY}), \
             patch.object(legacy,"sha256_file",return_value="a"*64), \
             patch.object(legacy,"ENV_FILE",Path("/fake/worker.env")), \
             patch.object(legacy,"worker_lease_snapshot",side_effect=stale_snapshot), \
             patch.object(legacy.time,"monotonic",side_effect=lambda:clock[0]), \
             patch.object(legacy.time,"sleep",side_effect=sleep), \
             self.assertRaisesRegex(legacy.BaselineError,"fresh PID/start time and renewing lease"):
            legacy.verify_restarted_worker(worker_id,"postgres",previous_pid=1234,previous_start_ticks="old-start-ticks",
                previous_heartbeat_at=timestamp,expected_worker_sha256="a"*64,expected_env_sha256="a"*64,
                expected_pumpkin_image=legacy.PUMPKIN_LEGACY,timeout_seconds=40)
        self.assertGreaterEqual(clock[0],40)

    def test_retry_inventory_allows_only_exact_candidate_recorded_in_worker_journal(self):
        revision="a"*40
        migration={"status":"migrated","candidate_revision":revision,
                   "candidate_tag":f"melon-pumpkin:host-control-{revision}",
                   "candidate_image_id":"sha256:"+"b"*64}
        with patch.object(legacy,"migration_record",return_value=migration):
            self.assertEqual(legacy.authorized_worker_images_for_inventory(FIXTURE),
                             {legacy.PUMPKIN_LEGACY,migration["candidate_tag"]})
        migration["candidate_tag"]="melon-pumpkin:host-control-"+"c"*40
        with patch.object(legacy,"migration_record",return_value=migration), \
             self.assertRaisesRegex(legacy.BaselineError,"candidate identity is invalid"):
            legacy.authorized_worker_images_for_inventory(FIXTURE)

    def test_lost_bootstrap_response_reconciles_a_journaled_migrated_worker(self):
        migration={"status":"migrated","candidate_tag":"melon-pumpkin:host-control-"+"a"*40,
                   "candidate_image_id":"sha256:candidate","candidate_revision":"a"*40}
        with patch.object(legacy,"require_lock"),patch.object(legacy,"load_attestation",return_value={"inventory":FIXTURE["expected_inventory"],"manifest_sha256":"f"*64}), \
             patch.object(legacy,"migration_record",return_value=migration), \
             patch.object(legacy,"verify_worker_compatibility",return_value={"worker_lease":"worker|t|t|1"}):
            result=legacy.reconcile_worker_bootstrap(FIXTURE,"a"*32)
        self.assertEqual(result["status"],"migrated-reconciled")
        self.assertEqual(result["candidate_revision"],"a"*40)
        self.assertEqual(result["worker_lease"],"worker|t|t|1")

    def test_migrated_worker_bootstrap_retry_is_idempotent(self):
        revision = "a" * 40
        tag = f"melon-pumpkin:host-control-{revision}"
        migration = {"status": "migrated", "candidate_tag": tag, "candidate_image_id": "sha256:candidate"}
        with patch.object(legacy, "OperationLock", NoopLock), patch.object(legacy, "require_lock"), \
             patch.object(legacy, "load_attestation", return_value={"inventory": FIXTURE["expected_inventory"]}), \
             patch.object(legacy, "_verify_worker_compatibility_locked") as compatibility, patch.object(legacy, "migration_record", return_value=migration), \
             patch.object(legacy, "worker_values", return_value={"PUMPKIN_IMAGE": tag}), \
             patch.object(legacy, "run_text", return_value="1234"), patch.object(legacy, "worker_process_environment", return_value={"PUMPKIN_IMAGE": tag}), \
             patch.object(legacy, "atomic_file") as atomic, patch.object(legacy.subprocess, "run") as process:
            result = legacy.bootstrap_worker(FIXTURE, "sha256:candidate", revision, "a" * 32)
        self.assertEqual(result, migration)
        compatibility.assert_called_once()
        atomic.assert_not_called()
        process.assert_not_called()


if __name__ == "__main__":
    unittest.main()
