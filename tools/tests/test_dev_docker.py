import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev
import dev_checks as checks
import dev_docker as docker


class DockerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name).resolve()
        self.overrides = [patch.object(dev, 'STATE', self.root / 'state')]
        for override in self.overrides:
            override.start()
        self.path, self.profile = dev.load_profile('preview-fixture', create=True)
        self.profile.update(architecture='arm64', docker_context='fixture', media_root=str(self.root / 'music'),
                            images={'compiler': 'compiler', 'core': 'core', 'resolver': 'resolver'})
        (self.root / 'music').mkdir()

    def tearDown(self):
        for override in reversed(self.overrides):
            override.stop()
        self.temporary.cleanup()

    def test_runner_alive_requires_complete_matching_identity(self):
        profile = {'runner': {'pid': 123, 'start_time': 'start'}}
        with patch.object(docker, 'runner_identity', return_value='start'):
            self.assertTrue(docker.runner_alive(profile))
        with patch.object(docker, 'runner_identity', return_value='different'):
            self.assertFalse(docker.runner_alive(profile))
        with patch.object(docker, 'runner_identity', return_value=None):
            self.assertFalse(docker.runner_alive(profile))
        for runner in ({'pid': 123}, {'pid': 123, 'start_time': ''}, {'pid': 123, 'start_time': None}):
            with self.subTest(runner=runner):
                self.assertFalse(docker.runner_alive({'runner': runner}))

    def test_runner_record_requires_process_start_time(self):
        with patch.object(docker.os, 'getpid', return_value=123), \
             patch.object(docker, 'runner_identity', return_value='start'):
            self.assertEqual(docker.runner_record(), {'pid': 123, 'start_time': 'start'})
        with patch.object(docker.os, 'getpid', return_value=123), \
             patch.object(docker, 'runner_identity', return_value=None):
            with self.assertRaisesRegex(RuntimeError, 'cannot identify preview runner'):
                docker.runner_record()

    def test_cleanup_reaps_only_dead_runner_previews(self):
        dead_path, dead = dev.load_profile('preview-check-dead', create=True)
        live_path, live = dev.load_profile('preview-restore-live', create=True)
        for profile in (dead, live):
            profile.update(runtime='docker', docker_context='fixture', expires=9999999999,
                           source_checkout=str(dev.REPO), runner={'pid': 123, 'start_time': 'start'})
            dev.write_json((dev.STATE / 'profiles' / profile['name'] / 'profile.json'), profile)
        removed = []
        with patch.object(docker, 'runner_alive', side_effect=lambda profile: profile['name'] == live['name']), \
             patch.object(docker, 'destroy', side_effect=lambda path, profile: removed.append(profile['name'])), \
             patch.object(docker, 'cleanup_caches'), patch.object(dev, 'prune_installed_builds'), \
             patch.object(checks, 'prune_caches'), patch.object(checks, 'check_storage'):
            docker.cleanup()
        self.assertEqual(removed, [dead['name']])
        self.assertTrue(dead_path.exists())
        self.assertTrue(live_path.exists())

    def test_profile_artifacts_live_in_the_container_build_root(self):
        self.assertEqual(dev.builds_root(self.profile), dev.state_root() / 'container-builds' / 'arm64')
        self.assertEqual(dev.docker_args(self.profile), ['docker', '--context', 'fixture'])

    def test_unconfigured_profile_does_not_break_build_retention(self):
        dev.load_profile('preview-unconfigured', create=True)
        dev.prune_installed_builds(dev.state_root())

    def test_cached_binaries_install_missing_images_in_another_engine(self):
        from contextlib import contextmanager
        import dev_watch
        identity = 'b' * 64
        destination = dev.builds_root(self.profile) / identity
        destination.mkdir()
        dev.write_json(destination / 'build.json', {'images': self.profile['images']})
        @contextmanager
        def admission(*arguments):
            yield 0, ()
        with patch.object(docker, 'configure_profile'), patch.object(docker, 'identity', return_value=identity), \
             patch.object(dev_watch, 'components', return_value={}), patch.object(dev, 'inspect', return_value=None), \
             patch.object(checks, 'admission', admission), patch.object(docker, 'copy_source'), \
             patch.object(docker, 'ensure_images') as install:
            docker.build(self.path, self.profile, persist=False)
        install.assert_called_once()
        self.assertEqual(self.profile['build'], identity)

    def test_new_target_volume_clones_the_main_checkout_cache(self):
        main_key = 'a' * 16
        source = f'lumen-build-{os.getuid()}-{docker.context_key(self.profile)}-arm64-{main_key}'
        def inspect(kind, name, profile=None):
            return {'Labels': {}} if kind == 'volume' and name == source else None
        completed = subprocess.CompletedProcess([], 0)
        with patch.object(dev, 'inspect', side_effect=inspect), \
             patch.object(docker, 'main_checkout_key', return_value=main_key), \
             patch.object(docker, 'command', return_value='created') as mutation, \
             patch.object(subprocess, 'run', return_value=completed) as run:
            name = docker.cache_volume(self.profile, 'target', 'b' * 16)
        mutation.assert_called_once()
        self.assertEqual(mutation.call_args.args[1:3], ('volume', 'create'))
        self.assertIn('dev.lumen.checkout=' + 'b' * 16, mutation.call_args_list[0].args)
        arguments = run.call_args.args[0]
        self.assertIn(f'type=volume,source={source},target=/from', arguments)
        self.assertIn(f'type=volume,source={name},target=/to', arguments)
        self.assertEqual(arguments[-4:], ['-a', '--reflink=always', '/from/.', '/to/'])

    def test_main_checkout_target_volume_is_not_cloned(self):
        key = 'a' * 16
        with patch.object(dev, 'inspect', return_value=None), \
             patch.object(docker, 'main_checkout_key', return_value=key), \
             patch.object(docker, 'command') as mutation, \
             patch.object(subprocess, 'run') as run:
            docker.cache_volume(self.profile, 'target', key)
        mutation.assert_called_once()
        run.assert_not_called()

    def test_existing_target_volume_is_not_cloned(self):
        labels = {docker.OWNER: str(os.getuid()), docker.KIND: 'target', 'dev.lumen.checkout': 'b' * 16}
        with patch.object(dev, 'inspect', return_value={'Labels': labels}), \
             patch.object(docker, 'main_checkout_key', return_value='a' * 16), \
             patch.object(docker, 'command') as mutation, \
             patch.object(subprocess, 'run') as run:
            docker.cache_volume(self.profile, 'target', 'b' * 16)
        mutation.assert_not_called()
        run.assert_not_called()

    def test_failed_clone_keeps_the_volume_and_reports(self):
        main_key = 'a' * 16
        source = f'lumen-build-{os.getuid()}-{docker.context_key(self.profile)}-arm64-{main_key}'
        def inspect(kind, name, profile=None):
            return {'Labels': {}} if kind == 'volume' and name == source else None
        failures = (
            [subprocess.CompletedProcess([], 1), subprocess.CompletedProcess([], 0)],
            [subprocess.TimeoutExpired('docker', 300), subprocess.CompletedProcess([], 0)],
        )
        for index, results in enumerate(failures):
            with self.subTest(failure=index):
                output = io.StringIO()
                with patch.object(dev, 'inspect', side_effect=inspect), \
                     patch.object(docker, 'main_checkout_key', return_value=main_key), \
                     patch.object(docker, 'command', return_value='created') as mutation, \
                     patch.object(subprocess, 'run', side_effect=results) as run, \
                     patch.object(docker, 'remove_container') as remove, \
                     contextlib.redirect_stdout(output):
                    docker.cache_volume(self.profile, 'target', 'b' * 16)
                self.assertEqual(output.getvalue(), 'target cache was not cloned; build starts from an empty target\n')
                mutation.assert_called_once()
                self.assertEqual(mutation.call_args.args[1:3], ('volume', 'create'))
                self.assertEqual(run.call_count, 2)
                self.assertIn('find /to -mindepth 1 -delete', run.call_args.args[0])
                self.assertEqual(remove.call_count, 2)

    def test_unowned_cache_cannot_be_adopted(self):
        with patch.object(dev, 'inspect', return_value={'Labels': {docker.OWNER: '-1'}}), patch.object(docker, 'command') as mutation:
            with self.assertRaisesRegex(RuntimeError, 'another owner'):
                docker.cache_volume(self.profile, 'registry')
            mutation.assert_not_called()

    def test_initialized_missing_database_never_creates_empty_volume(self):
        self.profile['database_initialized'] = True
        with patch.object(dev, 'owned', return_value=None), patch.object(docker, 'command') as mutation:
            with self.assertRaisesRegex(RuntimeError, 'no empty replacement'):
                docker.start_db(self.path, self.profile)
            mutation.assert_not_called()

    def test_runtime_media_is_readonly_and_preserves_database_paths(self):
        document = json.loads(docker.config(self.path, self.profile, True).read_text())
        self.assertNotIn('builder', document['services'])
        for name in ('api', 'worker'):
            media = [value for value in document['services'][name]['volumes'] if value['source'] == self.profile['media_root']]
            self.assertEqual(media[0]['target'], self.profile['media_root'])
            self.assertTrue(media[0]['read_only'])
            self.assertEqual(document['services'][name]['environment']['WHIO_MEDIA_ROOTS'], self.profile['media_root'])

    def test_source_mirror_updates_changed_files_and_deletes_removed_files(self):
        repository = self.root / 'repo/apps/core/tests/fixtures'
        repository.mkdir(parents=True)
        changed = repository / 'changed.json'
        removed = repository / 'removed.json'
        changed.write_text('{"value": 1}')
        removed.write_text('{"value": 2}')
        dockerfiles = self.root / 'repo/docker'
        dockerfiles.mkdir()
        for name in ('local.Dockerfile', 'ytm-resolver.Dockerfile'):
            (dockerfiles / name).write_text('FROM scratch\\n')
        destination = self.root / 'mirror'
        with patch.object(dev, 'REPO', self.root / 'repo'):
            docker.copy_source(destination)
            output = destination / changed.relative_to(dev.REPO)
            os.utime(output, ns=(1_000_000_000, 1_000_000_000))
            before = output.stat().st_mtime_ns
            docker.copy_source(destination)
            self.assertEqual(output.stat().st_mtime_ns, before)
            changed.write_text('{"value": 3}')
            removed.unlink()
            docker.copy_source(destination)
            self.assertNotEqual(output.stat().st_mtime_ns, before)
            self.assertEqual(output.read_text(), '{"value": 3}')
            self.assertFalse((destination / removed.relative_to(dev.REPO)).exists())

    def test_source_snapshot_copies_binary_and_json_fixtures_and_hashes_changes(self):
        fixture_root = self.root / 'repo/apps/core/tests/fixtures'
        fixture_root.mkdir(parents=True)
        audio = fixture_root / 'tagged.flac'
        audio.write_bytes(b'fixture bytes')
        response = self.root / 'repo/apps/core/src/integrations/musicbrainz/fixtures/artist.json'
        response.parent.mkdir(parents=True)
        response.write_text('{"artist": "controlled"}')
        dockerfiles = self.root / 'repo/docker'
        dockerfiles.mkdir()
        for name in ('local.Dockerfile', 'ytm-resolver.Dockerfile'):
            (dockerfiles / name).write_text('FROM scratch\n')
        destination = self.root / 'snapshot'
        destination.mkdir()
        with patch.object(dev, 'REPO', self.root / 'repo'):
            identity = docker.copy_source(destination)
            self.assertEqual((destination / audio.relative_to(dev.REPO)).read_bytes(), audio.read_bytes())
            self.assertEqual((destination / response.relative_to(dev.REPO)).read_bytes(), response.read_bytes())
            audio.write_bytes(b'changed fixture')
            self.assertNotEqual(dev.source_identity(), identity)

    def test_builder_does_not_require_a_persistent_database(self):
        with patch.object(docker, 'cache_volume', side_effect=lambda profile, kind, key=None: kind):
            document = json.loads(docker.config(self.path, self.profile, False, self.root).read_text())
        self.assertEqual(set(document['services']), {'builder'})
        self.assertEqual(set(document['volumes']), {'target', 'registry'})
        self.assertEqual(document['services']['builder']['environment']['CARGO_BUILD_JOBS'], '2')

    def test_disposable_memory_and_swap_limits_match(self):
        self.profile['disposable'] = True
        document = json.loads(docker.config(self.path, self.profile, False).read_text())
        postgres = document['services']['postgres']
        self.assertEqual(postgres['mem_limit'], postgres['memswap_limit'])
        self.assertEqual(postgres['mem_limit'], '4g')
        self.assertEqual(postgres['shm_size'], '1gb')
        self.assertEqual(postgres['tmpfs'], ['/var/lib/postgresql:rw,size=3g'])
        self.assertIn('max_connections=200', postgres['command'])
        self.assertEqual(postgres['command'][postgres['command'].index('max_connections=200') - 1], '-c')
        self.assertIn('127.0.0.1', postgres['healthcheck']['test'])
        self.assertNotIn('volumes', postgres)
        self.assertNotIn('postgres', document['volumes'])

    def test_cli_passes_the_spaced_binary_path_as_one_argument(self):
        import subprocess
        result = subprocess.CompletedProcess([], 0, 'done')
        spaced = self.root / 'Application Support' / 'profile'
        with patch.object(subprocess, 'run', return_value=result) as run:
            docker.cli(spaced, self.profile, 'assign-admin', 'local')
        arguments = run.call_args.args[0]
        self.assertIn(str(spaced / 'current-rust/whio-cli'), arguments)
        self.assertIn('exec', arguments)

    def test_existing_music_directory_has_no_import_size_or_symlink_restrictions(self):
        music = self.root / 'music'
        (music / 'track.flac').write_bytes(b'larger than the import limit in this test')
        (music / 'linked-track.flac').symlink_to(music / 'track.flac')
        with patch.object(dev.dev_media, 'LIMIT', 1):
            self.assertEqual(docker.media(self.profile), music)

    def test_missing_music_directory_is_reported(self):
        missing = self.root / 'missing'
        self.profile['media_root'] = str(missing)
        with self.assertRaisesRegex(RuntimeError, 'music directory is missing'):
            docker.media(self.profile)
        self.assertFalse(missing.exists())

    def test_provider_configuration_cannot_import_unrelated_environment(self):
        (self.path / 'providers.env').write_text('WHIO_DATABASE_URL=unrelated\n')
        (self.path / 'providers.env').chmod(0o600)
        with self.assertRaisesRegex(RuntimeError, 'native metadata provider'):
            docker.providers(self.path)

    def test_provider_settings_reach_api_and_worker_only(self):
        (self.path / 'providers.env').write_text('WHIO_MUSICBRAINZ_ENABLED=true\nWHIO_AUDIODB_KEY=secret\n')
        (self.path / 'providers.env').chmod(0o600)
        document = json.loads(docker.config(self.path, self.profile, True).read_text())
        for name in ('api', 'worker'):
            self.assertEqual(document['services'][name]['environment']['WHIO_MUSICBRAINZ_ENABLED'], 'true')
            self.assertEqual(document['services'][name]['environment']['WHIO_AUDIODB_KEY'], 'secret')
        self.assertNotIn('WHIO_AUDIODB_KEY', document['services']['youtube']['environment'])

    def test_failed_import_does_not_publish_or_stop_live_stack(self):
        with patch.object(docker, 'running', return_value=True), patch.object(dev, 'ready', return_value=True), \
             patch.object(docker, 'validate_resolvers', side_effect=RuntimeError('import failed')), \
             patch.object(docker, 'config') as publish, patch.object(docker, 'compose') as mutation, \
             patch.object(dev, 'runtime_links'), patch.object(dev, 'write_json'):
            with self.assertRaisesRegex(RuntimeError, 'import failed'):
                docker.replace(self.path, self.profile, dict(self.profile))
            publish.assert_not_called()
            mutation.assert_not_called()

    def test_database_password_dollar_is_literal_in_compose(self):
        self.profile['postgres_password'] = 'literal$VALUE'
        document = json.loads(docker.config(self.path, self.profile, False).read_text())
        self.assertEqual(document['services']['postgres']['environment']['POSTGRES_PASSWORD'], 'literal$$VALUE')

    def test_btrfs_volume_measurement_uses_exclusive_bytes(self):
        result = subprocess.CompletedProcess(
            [], 0, stdout='btrfs\nTotal Exclusive Set shared Filename\n1000 400 600 /v\n'
        )
        with patch.object(subprocess, 'run', return_value=result), \
             patch.object(docker, 'measurement_image', return_value='compiler-image'), \
             patch.object(docker, 'remove_container') as remove:
            measured = docker.volume_filesystem_usage(self.profile, 'target-volume')
        self.assertEqual(measured, ('btrfs', 400, 600))
        remove.assert_called_once()

    def test_storage_uses_exclusive_volume_measurement(self):
        rows = {
            ('system', 'df'): {
                'Images': [{'ID': 'image-id', 'Size': '10GiB', 'UniqueSize': '3GiB'}],
                'Volumes': [{'Name': 'target', 'Labels': f'{docker.OWNER}={os.getuid()},{docker.KIND}=target', 'Size': '8GiB'}],
            },
            ('image', 'ls'): 'image-id',
            ('filesystem', 'target'): ('btrfs', 4 * 1024**3, 2 * 1024**3),
        }

        def command(_profile, *arguments):
            if arguments[:2] == ('system', 'df'):
                return json.dumps(rows[('system', 'df')])
            if arguments[:2] == ('image', 'ls'):
                return rows[('image', 'ls')]
            raise AssertionError(arguments)

        with patch.object(docker, 'command', side_effect=command), \
             patch.object(docker, 'volume_filesystem_usage', return_value=rows[('filesystem', 'target')]):
            result = docker.storage(self.profile)
        self.assertEqual(result['bytes'], 9 * 1024**3)
        self.assertEqual(result['volumes'][0]['bytes'], 6 * 1024**3)

    def test_host_guard_includes_registered_docker_cache(self):
        root = self.root / 'checks'
        (root / 'targets').mkdir(parents=True)
        dev.write_json(root / 'docker-cache-fixture.json', {'bytes': checks.CACHE_BUDGET + 1})
        with self.assertRaisesRegex(RuntimeError, '16 GiB'):
            checks.check_storage(root)

    def test_expired_preview_with_active_watcher_is_retained(self):
        self.profile['expires'] = 1
        dev.write_json(self.path / 'profile.json', self.profile)
        with patch.object(checks, 'try_lock', return_value=None), patch.object(docker, 'destroy') as destroy, \
             patch.object(docker, 'cleanup_caches'), patch.object(dev, 'prune_installed_builds'):
            docker.cleanup()
            destroy.assert_not_called()
        self.assertTrue(self.path.exists())


if __name__ == '__main__':
    unittest.main()
