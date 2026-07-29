"""Tests for the csilgen runnerlib plugin."""

from __future__ import annotations

import importlib.util
import os
import sys
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
PLUGIN_PATH = ROOT / ".reactorcide" / "plugins" / "plugin_csilgen_jobs.py"
SPEC = importlib.util.spec_from_file_location("plugin_csilgen_jobs", PLUGIN_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("The csilgen runnerlib plugin is not available")
PLUGIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PLUGIN
SPEC.loader.exec_module(PLUGIN)


class ConventionalCommitTests(unittest.TestCase):
    def test_supported_subjects_match(self) -> None:
        subjects = (
            "feat: add a generator",
            "fix(cli): correct the output",
            "ci!: replace the release job",
        )
        for subject in subjects:
            with self.subTest(subject=subject):
                self.assertIsNotNone(PLUGIN.CONVENTIONAL_COMMIT.fullmatch(subject))

    def test_unsupported_subject_does_not_match(self) -> None:
        self.assertIsNone(
            PLUGIN.CONVENTIONAL_COMMIT.fullmatch("Add a generator")
        )


class DispatchTests(unittest.TestCase):
    def test_dispatch_uses_post_source_prep(self) -> None:
        plugin = PLUGIN.CsilgenJobsPlugin()
        self.assertEqual(
            plugin.supported_phases(),
            [PLUGIN.PluginPhase.POST_SOURCE_PREP],
        )

    def test_dispatch_runs_selected_job(self) -> None:
        context = mock.Mock()
        context.config.code_dir = str(ROOT)
        context.metadata = {}
        with (
            mock.patch.dict(os.environ, {"CSILGEN_JOB": "core"}, clear=False),
            mock.patch.object(PLUGIN, "_test_core") as test_core,
        ):
            PLUGIN.CsilgenJobsPlugin().execute(context)
        test_core.assert_called_once_with(ROOT)


class CommandTests(unittest.TestCase):
    def test_sensitive_arguments_are_redacted(self) -> None:
        completed = mock.Mock()
        with (
            mock.patch.object(PLUGIN, "log_stdout") as log_stdout,
            mock.patch.object(
                PLUGIN.subprocess,
                "run",
                return_value=completed,
            ),
        ):
            result = PLUGIN._run(
                ("example", "secret-value"),
                cwd=ROOT,
                sensitive=("secret-value",),
            )
        self.assertIs(result, completed)
        log_stdout.assert_called_once_with("+ example [REDACTED]")


if __name__ == "__main__":
    unittest.main()
