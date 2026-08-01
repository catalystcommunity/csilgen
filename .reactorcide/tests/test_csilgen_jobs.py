"""Tests for the csilgen runnerlib plugin."""

from __future__ import annotations

import importlib.util
import json
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

from src.plugins import PluginManager


ROOT = Path(__file__).resolve().parents[2]
PLUGIN_PATH = ROOT / ".reactorcide" / "plugins" / "plugin_csilgen_jobs.py"
SPEC = importlib.util.spec_from_file_location("plugin_csilgen_jobs", PLUGIN_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("The csilgen runnerlib plugin is not available")
PLUGIN = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PLUGIN
SPEC.loader.exec_module(PLUGIN)

INTEROP_PATH = ROOT / ".reactorcide" / "scripts" / "interop.py"
INTEROP_SPEC = importlib.util.spec_from_file_location(
    "csilgen_interop",
    INTEROP_PATH,
)
if INTEROP_SPEC is None or INTEROP_SPEC.loader is None:
    raise RuntimeError("The csilgen interop implementation is not available")
INTEROP = importlib.util.module_from_spec(INTEROP_SPEC)
sys.modules[INTEROP_SPEC.name] = INTEROP
INTEROP_SPEC.loader.exec_module(INTEROP)


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
    def test_runnerlib_loader_imports_plugin(self) -> None:
        manager = PluginManager()
        manager.load_plugin_from_file(str(PLUGIN_PATH))
        self.assertIn("csilgen_jobs", manager.list_plugins())

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
            mock.patch.dict(
                os.environ,
                {"REACTORCIDE_CSILGEN_JOB": "core"},
                clear=False,
            ),
            mock.patch.object(PLUGIN, "_test_core") as test_core,
        ):
            PLUGIN.CsilgenJobsPlugin().execute(context)
        test_core.assert_called_once_with(ROOT)


class CommandTests(unittest.TestCase):
    def test_path_arguments_are_converted_to_strings(self) -> None:
        completed = mock.Mock()
        with mock.patch.object(
            PLUGIN.subprocess,
            "run",
            return_value=completed,
        ) as run:
            PLUGIN._run(("example", Path("input")), cwd=ROOT)

        self.assertEqual(run.call_args.args[0], ("example", "input"))

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


class TrustedImplementationTests(unittest.TestCase):
    def test_toolchain_image_does_not_copy_tested_source(self) -> None:
        self.assertNotIn("COPY .", PLUGIN.CI_TOOLCHAIN_DOCKERFILE)
        self.assertIn("mkdir -p /job/src /job/ci", PLUGIN.CI_TOOLCHAIN_DOCKERFILE)

    def test_interop_launches_programs_without_shell_wrappers(self) -> None:
        for language in INTEROP.LANGUAGES:
            with self.subTest(language=language.name):
                command, _, _ = INTEROP._launch(ROOT, language)
                self.assertNotIn(command[0], {"bash", "sh"})
                self.assertFalse(command[0].endswith("/run"))
                self.assertFalse(command[0].endswith(".sh"))

    def test_named_release_targets_do_not_use_marker_paths(self) -> None:
        config = (ROOT / ".semver-tags.yaml").read_text(encoding="utf-8")
        self.assertIn("targets:", config)
        self.assertNotIn(".release-targets", config)
        self.assertIn("skip_short_versions: true", config)


class ReleaseTests(unittest.TestCase):
    def test_release_plans_parse_all_targets(self) -> None:
        packages = list(PLUGIN.RELEASE_PACKAGES)
        versions = ["0.2.0"] * len(packages)
        tags = [f"{package}/v0.2.0" for package in packages]
        metadata = {
            "Release_package": ",".join(packages),
            "New_release_published": ",".join(
                "true" if index == 0 else "false"
                for index in range(len(packages))
            ),
            "New_release_version": ",".join(versions),
            "New_release_git_tag": ",".join(tags),
            "New_release_git_head": ",".join(["abc123"] * len(packages)),
            "New_release_notes_json": json.dumps(
                {
                    "new_release_notes_escaped": {
                        f"package_{package}": ["feat: initial release"]
                        for package in packages
                    }
                }
            ),
        }

        plans = PLUGIN._release_plans(metadata)

        self.assertEqual(len(plans), len(packages))
        self.assertTrue(plans[0].published)
        self.assertFalse(plans[1].published)
        self.assertEqual(plans[0].tag, "csilgen-core/v0.2.0")

    def test_release_tag_selects_one_generator(self) -> None:
        root = ROOT
        output = root / "target" / "release-artifacts"
        with (
            mock.patch.object(PLUGIN, "_release_output", return_value=output),
            mock.patch.object(PLUGIN, "_build_wasm") as build_wasm,
            mock.patch.object(PLUGIN, "_archive_generator") as archive,
        ):
            result = PLUGIN._build_tag_artifacts(
                root,
                "generator-rust/v1.2.3",
            )

        self.assertEqual(result, output)
        build_wasm.assert_called_once_with(
            root,
            release=True,
            packages=("csilgen-rust-generator",),
        )
        archive.assert_called_once_with(
            root,
            "csilgen-rust-generator",
            "1.2.3",
            output,
        )


if __name__ == "__main__":
    unittest.main()
