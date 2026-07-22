#!/usr/bin/env python3
"""Hermetic regression tests for sync_upstream_to_fork.sh."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("sync_upstream_to_fork.sh").resolve()
WORKFLOW = SCRIPT.parents[1] / ".github" / "workflows" / "sync-upstream.yml"


class SyncFixture:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.origin = root / "tickernelz" / "jcode.git"
        self.upstream = root / "1jehuang" / "jcode.git"
        self.origin.parent.mkdir(parents=True)
        self.upstream.parent.mkdir(parents=True)
        self.git(root, "init", "--bare", str(self.origin))
        self.git(root, "init", "--bare", str(self.upstream))

        seed = root / "seed"
        self.git(root, "init", "-b", "master", str(seed))
        self.configure_identity(seed)
        scripts = seed / "scripts"
        scripts.mkdir()
        shutil.copy2(SCRIPT, scripts / SCRIPT.name)
        for name in (
            "test_user_owned_update_sources.sh",
            "test_install_conversion.sh",
            "setup_friction_eval.sh",
        ):
            stub = scripts / name
            stub.write_text("#!/usr/bin/env bash\nexit 0\n")
            stub.chmod(0o755)
        (seed / "shared.txt").write_text("base\n")
        self.git(seed, "add", "shared.txt", "scripts")
        self.git(seed, "commit", "-m", "base")
        self.git(seed, "remote", "add", "origin", str(self.origin))
        self.git(seed, "remote", "add", "upstream", str(self.upstream))
        self.git(seed, "push", "origin", "master")
        self.git(seed, "push", "upstream", "master")

        self.fork_work = root / "fork-work"
        self.upstream_work = root / "upstream-work"
        self.git(root, "clone", "--branch", "master", str(self.origin), str(self.fork_work))
        self.git(root, "clone", "--branch", "master", str(self.upstream), str(self.upstream_work))
        self.configure_identity(self.fork_work)
        self.configure_identity(self.upstream_work)

        self.origin_url = "https://github.com/tickernelz/jcode.git"
        self.upstream_url = "https://github.com/1jehuang/jcode.git"
        self.git(self.fork_work, "remote", "set-url", "origin", self.origin_url)
        self.git(self.fork_work, "remote", "add", "upstream", self.upstream_url)
        self.git(self.fork_work, "remote", "set-url", "--push", "upstream", "DISABLED")
        self.git(
            self.fork_work,
            "config",
            f"url.{self.origin}.insteadOf",
            self.origin_url,
        )
        self.git(
            self.fork_work,
            "config",
            f"url.{self.upstream}.insteadOf",
            self.upstream_url,
        )

        self.fake_bin = root / "fake-bin"
        self.fake_bin.mkdir()
        fake_cargo = self.fake_bin / "cargo"
        fake_cargo.write_text('#!/usr/bin/env bash\nexit "${FAKE_CARGO_STATUS:-0}"\n')
        fake_cargo.chmod(0o755)

        self.cargo_only_bin = root / "cargo-only-bin"
        self.cargo_only_bin.mkdir()
        shutil.copy2(fake_cargo, self.cargo_only_bin / "cargo")

        real_git = shutil.which("git")
        assert real_git is not None
        fake_git = self.fake_bin / "git"
        fake_git.write_text(
            f"""#!/usr/bin/env python3
import os
import subprocess
import sys

real_git = {real_git!r}
args = sys.argv[1:]
if len(args) >= 4 and args[:2] == ["remote", "get-url"]:
    remote = args[-1]
    push = "--push" in args
    key = f"remote.{{remote}}.{{'pushurl' if push else 'url'}}"
    result = subprocess.run(
        [real_git, "config", "--get-all", key], text=True, stdout=subprocess.PIPE
    )
    values = result.stdout.splitlines()
    if push and not values:
        values = subprocess.run(
            [real_git, "config", "--get-all", f"remote.{{remote}}.url"],
            text=True,
            stdout=subprocess.PIPE,
        ).stdout.splitlines()
    if values:
        print("\\n".join(values))
        raise SystemExit(0)
os.execv(real_git, [real_git, *args])
"""
        )
        fake_git.chmod(0o755)

    @staticmethod
    def git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["git", *args],
            cwd=cwd,
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def configure_identity(self, repo: Path) -> None:
        self.git(repo, "config", "user.name", "Sync Test")
        self.git(repo, "config", "user.email", "sync-test@example.invalid")

    def commit_and_push(self, repo: Path, remote: str, path: str, content: str, message: str) -> str:
        (repo / path).write_text(content)
        self.git(repo, "add", path)
        self.git(repo, "commit", "-m", message)
        self.git(repo, "push", remote, "master")
        return self.rev(repo)

    def rev(self, repo: Path, ref: str = "HEAD") -> str:
        return self.git(repo, "rev-parse", ref).stdout.strip()

    def run_sync(
        self, *args: str, cargo_status: int = 0, expose_real_rewrites: bool = False
    ) -> subprocess.CompletedProcess[str]:
        env = os.environ.copy()
        tool_bin = self.cargo_only_bin if expose_real_rewrites else self.fake_bin
        self.result_file = self.root / "sync-result"
        self.result_file.unlink(missing_ok=True)
        env.update(
            {
                "PATH": f"{tool_bin}{os.pathsep}{env['PATH']}",
                "FAKE_CARGO_STATUS": str(cargo_status),
                "JCODE_SCRATCH_DIR": str(self.root / "scratch"),
                "JCODE_SYNC_RESULT_FILE": str(self.result_file),
            }
        )
        return subprocess.run(
            [str(self.fork_work / "scripts" / SCRIPT.name), *args],
            cwd=self.fork_work,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )


class SyncUpstreamToForkTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory(prefix="jcode-sync-test-")
        self.fixture = SyncFixture(Path(self.tempdir.name))

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def test_noop_when_fork_already_contains_upstream(self) -> None:
        before = self.fixture.rev(self.fixture.fork_work)
        result = self.fixture.run_sync()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("already up to date", result.stdout)
        self.assertEqual(self.fixture.rev(self.fixture.fork_work), before)
        self.assertEqual(self.fixture.rev(self.fixture.origin, "refs/heads/master"), before)
        self.assertIn("changed=0", self.fixture.result_file.read_text())

    def test_merges_diverged_upstream_and_pushes_only_fork(self) -> None:
        self.fixture.commit_and_push(
            self.fixture.fork_work, "origin", "fork.txt", "custom\n", "fork customization"
        )
        upstream_head = self.fixture.commit_and_push(
            self.fixture.upstream_work,
            "origin",
            "upstream.txt",
            "new upstream\n",
            "upstream change",
        )

        result = self.fixture.run_sync("--push")
        self.assertEqual(result.returncode, 0, result.stderr)
        fork_head = self.fixture.rev(self.fixture.origin, "refs/heads/master")
        self.assertEqual(self.fixture.rev(self.fixture.upstream, "refs/heads/master"), upstream_head)
        self.assertIn("changed=1", self.fixture.result_file.read_text())
        self.assertEqual(
            len(self.fixture.git(self.fixture.origin, "rev-list", "--parents", "-n", "1", fork_head).stdout.split()),
            3,
        )
        self.fixture.git(self.fixture.origin, "cat-file", "-e", f"{fork_head}:fork.txt")
        self.fixture.git(self.fixture.origin, "cat-file", "-e", f"{fork_head}:upstream.txt")

    def test_conflict_restores_local_branch_and_does_not_push(self) -> None:
        fork_head = self.fixture.commit_and_push(
            self.fixture.fork_work, "origin", "shared.txt", "fork\n", "fork conflict"
        )
        self.fixture.commit_and_push(
            self.fixture.upstream_work, "origin", "shared.txt", "upstream\n", "upstream conflict"
        )

        result = self.fixture.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.fixture.rev(self.fixture.fork_work), fork_head)
        self.assertEqual(self.fixture.rev(self.fixture.origin, "refs/heads/master"), fork_head)
        self.assertEqual(self.fixture.git(self.fixture.fork_work, "status", "--porcelain").stdout, "")

    def test_validation_failure_restores_local_branch_and_does_not_push(self) -> None:
        before = self.fixture.rev(self.fixture.fork_work)
        self.fixture.commit_and_push(
            self.fixture.upstream_work, "origin", "new.txt", "new\n", "upstream change"
        )

        result = self.fixture.run_sync(cargo_status=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.fixture.rev(self.fixture.fork_work), before)
        self.assertEqual(self.fixture.rev(self.fixture.origin, "refs/heads/master"), before)
        self.assertEqual(self.fixture.git(self.fixture.fork_work, "status", "--porcelain").stdout, "")
        self.assertFalse(self.fixture.result_file.exists())

    def test_dry_run_validates_but_does_not_publish_or_leave_a_merge(self) -> None:
        before = self.fixture.rev(self.fixture.fork_work)
        self.fixture.commit_and_push(
            self.fixture.upstream_work, "origin", "new.txt", "new\n", "upstream change"
        )

        result = self.fixture.run_sync("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fork was not pushed", result.stdout)
        self.assertEqual(self.fixture.rev(self.fixture.fork_work), before)
        self.assertEqual(self.fixture.rev(self.fixture.origin, "refs/heads/master"), before)

    def test_dirty_primary_tree_is_never_modified(self) -> None:
        (self.fixture.fork_work / "untracked.txt").write_text("dirty\n")
        before = self.fixture.rev(self.fixture.fork_work)
        self.fixture.commit_and_push(
            self.fixture.upstream_work, "origin", "new.txt", "new\n", "upstream change"
        )
        result = self.fixture.run_sync("--push")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.fixture.rev(self.fixture.fork_work), before)
        self.assertEqual((self.fixture.fork_work / "untracked.txt").read_text(), "dirty\n")

    def test_refuses_wrong_upstream_or_write_enabled_upstream(self) -> None:
        self.fixture.git(
            self.fixture.fork_work, "remote", "set-url", "upstream", self.fixture.origin_url
        )
        result = self.fixture.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("upstream fetch targets an unexpected repository", result.stderr)

        self.fixture.git(
            self.fixture.fork_work, "remote", "set-url", "upstream", self.fixture.upstream_url
        )
        self.fixture.git(
            self.fixture.fork_work,
            "remote",
            "set-url",
            "--push",
            "upstream",
            self.fixture.upstream_url,
        )
        result = self.fixture.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("push URL set to DISABLED", result.stderr)

    def test_refuses_multiple_push_destinations_and_hidden_url_rewrites(self) -> None:
        self.fixture.git(
            self.fixture.fork_work,
            "config",
            "--add",
            "remote.origin.pushurl",
            self.fixture.origin_url,
        )
        self.fixture.git(
            self.fixture.fork_work,
            "config",
            "--add",
            "remote.origin.pushurl",
            "git@github.com:tickernelz/jcode.git",
        )
        result = self.fixture.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exactly one URL", result.stderr)

        self.fixture.git(self.fixture.fork_work, "config", "--unset-all", "remote.origin.pushurl")
        result = self.fixture.run_sync(expose_real_rewrites=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("resolved origin fetch", result.stderr)

    def test_refuses_rewritten_upstream_history(self) -> None:
        self.fixture.commit_and_push(
            self.fixture.upstream_work, "origin", "first.txt", "first\n", "first upstream"
        )
        first_sync = self.fixture.run_sync("--push")
        self.assertEqual(first_sync.returncode, 0, first_sync.stderr)
        fork_head = self.fixture.rev(self.fixture.origin, "refs/heads/master")

        self.fixture.git(self.fixture.upstream_work, "reset", "--hard", "HEAD^")
        (self.fixture.upstream_work / "rewrite.txt").write_text("rewritten\n")
        self.fixture.git(self.fixture.upstream_work, "add", "rewrite.txt")
        self.fixture.git(self.fixture.upstream_work, "commit", "-m", "rewritten upstream")
        self.fixture.git(self.fixture.upstream_work, "push", "--force", "origin", "master")

        result = self.fixture.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.fixture.rev(self.fixture.origin, "refs/heads/master"), fork_head)


class SyncWorkflowSafetyTests(unittest.TestCase):
    def test_write_job_never_executes_merged_repository_code(self) -> None:
        workflow = WORKFLOW.read_text()
        validate, publish = workflow.split("  publish:\n", 1)

        self.assertIn("permissions:\n  contents: read", workflow)
        self.assertIn("persist-credentials: false", validate)
        self.assertIn("needs: validate", publish)
        self.assertIn("permissions:\n      contents: write", publish)
        self.assertNotIn("scripts/", publish)
        self.assertNotIn("cargo ", publish)
        self.assertNotIn("secrets.", workflow)
        self.assertNotIn("git push --force", workflow)
        self.assertNotIn("git push -f", workflow)
        self.assertIn("git remote set-url --push upstream DISABLED", publish)


if __name__ == "__main__":
    unittest.main(verbosity=2)
