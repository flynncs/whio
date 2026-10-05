import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from contextlib import redirect_stdout
from io import StringIO
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev_checks as checks


class DevChecksTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        for name in ('targets', 'locks', 'logs'):
            (self.root / name).mkdir()

    def holder(self, key):
        script = (
            'import sys,time; from pathlib import Path; import dev_checks as d; '
            'root=Path(sys.argv[1]); key=sys.argv[2]; '
            'context=d.admission(root,key,0); context.__enter__(); '
            'print("admitted",flush=True); time.sleep(30)'
        )
        environment = dict(os.environ, PYTHONPATH=str(Path(checks.__file__).parent))
        process = subprocess.Popen(
            [sys.executable, '-c', script, str(self.root), key],
            env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.addCleanup(self.stop, process)
        self.assertEqual(process.stdout.readline().strip(), 'admitted')
        return process

    def stop(self, process):
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        for stream in (process.stdout, process.stderr):
            if stream:
                stream.close()

    def test_two_checkouts_admitted_and_third_has_bounded_queue(self):
        first = self.holder('a' * 16)
        second = self.holder('b' * 16)
        started = time.monotonic()
        with self.assertRaisesRegex(RuntimeError, 'admission deadline'):
            with checks.admission(self.root, 'c' * 16, 0.1):
                self.fail('third command entered')
        self.assertLess(time.monotonic() - started, 1)
        first.kill()
        first.wait(timeout=5)
        with checks.admission(self.root, 'c' * 16, 0):
            self.assertIsNone(second.poll())

    def test_same_checkout_excluded_even_with_a_free_slot(self):
        self.holder('a' * 16)
        with self.assertRaisesRegex(RuntimeError, 'admission deadline'):
            with checks.admission(self.root, 'a' * 16, 0):
                self.fail('same checkout entered')

    def test_child_retains_locks_after_parent_is_killed(self):
        script = (
            'import sys,time,subprocess; from pathlib import Path; import dev_checks as d; '
            'context=d.admission(Path(sys.argv[1]),"a"*16,0); slot,fds=context.__enter__(); '
            'child=subprocess.Popen([sys.executable,"-c","import time; time.sleep(30)"],pass_fds=fds); '
            'print(child.pid,flush=True); time.sleep(30)'
        )
        process = subprocess.Popen(
            [sys.executable, '-c', script, str(self.root)],
            env=dict(os.environ, PYTHONPATH=str(Path(checks.__file__).parent)),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.addCleanup(self.stop, process)
        child_pid = int(process.stdout.readline())
        self.addCleanup(self.kill_pid, child_pid)
        process.kill()
        process.wait(timeout=5)
        with self.assertRaisesRegex(RuntimeError, 'admission deadline'):
            with checks.admission(self.root, 'a' * 16, 0):
                self.fail('inherited lock was lost')
        self.kill_pid(child_pid)
        deadline = time.monotonic() + 3
        while True:
            try:
                with checks.admission(self.root, 'a' * 16, 0):
                    break
            except RuntimeError:
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.05)

    def kill_pid(self, pid):
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def test_prune_preserves_active_cache_and_removes_idle_cache(self):
        active = self.root / 'targets' / ('a' * 16)
        idle = self.root / 'targets' / ('b' * 16)
        for directory in (active, idle):
            directory.mkdir()
            (directory / 'artifact').write_bytes(b'x' * 32768)
        with checks.admission(self.root, active.name, 0):
            checks.prune_caches(self.root, remove_all=True)
            self.assertTrue((active / 'artifact').exists())
            self.assertFalse(idle.exists())
        checks.prune_caches(self.root, remove_all=True)
        self.assertFalse(active.exists())

    def test_simultaneous_maintenance_waits_for_the_short_critical_section(self):
        held = checks.try_lock(self.root / 'locks' / 'maintenance.lock')
        self.addCleanup(held.close)
        errors = []
        def prune():
            try:
                checks.prune_caches(self.root)
            except Exception as error:
                errors.append(error)
        worker = threading.Thread(target=prune)
        worker.start()
        time.sleep(0.1)
        self.assertTrue(worker.is_alive())
        held.close()
        worker.join(timeout=2)
        self.assertFalse(worker.is_alive())
        self.assertEqual(errors, [])

    def test_budget_evicts_idle_oldest_cache(self):
        old = self.root / 'targets' / ('a' * 16)
        recent = self.root / 'targets' / ('b' * 16)
        for directory in (old, recent):
            directory.mkdir()
            (directory / 'artifact').write_bytes(b'x' * 32768)
        os.utime(old, (1, 1))
        with patch.object(checks, 'CACHE_BUDGET', checks.cache_size(recent)):
            checks.prune_caches(self.root)
        self.assertFalse(old.exists())
        self.assertTrue(recent.exists())

    def test_prune_is_silent_when_no_cache_is_removed(self):
        output = StringIO()
        with redirect_stdout(output):
            checks.prune_caches(self.root)
        self.assertEqual(output.getvalue(), '')
        self.assertEqual(list((self.root / 'targets').iterdir()), [])

    def test_budget_and_disk_reserve_fail(self):
        cache = self.root / 'targets' / ('a' * 16)
        cache.mkdir()
        (cache / 'artifact').write_bytes(b'x' * 32768)
        with patch.object(checks, 'CACHE_BUDGET', 1):
            with self.assertRaisesRegex(RuntimeError, 'exceeded'):
                checks.check_storage(self.root)
        with patch.object(checks, 'FREE_RESERVE', 10 ** 30):
            with self.assertRaisesRegex(RuntimeError, 'free'):
                checks.check_storage(self.root)

    def invoke_main(self, args, executed=1, exit_code=0):
        observed = self.root / 'observed.json'
        script = (
            'import sys,json; from pathlib import Path; import dev_checks as d; '
            f'd.runtime_root=lambda:Path({str(self.root)!r}); '
            'd.docker=lambda *args: (_ for _ in ()).throw(RuntimeError("unexpected docker call")); '
            f'd.run_tests=lambda command,environment,*args,**kwargs: (Path({str(observed)!r}).write_text(json.dumps([command,environment])) and {exit_code},{executed},0); '
            'sys.exit(d.main())'
        )
        environment = dict(os.environ, PYTHONPATH=str(Path(checks.__file__).parent), DATABASE_URL='must-not-be-inherited', WHIO_DATABASE_URL='must-not-be-inherited')
        result = subprocess.run([sys.executable, '-c', script, *args], env=environment, capture_output=True, text=True, timeout=5)
        return result, json.loads(observed.read_text()) if observed.exists() else None

    def test_unit_mode_has_no_database_and_uses_compact_owned_cache(self):
        result, observed = self.invoke_main(['--', '--lib', 'features::music_library::domain'])
        self.assertEqual(result.returncode, 0, result.stderr)
        command, environment = observed
        self.assertNotIn('--features', command)
        self.assertNotIn('DATABASE_URL', environment)
        self.assertNotIn('WHIO_DATABASE_URL', environment)
        self.assertTrue(environment['CARGO_TARGET_DIR'].startswith(str(self.root / 'targets')))
        self.assertEqual(environment['CARGO_BUILD_JOBS'], '2')
        self.assertEqual(environment['CARGO_INCREMENTAL'], '0')
        self.assertEqual(environment['CARGO_PROFILE_TEST_DEBUG'], '0')
        self.assertEqual(environment['CARGO_PROFILE_TEST_STRIP'], 'none')

    def test_unscoped_and_no_execution_selectors_fail_before_resources(self):
        for args in ([], ['--', '--lib'], ['--', '--lib', ''], ['--', '--list'], ['--', '--test', 'x'], ['--', '--test', 'x', '--', '--test-threads=99']):
            with self.subTest(args=args):
                result, observed = self.invoke_main(args)
                self.assertEqual(result.returncode, 2)
                self.assertIsNone(observed)
        self.assertEqual(list((self.root / 'targets').iterdir()), [])

    def test_several_filters_and_targets_share_one_run(self):
        result, observed = self.invoke_main(['--', '--lib', '--test', 'integration', 'domain::text', 'compat_http', '--', '--exact'])
        self.assertEqual(result.returncode, 0, result.stderr)
        command = observed[0]
        separator = command.index('--')
        self.assertEqual(command[separator - 3:separator], ['--lib', '--test', 'integration'])
        self.assertEqual(command[separator + 2:], ['domain::text', 'compat_http', '--exact'])

    def test_full_is_explicit_and_zero_tests_fail(self):
        result, _ = self.invoke_main(['--full'])
        self.assertEqual(result.returncode, 0, result.stderr)
        result, _ = self.invoke_main(['--', '--test', 'named_target', 'named_filter'], executed=0)
        self.assertEqual(result.returncode, 2)
        result, _ = self.invoke_main(['--', '--test', 'named_target', 'named_filter'], exit_code=7)
        self.assertEqual(result.returncode, 7)

    def test_streaming_is_visible_before_child_exit_and_failure_propagates(self):
        log = self.root / 'live.log'
        result = []
        script = 'import time,sys; print("live",flush=True); time.sleep(0.5); print("test result: FAILED. 1 passed; 1 failed;"); sys.exit(7)'
        worker = threading.Thread(target=lambda: result.append(checks.run_tests([sys.executable, '-c', script], os.environ.copy(), log, 3)))
        worker.start()
        deadline = time.monotonic() + 1
        while not log.exists() or b'live' not in log.read_bytes():
            if time.monotonic() >= deadline:
                self.fail('output was not streamed')
            time.sleep(0.01)
        self.assertTrue(worker.is_alive())
        worker.join(timeout=3)
        self.assertFalse(worker.is_alive())
        self.assertEqual(result[0][:2], (7, 2))

    def test_log_cap_preserves_live_output_and_test_count(self):
        log = self.root / 'bounded.log'
        script = 'print("x"*256); print("test result: ok. 1 passed; 0 failed;")'
        with patch.object(checks, 'MAX_LOG_BYTES', 128):
            result = checks.run_tests([sys.executable, '-c', script], os.environ.copy(), log, 3)
        self.assertEqual(result[:2], (0, 1))
        self.assertLess(log.stat().st_size, 256)
        self.assertIn(b'truncated', log.read_bytes())

    def test_storage_guard_failure_stops_the_running_child(self):
        def guard():
            raise RuntimeError('injected storage budget failure')
        started = time.monotonic()
        with patch.object(checks, 'STORAGE_CHECK_INTERVAL', 0.05):
            with self.assertRaisesRegex(RuntimeError, 'injected storage'):
                checks.run_tests([sys.executable, '-c', 'import time; time.sleep(30)'], os.environ.copy(), self.root / 'guard.log', 3, guard=guard)
        self.assertLess(time.monotonic() - started, 2)

    def test_deadline_kills_owned_process_group(self):
        script = 'import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print("started",flush=True); time.sleep(30)'
        started = time.monotonic()
        result = checks.run_tests([sys.executable, '-c', script], os.environ.copy(), self.root / 'timeout.log', 0.1)
        self.assertEqual(result[:2], (124, 0))
        self.assertLess(time.monotonic() - started, 7)


if __name__ == '__main__':
    unittest.main()
