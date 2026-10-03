from contextlib import contextmanager
import json
import os
import plistlib
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev
import dev_checks
import dev_media


class StateTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.state = self.root / 'state'
        self.override = patch.object(dev, 'STATE', self.state)
        self.override.start()

    def tearDown(self):
        self.override.stop()
        self.temporary.cleanup()

    def test_media_import_requires_explicit_selection_before_mutating_state(self):
        with patch.object(sys, 'argv', ['dev.py', 'media-import']), \
             patch.object(dev, 'state_lock') as lock, \
             patch.object(dev_media, 'import_media') as importer:
            with self.assertRaises(SystemExit) as result:
                dev.main()
            self.assertEqual(result.exception.code, 2)
            lock.assert_not_called()
            importer.assert_not_called()

    def test_nonempty_unowned_state_is_preserved(self):
        self.state.mkdir()
        original = self.state / 'important'
        original.write_text('keep')
        with self.assertRaisesRegex(RuntimeError, 'adopt'):
            dev.state_root()
        self.assertEqual(original.read_text(), 'keep')

    def test_wrong_owner_is_refused(self):
        dev.state_root()
        dev.write_json(self.state / 'owner.json', {'uid': -1})
        with self.assertRaisesRegex(RuntimeError, 'owner'):
            dev.state_root()

    def test_symlink_state_is_refused(self):
        destination = self.root / 'elsewhere'
        destination.mkdir()
        self.state.symlink_to(destination)
        with self.assertRaisesRegex(RuntimeError, 'symlink'):
            dev.state_root()
        self.assertFalse(any(destination.iterdir()))

    def test_occupied_port_remains_usable(self):
        with socket.socket() as server:
            server.bind(('127.0.0.1', 0))
            server.listen()
            port = server.getsockname()[1]
            with self.assertRaisesRegex(RuntimeError, 'occupied'):
                dev.check_ports({'api': port})
            with socket.create_connection(('127.0.0.1', port), timeout=1):
                pass

    def test_traversal_cannot_name_a_profile(self):
        for value in ('../personal', '/tmp/elsewhere', 'preview-../bad', 'personal-other'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                dev.profile_name(value)

    def test_external_credentials_are_not_inherited(self):
        with patch.dict(os.environ, {'WHIO_DATABASE_URL': 'private', 'AUDIODB_API_KEY': 'private',
                                     'PC_CONFIG_FILES': '/private', 'PATH': '/bin'}):
            environment = dev.clean_environment()
        self.assertEqual(environment['PATH'], '/bin')
        for name in ('WHIO_DATABASE_URL', 'AUDIODB_API_KEY', 'PC_CONFIG_FILES'):
            self.assertNotIn(name, environment)

    def test_unowned_database_is_not_removed(self):
        other = {'Config': {'Labels': {'dev.lumen.local-profile': 'somebody-else'}}}
        with patch.object(dev, 'inspect', return_value=other), patch.object(dev, 'docker_operation') as mutation:
            with self.assertRaisesRegex(RuntimeError, 'another owner'):
                dev.owned('container', 'unrelated', {'name': 'personal'})
            mutation.assert_not_called()

    def test_build_failure_keeps_installed_profile(self):
        root = dev.state_root()
        path = root / 'profiles/personal'
        path.mkdir()
        profile = {'name': 'personal', 'build': 'old'}
        dev.write_json(path / 'profile.json', profile)
        cache = self.root / 'cache'
        for directory in ('locks', 'targets', 'logs'):
            (cache / directory).mkdir(parents=True, exist_ok=True)
        @contextmanager
        def admission(*args):
            yield 0, ()
        with patch.object(dev, 'source_identity', return_value='a' * 64), \
             patch.object(dev_checks, 'runtime_root', return_value=cache), \
             patch.object(dev_checks, 'admission', admission), \
             patch.object(dev_checks, 'prune_caches'), \
             patch.object(dev_checks, 'check_storage'), \
             patch.object(dev_checks, 'run_tests', return_value=(1, 0, 0.1)):
            with self.assertRaisesRegex(RuntimeError, 'previous installed build retained'):
                dev.build(path, profile)
        self.assertEqual(profile['build'], 'old')
        self.assertEqual(json.loads((path / 'profile.json').read_text())['build'], 'old')

    def test_media_traversal_is_rejected(self):
        for value in ('../track.flac', '/track.flac', 'artist/../../track.flac', 'a\0b'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                dev_media.relative_path(value)

    def test_read_only_mount_uses_actual_macos_flags_and_checks_image_ownership(self):
        root = dev.state_root()
        volume = {'WritableVolume': False, 'WritableMedia': False}
        images = {'images': [{'image-path': str(root / 'music.sparseimage'),
                   'system-entities': [{'mount-point': str(root / 'music')}]}]}
        results = [subprocess.CompletedProcess([], 0, plistlib.dumps(value)) for value in (volume, images)]
        with patch.object(Path, 'is_mount', return_value=True), patch.object(subprocess, 'run', side_effect=results):
            self.assertTrue(dev_media.mounted(root))
        images['images'][0]['image-path'] = '/another-owner.sparseimage'
        results = [subprocess.CompletedProcess([], 0, plistlib.dumps(value)) for value in (volume, images)]
        with patch.object(Path, 'is_mount', return_value=True), patch.object(subprocess, 'run', side_effect=results):
            with self.assertRaisesRegex(RuntimeError, 'does not belong'):
                dev_media.mounted(root)
        volume['WritableVolume'] = True
        with patch.object(Path, 'is_mount', return_value=True), \
             patch.object(subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, plistlib.dumps(volume))):
            with self.assertRaisesRegex(RuntimeError, 'writable'):
                dev_media.mounted(root)

    def test_missing_volume_is_distinct_from_runtime_failure(self):
        missing = subprocess.CompletedProcess([], 1, '[]', 'Error: no such volume')
        with patch.object(subprocess, 'run', return_value=missing):
            self.assertIsNone(dev.inspect('volume', 'owned'))
        unavailable = subprocess.CompletedProcess([], 1, '', 'Cannot connect: no such file or directory')
        with patch.object(subprocess, 'run', return_value=unavailable):
            with self.assertRaisesRegex(RuntimeError, 'OrbStack'):
                dev.inspect('volume', 'owned')

    def test_installed_retention_keeps_current_backup_and_unknown_data(self):
        root = dev.state_root()
        profile = root / 'profiles/personal'
        profile.mkdir()
        current, saved, obsolete, unknown = (character * 64 for character in 'abcd')
        dev.write_json(profile / 'profile.json', {'build': current})
        backup = profile / 'backups/fixture'
        backup.mkdir(parents=True)
        dev.write_json(backup / 'backup.json', {'build': saved})
        for identity in (current, saved, obsolete, unknown):
            directory = root / 'builds' / identity
            directory.mkdir()
            (directory / 'keep.bin').write_bytes(b'important')
            if identity != unknown:
                dev.write_json(directory / 'build.json', {'source_sha256': identity})
        dev.prune_installed_builds(root)
        self.assertFalse((root / 'builds' / obsolete).exists())
        for identity in (current, saved, unknown):
            self.assertTrue((root / 'builds' / identity / 'keep.bin').is_file())

    def test_import_cannot_silently_change_existing_source(self):
        root = dev.state_root()
        dev.write_json(root / 'media-manifest.json', {'remote': 'old@host', 'source': '/music', 'albums': ['artist/album']})
        with patch.object(dev_media, 'verify') as verify:
            with self.assertRaisesRegex(RuntimeError, 'another source'):
                dev_media.import_media(root, 'new@host', '/music', root / 'unused.json')
            verify.assert_not_called()

    def test_missing_persistent_volume_never_becomes_an_empty_replacement(self):
        root = dev.state_root()
        path = root / 'profiles/personal'
        path.mkdir()
        profile = {'name': 'personal', 'database_initialized': True}
        with patch.object(dev, 'owned', return_value=None), patch.object(dev, 'docker_operation') as mutation:
            with self.assertRaisesRegex(RuntimeError, 'no empty replacement'):
                dev.start_db(path, profile)
            mutation.assert_not_called()
            profile.pop('database_initialized')
            (path / 'client-login.json').write_text('{}')
            with self.assertRaisesRegex(RuntimeError, 'no empty replacement'):
                dev.start_db(path, profile)
            mutation.assert_not_called()


if __name__ == '__main__':
    unittest.main()
