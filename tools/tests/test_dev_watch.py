import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev
import dev_watch


class WatchTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.repo = self.root / 'repo'
        self.overrides = [patch.object(dev, 'REPO', self.repo), patch.object(dev, 'STATE', self.root / 'state')]
        for override in self.overrides:
            override.start()
        for directory in ('core/src', 'core/migrations', 'ytm-resolver/app', 'resolver-client-generated/src', 'subsonic-types-generated/src'):
            (self.repo / 'apps' / directory).mkdir(parents=True)
        (self.repo / 'apps/core/src/main.rs').write_text('fn main() {}')
        (self.repo / 'apps/core/migrations/0001_init.sql').write_text('SELECT 1;')
        for name in ('pyproject.toml', 'uv.lock', 'README.md'):
            (self.repo / 'apps/ytm-resolver' / name).write_text('fixture')
        (self.repo / 'apps/ytm-resolver/app/main.py').write_text('value = 1')
        (self.repo / 'docker').mkdir()
        for name in ('local.Dockerfile', 'ytm-resolver.Dockerfile'):
            (self.repo / 'docker' / name).write_text('FROM scratch\n')
        self.path = dev.state_root() / 'profiles/dev'
        self.path.mkdir()
        (self.path / 'logs').mkdir()
        self.profile = {'name': 'dev', 'build': 'a' * 64, 'full': False, 'ports': {'api': 3100}}
        dev.write_json(self.path / 'profile.json', self.profile)

    def tearDown(self):
        for override in reversed(self.overrides):
            override.stop()
        self.temporary.cleanup()

    def test_python_edits_do_not_change_rust_identity(self):
        before = dev_watch.components()
        (self.repo / 'apps/ytm-resolver/app/main.py').write_text('value = 2')
        after = dev_watch.components()
        self.assertEqual(before['rust'], after['rust'])
        self.assertNotEqual(before['python'], after['python'])
        self.assertEqual(before['dependencies'], after['dependencies'])

    def test_migration_and_dependency_changes_block_build_and_restart(self):
        for kind in ('migrations', 'dependencies'):
            baseline = dev_watch.components()
            current = dict(baseline, **{kind: 'changed'})
            with patch.object(dev_watch, 'installed_components', return_value=baseline), \
                 patch.object(dev_watch, 'components', return_value=current), \
                 patch.object(dev, 'build') as build, patch.object(dev_watch, 'replace') as replace:
                dev_watch.reload(self.path, self.profile)
            build.assert_not_called()
            replace.assert_not_called()

    def test_failed_compile_does_not_dump_stop_or_replace(self):
        baseline = dev_watch.components()
        with patch.object(dev_watch, 'installed_components', return_value=baseline), \
             patch.object(dev, 'current_identity', return_value=self.profile['build']), \
             patch.object(dev, 'build', side_effect=RuntimeError('compile failed')), \
             patch.object(dev, 'backup') as backup, patch.object(dev, 'stop') as stop, \
             patch.object(dev_watch, 'replace') as replace:
            with self.assertRaisesRegex(RuntimeError, 'compile failed'):
                dev_watch.reload(self.path, self.profile)
        backup.assert_not_called()
        stop.assert_not_called()
        replace.assert_not_called()

    def test_two_watchers_cannot_own_one_profile(self):
        with dev_watch.watcher_owner(self.path):
            with self.assertRaisesRegex(RuntimeError, 'already owns'):
                with dev_watch.watcher_owner(self.path):
                    pass
        self.assertFalse((self.path / 'watcher.json').exists())

    def test_another_checkout_cannot_replace_owned_profile(self):
        with dev_watch.watcher_owner(self.path):
            with patch.object(dev, 'REPO', self.root / 'other'):
                with self.assertRaisesRegex(RuntimeError, 'another checkout'):
                    dev_watch.owner(self.path)

    def test_stopped_profile_is_not_resurrected_by_a_save(self):
        with dev_watch.watcher_owner(self.path) as token:
            with patch.object(dev_watch, 'reload') as reload:
                dev_watch.watch_step(self.path, self.profile, token)
            reload.assert_not_called()

    def test_personal_watch_and_migrate_are_refused(self):
        with self.assertRaisesRegex(RuntimeError, 'personal'):
            dev_watch.editable({'name': 'personal'})

    def test_changed_applied_migration_is_reported(self):
        document = [{'version': 1, 'success': True, 'checksum': 'incorrect'}]
        results = [subprocess.CompletedProcess([], 0, 't\n', ''), subprocess.CompletedProcess([], 0, json.dumps(document), '')]
        with patch.object(dev, 'owned', return_value={'State': {'Running': True}}), \
             patch.object(subprocess, 'run', side_effect=results):
            self.assertTrue(dev_watch.migration_status(self.path, self.profile))

    def test_pending_migration_and_valid_applied_checksum(self):
        file = self.repo / 'apps/core/migrations/0001_init.sql'
        document = [{'version': 1, 'success': True, 'checksum': hashlib.sha384(file.read_bytes()).hexdigest()}]
        (file.parent / '0002_next.sql').write_text('SELECT 2;')
        results = [subprocess.CompletedProcess([], 0, 't\n', ''), subprocess.CompletedProcess([], 0, json.dumps(document), '')]
        with patch.object(dev, 'owned', return_value={'State': {'Running': True}}), \
             patch.object(subprocess, 'run', side_effect=results):
            self.assertFalse(dev_watch.migration_status(self.path, self.profile))


if __name__ == '__main__':
    unittest.main()
