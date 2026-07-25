#!/usr/bin/env python3
"""Static regression checks for Windows cargo timeout test invocations."""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WRAPPER = ROOT / "scripts/invoke_cargo_with_timeout.ps1"
WORKFLOWS = [
    ROOT / ".github/workflows/ci.yml",
    ROOT / ".github/workflows/windows-smoke.yml",
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
        for workflow in WORKFLOWS:
            text = workflow.read_text()
            calls = text.split("invoke_cargo_with_timeout.ps1")[1:]
            self.assertTrue(calls, workflow)
            for call in calls:
                block = call.split("-CargoArgs", 1)[0]
                self.assertIn("-RequireTests", block, workflow)

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
