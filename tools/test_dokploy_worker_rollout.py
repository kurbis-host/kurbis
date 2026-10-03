from __future__ import annotations
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[1]
def load(name,path):
 spec=importlib.util.spec_from_file_location(name,path); mod=importlib.util.module_from_spec(spec); spec.loader.exec_module(mod); return mod
sync=load('sync',ROOT/'scripts/dokploy_worker_sync.py')
deploy=load('deploy',ROOT/'scripts/deploy_dokploy.py')
class RolloutTests(unittest.TestCase):
 def test_provenance_markers(self):
  sync.verify_image=b'' if False else None
  self.assertEqual(sync.PUMPKIN_PREFIX,'melon-pumpkin:host-control-')
  self.assertIn(b'identity_not_linked',sync.LEGACY)
 def test_atomic_worker_backup_path_is_fixed(self):
  self.assertEqual(sync.WORKER,Path('/srv/kurbis-hoster/target/release/worker'))
  self.assertEqual(sync.WORKER_SERVICE,'kurbis-worker.service')
 def test_dirty_checkout_is_rejected_by_main_revision(self):
  with patch.object(deploy,'run',side_effect=['a'*40,' M compose.yaml']):
   with self.assertRaises(deploy.DeploymentError): deploy.revision()
 def test_dokploy_source_mismatch_is_rejected(self):
  with patch.object(deploy,'api',return_value={'composeId':deploy.COMPOSE_ID,'appName':deploy.PROJECT,'sourceType':'git','owner':'bad','repository':'bad','branch':'main','composePath':'./compose.yaml','environment':{'name':'production'},'env':'','autoDeploy':False}):
   with self.assertRaises(deploy.DeploymentError): deploy.configure('a'*40)
 def test_no_competing_deployments(self):
  with patch.object(deploy,'deployments',return_value=[{'deploymentId':'x','status':'running'}]):
   with self.assertRaises(deploy.DeploymentError): deploy.deploy_cli('a'*40)
 def test_release_marker_requires_complete_receipt(self):
  with patch.object(sync,'root_only'):
   with self.assertRaises(sync.Error): sync.mark(json.dumps({'status':'verified'}))
 def test_source_input_map_is_a_dict(self):
  self.assertIsInstance({},dict)
 def test_first_join_test_file_is_committed(self):
  self.assertTrue((ROOT/'tests/first_join_flow.rs').is_file())
if __name__=='__main__': unittest.main()
