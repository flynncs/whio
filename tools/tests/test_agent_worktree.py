from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import agent_worktree
import dev


class WorktreeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name).resolve()
        self.repo = root / 'repo'
        self.repo.mkdir()
        self.patches = [patch.object(dev, 'REPO', self.repo), patch.object(dev, 'STATE', root / 'state')]
        for override in self.patches:
            override.start()
        subprocess.run(['git', 'init', '-q', self.repo], check=True)
        for name, value in (('user.name', 'dev setup tests'), ('user.email', 'dev-tests@example.invalid')):
            self.git('config', name, value)
        (self.repo / '.git/info/exclude').write_text('AGENTS.md\ndocs/\n.env\n')
        (self.repo / 'source.rs').write_text('baseline')
        (self.repo / 'tools').mkdir()
        for name in ('test-core.py', 'test-postgres.py', 'dev_checks.py'):
            (self.repo / 'tools' / name).write_text('pass\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'fixture')
        (self.repo / 'AGENTS.md').write_text('read task packet')
        (self.repo / 'docs').mkdir()
        self.packet = self.repo / 'docs/task.md'
        self.packet.write_text('allowed writes: source.rs; checks: named target')

    def tearDown(self):
        for override in reversed(self.patches):
            override.stop()
        self.temporary.cleanup()

    def git(self, *args, cwd=None):
        return subprocess.check_output(['git', *args], cwd=cwd or self.repo)

    def test_context_files_include_only_instructions_packet_agents_and_areas(self):
        (self.repo / 'docs/agents').mkdir()
        (self.repo / 'docs/agents/review-brief.md').write_text('review')
        (self.repo / 'docs/agents/dispatch.sh').write_text('#!/bin/sh\n')
        (self.repo / 'docs/areas').mkdir()
        (self.repo / 'docs/areas/dev-tooling.md').write_text('area')
        (self.repo / 'docs/implementation').mkdir()
        (self.repo / 'docs/implementation/other.md').write_text('other')
        (self.repo / 'docs/ROADMAP.md').write_text('roadmap')
        self.assertEqual(agent_worktree.context_files(self.packet), sorted([
            self.repo / 'AGENTS.md', self.packet,
            self.repo / 'docs/agents/review-brief.md', self.repo / 'docs/agents/dispatch.sh',
            self.repo / 'docs/areas/dev-tooling.md']))

    def test_current_snapshot_preserves_index_and_hands_off_untracked_source(self):
        (self.repo / 'source.rs').write_text('staged')
        self.git('add', 'source.rs')
        (self.repo / 'source.rs').write_text('working')
        (self.repo / 'new.rs').write_text('new')
        (self.repo / '.env').write_text('secret')
        before = (self.repo / '.git/index').read_bytes()
        status = self.git('status', '--porcelain')
        agent_worktree.create('fixture', self.packet, 'HEAD', 'current')
        _, path, record = agent_worktree.load('fixture')
        self.assertEqual((self.repo / '.git/index').read_bytes(), before)
        self.assertEqual(self.git('status', '--porcelain'), status)
        self.assertEqual((path / 'source.rs').read_text(), 'working')
        self.assertEqual((path / 'new.rs').read_text(), 'new')
        self.assertFalse((path / '.env').exists())
        self.assertEqual(self.git('status', '--porcelain', cwd=path), b'')
        self.assertEqual((path / 'docs/task.md').read_text(), self.packet.read_text())
        self.assertEqual(record['source'], 'current')
        agent_worktree.retire('fixture')
        self.assertFalse(path.exists())
        self.assertTrue(self.git('rev-parse', 'codex/fixture').strip())

    def test_dirty_and_extra_ignored_files_block_retirement(self):
        agent_worktree.create('fixture', self.packet, 'HEAD', 'head')
        _, path, _ = agent_worktree.load('fixture')
        source = path / 'source.rs'
        source.write_text('important changes')
        with self.assertRaisesRegex(RuntimeError, 'changes'):
            agent_worktree.retire('fixture')
        source.write_text('baseline')
        (path / '.env').write_text('important local credentials')
        with self.assertRaisesRegex(RuntimeError, 'ignored files'):
            agent_worktree.retire('fixture')
        self.assertTrue(path.exists())
        self.assertEqual((path / '.env').read_text(), 'important local credentials')

    def test_current_snapshot_rejects_an_older_base(self):
        with self.assertRaisesRegex(ValueError, 'current HEAD'):
            agent_worktree.snapshot('0' * 40)


if __name__ == '__main__':
    unittest.main()
