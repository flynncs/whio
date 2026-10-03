from contextlib import contextmanager
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
import dev_checks
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
             patch.object(dev, 'build', side_effect=RuntimeError('compile failed')), \
             patch.object(dev, 'backup') as backup, patch.object(dev, 'stop') as stop, \
             patch.object(dev_watch, 'replace') as replace:
            with self.assertRaisesRegex(RuntimeError, 'compile failed'):
                dev_watch.reload(self.path, self.profile)
        backup.assert_not_called()
        stop.assert_not_called()
        replace.assert_not_called()

    def test_older_runtime_paths_are_converted_even_with_unchanged_source(self):
        self.profile['resolver_snapshot'] = True
        directory = dev.state_root() / 'builds' / self.profile['build']
        directory.mkdir()
        baseline = dev_watch.components()
        dev.write_json(directory / 'build.json', {'components': baseline})
        with patch.object(dev_watch, 'installed_components', return_value=baseline), \
             patch.object(dev, 'source_identity', return_value=self.profile['build']), \
             patch.object(dev, 'build') as build, patch.object(dev_watch, 'replace') as replace:
            dev_watch.reload(self.path, self.profile)
        build.assert_called_once()
        replace.assert_called_once()

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
            with patch.object(dev, 'socket_path', return_value=self.root / 'missing.sock'), \
                 patch.object(dev_watch, 'reload') as reload:
                dev_watch.watch_step(self.path, self.profile, token)
            reload.assert_not_called()

    def test_shutdown_waits_for_owned_processes_after_socket_disappears(self):
        socket = self.root / 'running.sock'
        socket.touch()
        def supervisor(path, profile, *args):
            if args[0] == 'down':
                socket.unlink()
                return ''
            return json.dumps([{'pid': 123, 'is_running': True}])
        with patch.object(dev, 'socket_path', return_value=socket), \
             patch.object(dev, 'supervisor', side_effect=supervisor), patch.object(dev, 'owned', return_value=None), \
             patch.object(dev, 'process_alive', side_effect=[True, False, False]), patch.object(dev.time, 'sleep') as sleep:
            dev.stop(self.path, self.profile, database=False)
        sleep.assert_called_once()

    def test_personal_watch_and_migrate_are_refused(self):
        with self.assertRaisesRegex(RuntimeError, 'personal'):
            dev_watch.editable({'name': 'personal'})

    def test_import_failure_keeps_live_configuration(self):
        configuration = self.path / 'process-compose.json'
        configuration.write_text('old configuration')
        def config(*args):
            configuration.write_text('candidate configuration')
        with patch.object(dev, 'socket_path', return_value=self.root / 'missing.sock'), \
             patch.object(dev, 'config', side_effect=config), \
             patch.object(dev_watch, 'validate_resolvers', side_effect=RuntimeError('import failed')), \
             patch.object(dev, 'supervisor') as supervisor:
            with self.assertRaisesRegex(RuntimeError, 'import failed'):
                dev_watch.replace(self.path, self.profile, dict(self.profile))
        self.assertEqual(configuration.read_text(), 'old configuration')
        self.assertEqual((self.path / 'current-rust').resolve(), (dev.state_root() / 'builds' / self.profile['build']).resolve())
        supervisor.assert_not_called()

    def test_python_replacement_restarts_only_resolvers_and_retains_database(self):
        previous = dict(self.profile, full=True)
        candidate = dict(previous, build='b' * 64)
        for build, python in ((previous['build'], 'old'), (candidate['build'], 'new')):
            directory = dev.state_root() / 'builds' / build
            directory.mkdir()
            dev.write_json(directory / 'build.json', {'components': {'rust': 'same', 'python': python}})
        configuration = {'processes': {name: {'command': name} for name in ('api', 'worker', 'youtube', 'metadata')}}
        dev.write_json(self.path / 'process-compose.json', configuration)
        socket = self.root / 'running.sock'
        socket.touch()
        with patch.object(dev, 'socket_path', return_value=socket), patch.object(dev, 'ready', return_value=True), \
             patch.object(dev_watch, 'process_states', return_value={name: {'pid': index} for index,name in enumerate(configuration['processes'])}), \
             patch.object(dev, 'config'), patch.object(dev_watch, 'validate_resolvers'), \
             patch.object(dev, 'supervisor') as supervisor, patch.object(dev_watch, 'wait_ready'), \
             patch.object(dev, 'create_user'), patch.object(dev, 'stop') as stop, patch.object(dev, 'backup') as backup:
            dev_watch.replace(self.path, previous, candidate)
        self.assertEqual([call.args[-1] for call in supervisor.call_args_list], ['youtube', 'metadata'])
        stop.assert_not_called()
        backup.assert_not_called()

    def test_startup_failure_restores_previous_profile_and_process_config(self):
        previous = dict(self.profile)
        candidate = dict(previous, build='b' * 64)
        configuration = self.path / 'process-compose.json'
        original = {'processes': {'api': {'command': 'old'}}}
        dev.write_json(configuration, original)
        socket = self.root / 'running.sock'
        socket.touch()
        for build, rust in ((previous['build'], 'old'), (candidate['build'], 'new')):
            directory = dev.state_root() / 'builds' / build
            directory.mkdir()
            dev.write_json(directory / 'build.json', {'components': {'rust': rust}})
        def config(*args):
            dev.write_json(configuration, original)
        with patch.object(dev, 'socket_path', return_value=socket), patch.object(dev, 'ready', return_value=True), \
             patch.object(dev_watch, 'process_states', return_value={'api': {'pid': 1}}), \
             patch.object(dev, 'config', side_effect=config), patch.object(dev_watch, 'validate_resolvers'), \
             patch.object(dev, 'supervisor') as supervisor, \
             patch.object(dev_watch, 'wait_ready', side_effect=[RuntimeError('not ready'), None]), \
             patch.object(dev, 'stop') as stop:
            with self.assertRaisesRegex(RuntimeError, 'not ready'):
                dev_watch.replace(self.path, previous, candidate)
        self.assertEqual(json.loads(configuration.read_text()), original)
        self.assertEqual(json.loads((self.path / 'profile.json').read_text()), previous)
        self.assertEqual(supervisor.call_count, 2)
        stop.assert_not_called()

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

    def test_python_only_install_reuses_binaries_without_cargo(self):
        old = dev.state_root() / 'builds' / self.profile['build']
        old.mkdir()
        dev.write_json(old / 'build.json', {'components': dev_watch.components()})
        for name in dev.BINARIES:
            (old / name).write_text('installed binary')
        (self.repo / 'apps/ytm-resolver/app/main.py').write_text('value = 2')
        cache = self.root / 'cache'
        (cache / 'targets').mkdir(parents=True)
        @contextmanager
        def admission(*args):
            yield 0, ()
        with patch.object(dev_checks, 'runtime_root', return_value=cache), \
             patch.object(dev_checks, 'admission', admission), patch.object(dev_checks, 'prune_caches'), \
             patch.object(dev_checks, 'check_storage'), patch.object(dev_checks, 'run_tests') as cargo, \
             patch.object(subprocess, 'check_output', return_value='fixture-head'):
            candidate = dict(self.profile)
            dev.build(self.path, candidate, persist=False)
        cargo.assert_not_called()
        self.assertNotEqual(candidate['build'], self.profile['build'])
        installed = dev.state_root() / 'builds' / candidate['build']
        self.assertEqual((installed / 'whio-api').read_text(), 'installed binary')
        self.assertEqual(json.loads((self.path / 'profile.json').read_text()), self.profile)


if __name__ == '__main__':
    unittest.main()
