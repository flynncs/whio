from contextlib import contextmanager, redirect_stderr, redirect_stdout
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev
import dev_docker
import dev_watch


@contextmanager
def unlocked():
    yield


def commit_fixture(root, name, branch=None):
    repo = root / name
    (repo / 'apps/core/src').mkdir(parents=True)
    (repo / 'apps/core/src/main.rs').write_text('fn main() {}')
    (repo / 'rust-toolchain.toml').write_text('channel = "stable"\n')
    git = ['git', '-c', 'user.email=dev@example.invalid', '-c', 'user.name=dev']
    subprocess.run([*git, 'init', '-q', str(repo)], check=True)
    subprocess.run([*git, '-C', str(repo), 'add', '-A'], check=True)
    subprocess.run([*git, '-C', str(repo), 'commit', '-qm', 'fixture'], check=True)
    if branch:
        subprocess.run([*git, '-C', str(repo), 'branch', '-M', branch], check=True)
    revision = subprocess.run([*git, '-C', str(repo), 'rev-parse', 'HEAD'],
                              capture_output=True, text=True, check=True).stdout.strip()
    return repo, revision


class CommandSurfaceTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.override = patch.object(dev, 'STATE', self.root / 'state')
        self.override.start()
        self.addCleanup(self.override.stop)

    def test_removed_verbs_are_rejected(self):
        for verb in ('setup', 'start', 'stop', 'reload', 'upgrade', 'backup-check', 'login',
                     'cleanup', 'watch', 'migrate', 'monitor', 'services', 'resources'):
            with self.subTest(verb=verb), patch.object(sys, 'argv', ['dev.py', verb]):
                with self.assertRaises(SystemExit) as result:
                    dev.main()
                self.assertEqual(result.exception.code, 2)

    def test_flags_apply_only_to_their_command(self):
        cases = (['dev.py', 'status', 'personal', '--service', 'api'],
                 ['dev.py', 'logs', 'personal', '--watch-token', 'x'],
                 ['dev.py', 'status', 'personal', '--full'])
        for arguments in cases:
            with self.subTest(arguments=arguments), patch.object(sys, 'argv', arguments):
                with self.assertRaises(SystemExit) as result:
                    dev.main()
                self.assertEqual(result.exception.code, 2)

    def test_errors_name_the_subcommand_that_failed(self):
        errors = io.StringIO()
        with patch.object(sys, 'argv', ['dev.py', 'down', 'personal']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', side_effect=RuntimeError('boom')), \
             redirect_stderr(errors):
            self.assertEqual(dev.main(), 1)
        self.assertEqual(errors.getvalue().strip(), 'down failed: boom')

    def test_logs_defaults_to_every_service(self):
        profile = {'name': 'personal', 'runtime': 'docker', 'build': 'a' * 64, 'ports': {'api': 3000}}
        for arguments, service in ((['dev.py', 'logs', 'personal'], None),
                                   (['dev.py', 'logs', 'personal', '--service', 'postgres'], 'postgres')):
            with self.subTest(arguments=arguments), \
                 patch.object(sys, 'argv', arguments), \
                 patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
                 patch.object(dev_docker, 'observe') as observe:
                dev.main()
            observe.assert_called_once_with(self.root, profile, 'logs', service)

    def test_native_profile_is_refused(self):
        path = dev.state_root() / 'profiles' / 'personal'
        path.mkdir()
        dev.write_json(path / 'profile.json', {'name': 'personal', 'uid': os.getuid(), 'runtime': 'native'})
        errors = io.StringIO()
        with patch.object(sys, 'argv', ['dev.py', 'down', 'personal']), redirect_stderr(errors):
            self.assertEqual(dev.main(), 1)
        self.assertEqual(errors.getvalue().strip(),
                         'down failed: native profiles are no longer supported; recreate it')

    def test_up_builds_once_then_starts_the_full_stack(self):
        profile = {'name': 'personal', 'runtime': 'docker', 'build': None, 'ports': {'api': 3000}}
        with patch.object(sys, 'argv', ['dev.py', 'up', 'personal']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
             patch.object(dev, 'cleanup') as cleanup, \
             patch.object(dev_docker, 'configure_profile') as configure, \
             patch.object(dev, 'build') as build, \
             patch.object(dev, 'start') as start, \
             patch.object(dev, 'create_user') as create_user:
            dev.main()
        cleanup.assert_called_once_with()
        configure.assert_called_once_with(self.root, profile)
        build.assert_called_once_with(self.root, profile)
        start.assert_called_once_with(self.root, profile, True)
        create_user.assert_called_once_with(self.root, profile)

    def test_up_starts_an_installed_build_without_rebuilding(self):
        profile = {'name': 'personal', 'runtime': 'docker', 'build': 'b' * 64, 'ports': {'api': 3000}}
        with patch.object(sys, 'argv', ['dev.py', 'up', 'personal']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
             patch.object(dev, 'cleanup'), patch.object(dev, 'build') as build, \
             patch.object(dev, 'start') as start, patch.object(dev, 'create_user'):
            dev.main()
        build.assert_not_called()
        start.assert_called_once_with(self.root, profile, True)

    def test_backup_verifies_the_backup_it_just_made(self):
        profile = {'name': 'personal', 'runtime': 'docker', 'build': 'b' * 64, 'ports': {'api': 3000}}
        with patch.object(sys, 'argv', ['dev.py', 'backup', 'personal']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
             patch.object(dev, 'backup') as backup, \
             patch.object(dev, 'backup_check') as backup_check:
            dev.main()
        backup.assert_called_once_with(self.root, profile)
        backup_check.assert_called_once_with(self.root, profile)

    def test_hidden_watch_step_is_still_routable(self):
        profile = {'name': 'dev', 'runtime': 'docker', 'build': 'c' * 64, 'ports': {'api': 3100}}
        with patch.object(sys, 'argv', ['dev.py', 'watch-step', 'dev', '--watch-token', 'token']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
             patch.object(dev_watch, 'watch_step') as watch_step:
            dev.main()
        watch_step.assert_called_once_with(self.root, profile, 'token')

    def test_overview_prints_one_line_per_profile(self):
        root = dev.state_root()
        fixtures = (('dev', 'a' * 64, 3100),
                    ('personal', 'b' * 64, 3000),
                    ('preview-stopped', None, 3999))
        for name, build, port in fixtures:
            path = root / 'profiles' / name
            path.mkdir()
            dev.write_json(path / 'profile.json', {'name': name, 'build': build, 'ports': {'api': port}})
        output = io.StringIO()
        with patch.object(sys, 'argv', ['dev.py', 'status']), \
             patch.object(dev, 'ready', side_effect=lambda port: port == 3000), \
             patch.object(dev, 'running', side_effect=lambda path, profile: profile['name'] != 'preview-stopped'), \
             redirect_stdout(output):
            self.assertEqual(dev.main(), 0)
        self.assertEqual(output.getvalue().splitlines(), [
            'dev  http://127.0.0.1:3100  aaaaaaaaaaaa  running',
            'personal  http://127.0.0.1:3000  bbbbbbbbbbbb  ready',
            'preview-stopped  http://127.0.0.1:3999  none  stopped',
        ])

    def test_source_identity_matches_an_export_but_not_uncommitted_changes(self):
        repo, _ = commit_fixture(self.root, 'identity-repo')
        export = self.root / 'export'
        export.mkdir()
        subprocess.run(f'git -C {repo} archive HEAD | tar -x -C {export}', shell=True, check=True)
        with patch.object(dev, 'REPO', repo):
            exported = dev.source_identity(export)
            self.assertEqual(dev.source_identity(), exported)
            (repo / 'apps/core/src/main.rs').write_text('fn main() { /* uncommitted */ }')
            self.assertNotEqual(dev.source_identity(), exported)

    def test_deploy_without_a_main_branch_fails_before_building(self):
        profile = {'name': 'personal', 'runtime': 'docker', 'build': 'a' * 64, 'full': True,
                   'ports': {'api': 3000}}
        repo, _ = commit_fixture(self.root, 'no-main', branch='other')
        errors = io.StringIO()
        with patch.object(sys, 'argv', ['dev.py', 'deploy', 'personal']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
             patch.object(dev, 'REPO', repo), \
             patch.object(dev_docker, 'upgrade') as upgrade, redirect_stderr(errors):
            self.assertEqual(dev.main(), 1)
        self.assertEqual(errors.getvalue().strip(), 'deploy failed: no main branch')
        upgrade.assert_not_called()

    def test_deploy_builds_from_the_main_commit_not_the_working_tree(self):
        repo, revision = commit_fixture(self.root, 'deploy-repo', branch='main')
        (repo / 'apps/core/src/main.rs').write_text('fn main() { /* uncommitted */ }')
        profile = {'name': 'personal', 'runtime': 'docker', 'build': 'a' * 64, 'full': True,
                   'ports': {'api': 3000}}
        upgraded = []
        def record(path, profile, root=None, commit=None):
            upgraded.append((root, commit, (root / 'apps/core/src/main.rs').read_text()))
        output = io.StringIO()
        with patch.object(sys, 'argv', ['dev.py', 'deploy', 'personal']), \
             patch.object(dev, 'state_lock', unlocked), \
             patch.object(dev, 'load_profile', return_value=(self.root, profile)), \
             patch.object(dev, 'REPO', repo), patch.object(dev_docker, 'upgrade', side_effect=record), \
             redirect_stdout(output):
            self.assertEqual(dev.main(), 0)
        self.assertEqual(output.getvalue().splitlines()[0], f'deploying main {revision[:12]}')
        root, commit, source = upgraded[0]
        self.assertEqual(commit, revision)
        self.assertEqual(source, 'fn main() {}')
        self.assertFalse(root.exists())

    def test_cleanup_is_silent_and_clean_prints_one_line(self):
        output = io.StringIO()
        with patch.object(dev_docker, 'cleanup'), \
             patch.object(dev, 'prune_installed_builds'), \
             patch.object(dev.checks, 'prune_caches'), \
             patch.object(dev.checks, 'check_storage'), \
             redirect_stdout(output):
            dev.cleanup()
            self.assertEqual(output.getvalue(), '')
            dev.clean()
        self.assertEqual(output.getvalue(), 'clean complete; expired previews and idle caches reclaimed\n')


if __name__ == '__main__':
    unittest.main()
