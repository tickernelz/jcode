#!/usr/bin/env python3
"""Static regression checks for Windows cargo timeout test invocations."""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WRAPPER = ROOT / "scripts/invoke_cargo_with_timeout.ps1"
TARGETED_RUNNER = ROOT / "scripts/run_windows_targeted_tests.ps1"
WORKFLOWS = [
    ROOT / ".github/workflows/ci.yml",
    ROOT / ".github/workflows/windows-smoke.yml",
]
TEST_CALLERS = [*WORKFLOWS, TARGETED_RUNNER]
PACKAGE_ROOTS = {
    "jcode": ROOT / "src",
    "jcode-base": ROOT / "crates/jcode-base",
    "jcode-app-core": ROOT / "crates/jcode-app-core",
    "jcode-tui": ROOT / "crates/jcode-tui",
}
EXPECTED_TARGETED_TESTS = [
    ("jcode-base", "command_candidates_adds_extension_on_windows"),
    ("jcode-base", "command_exists_for_known_binary"),
    ("jcode-base", "command_exists_absolute_path"),
    ("jcode-app-core", "sibling_socket_path_roundtrip"),
    ("jcode-app-core", "cleanup_socket_pair_removes_main_and_debug_files"),
    ("jcode-base", "is_process_running_reports_exited_children_as_stopped"),
    ("jcode-base", "spawn_replacement_process_returns_without_waiting_for_child_exit"),
    ("jcode-app-core", "build_shell_command_uses_cmd_and_executes_command"),
    ("jcode-base", "pipe_name_is_stable_and_normalizes_case_and_separators"),
    ("jcode-base", "pipe_name_falls_back_when_stem_is_empty"),
    ("jcode-base", "busy_pipe_is_reported_as_a_live_socket_path"),
    ("jcode-base", "stream_pair_round_trips_bytes"),
    ("jcode-base", "split_stream_supports_concurrent_read_and_write"),
    ("jcode", "auto_provider_noninteractive_skips_untrusted_external_auth_instead_of_blocking"),
    ("jcode-tui", "test_cancel_command_idle_reports_nothing_to_cancel"),
    ("jcode-tui", "test_menu_number_rejected_as_api_key"),
    ("jcode-tui", "test_command_palette_suppressed_while_api_key_prompt_pending"),
    ("jcode-tui", "test_ctrl_c_with_active_copy_selection_copies_instead_of_quitting"),
    ("jcode-tui", "test_ctrl_c_in_copy_mode_without_selection_still_falls_through"),
]
JOURNAL_TESTS = ROOT / "crates/jcode-base/src/session_tests/cases/part_3.rs"
EXPECTED_JOURNAL_TESTS = [
    "test_journal_replay_fails_closed_on_corrupt_middle_and_preserves_tail",
    "test_journal_replay_fails_closed_on_glued_entry_after_gap",
]


class WindowsCargoTimeoutTests(unittest.TestCase):
    def test_required_test_output_rejects_zero_tests(self):
        wrapper = WRAPPER.read_text()
        pattern = re.search(r"Matches\(\$output, '([^']+)'\)", wrapper).group(1)

        def executed(output: str) -> int:
            return sum(map(int, re.findall(pattern, output)))

        self.assertEqual(executed("running 0 tests\n"), 0)
        self.assertEqual(executed("running 1 test\n"), 1)
        self.assertEqual(executed("running 0 tests\nrunning 2 tests\n"), 2)
        self.assertIn("$executedTests.Sum -eq 0", wrapper)

    def test_every_wrapper_test_caller_requires_tests(self):
        for caller in TEST_CALLERS:
            text = caller.read_text()
            calls = text.split("invoke_cargo_with_timeout.ps1")[1:]
            self.assertTrue(calls, caller)
            for call in calls:
                block = call.split("-CargoArgs", 1)[0]
                self.assertIn("-RequireTests", block, caller)

    def test_targeted_runner_owns_valid_package_mappings(self):
        runner = TARGETED_RUNNER.read_text()
        tests = re.findall(
            r"Package = '([^']+)'; Name = '([^']+)'",
            runner,
        )
        self.assertEqual(tests, EXPECTED_TARGETED_TESTS)

        for package, name in tests:
            declaration = re.compile(rf"\b(?:async\s+)?fn\s+{re.escape(name)}\s*\(")
            sources = PACKAGE_ROOTS[package].rglob("*.rs")
            self.assertTrue(
                any(declaration.search(source.read_text()) for source in sources),
                f"{name} is not declared in package {package}",
            )

    def test_targeted_runner_precompiles_before_filtered_execution(self):
        runner = TARGETED_RUNNER.read_text()
        compile_command = "cargo test --locked --target $target -p $package --lib --no-run"
        execute_command = (
            "-CargoArgs @('test', '--locked', '--target', $target, '-p', "
            "$test.Package, '--lib', $test.Name, '--', '--nocapture')"
        )
        filtered_command = 'foreach ($test in $tests)'
        self.assertIn("$target = 'x86_64-pc-windows-msvc'", runner)
        self.assertIn(
            "$packages = $tests | ForEach-Object { $_.Package } | Sort-Object -Unique",
            runner,
        )
        self.assertIn(execute_command, runner)
        self.assertEqual(re.findall(r"-TimeoutSeconds\s+(\d+)", runner), ["300"])
        self.assertLess(runner.index(compile_command), runner.index(filtered_command))

    def test_workflows_share_the_targeted_runner(self):
        for workflow in WORKFLOWS:
            text = workflow.read_text()
            self.assertEqual(text.count("run_windows_targeted_tests.ps1"), 1, workflow)
            self.assertNotIn("command_candidates_adds_extension_on_windows", text, workflow)
        self.assertIn(
            "python3 scripts/test_windows_cargo_timeout.py",
            WORKFLOWS[0].read_text(),
        )

    def test_windows_smoke_journal_filters_are_exact_current_names(self):
        workflow = WORKFLOWS[1].read_text()
        declarations = JOURNAL_TESTS.read_text()
        for name in EXPECTED_JOURNAL_TESTS:
            self.assertIn(f"fn {name}()", declarations)
            self.assertIn(f"'{name}'", workflow)
        self.assertNotIn("test_journal_replay_skips_corrupt_line_and_keeps_tail", workflow)
        self.assertNotIn("test_journal_replay_salvages_glued_entries_on_torn_line", workflow)


if __name__ == "__main__":
    unittest.main()
