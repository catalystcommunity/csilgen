"""Tests for the csilgen runnerlib plugin."""

from __future__ import annotations

import importlib.util
import hashlib
import io
import json
import os
import re
import sys
import tempfile
import unittest
import datetime as dt
from contextlib import redirect_stdout
from pathlib import Path
from unittest import mock

import yaml
import src.workflow as runner_workflow
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
                {"CSILGEN_JOB": "core"},
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


class WorkflowVariablesTests(unittest.TestCase):
    def test_remote_inline_input_does_not_require_the_local_file(self) -> None:
        environment = {
            "RC_WF_VARS_FILE": "/job/workflow-vars.json",
            "RC_WF_VARS_JSON": '{"asset_cache_lane":"v0.2.2"}',
        }
        with (
            mock.patch.dict(os.environ, environment, clear=True),
            mock.patch.object(runner_workflow, "_global_context", None),
        ):
            self.assertEqual(
                PLUGIN._workflow_vars(),
                {"asset_cache_lane": "v0.2.2"},
            )

    def test_local_input_uses_the_workflow_variables_file(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "workflow-vars.json"
            path.write_text(
                '{"asset_cache_lane":"v0.2.2"}',
                encoding="utf-8",
            )
            with (
                mock.patch.dict(
                    os.environ,
                    {"RC_WF_VARS_FILE": str(path)},
                    clear=True,
                ),
                mock.patch.object(runner_workflow, "_global_context", None),
            ):
                self.assertEqual(
                    PLUGIN._workflow_vars(),
                    {"asset_cache_lane": "v0.2.2"},
                )

    def test_asset_job_uses_remote_inline_upload_map(self) -> None:
        asset = "transport-c.tar.gz"
        uploads = {
            asset: {
                "asset": "https://cache.example.test/asset",
                "sha256": "https://cache.example.test/digest",
            }
        }
        environment = {
            "CSILGEN_ASSET_ITEM": "c",
            "CSILGEN_ASSET_KIND": "transport",
            "RC_WF_VARS_JSON": json.dumps(
                {"asset_cache_uploads": uploads}
            ),
        }
        with tempfile.TemporaryDirectory() as directory:
            archive = Path(directory) / asset
            archive.write_bytes(b"archive")
            with (
                mock.patch.dict(os.environ, environment, clear=True),
                mock.patch.object(runner_workflow, "_global_context", None),
                mock.patch.object(
                    PLUGIN,
                    "_build_cache_asset",
                    return_value=archive,
                ),
                mock.patch.object(PLUGIN, "_put_presigned") as put_presigned,
            ):
                PLUGIN._build_and_upload_asset(ROOT)

        self.assertEqual(put_presigned.call_count, 2)
        self.assertEqual(
            put_presigned.call_args_list[0].args[0],
            uploads[asset]["asset"],
        )
        self.assertEqual(put_presigned.call_args_list[0].args[1], b"archive")
        self.assertEqual(
            put_presigned.call_args_list[1].args[0],
            uploads[asset]["sha256"],
        )


class TrustedImplementationTests(unittest.TestCase):
    def test_install_commands_cover_all_release_generators(self) -> None:
        tools = (ROOT / "tools.sh").read_text(encoding="utf-8")
        match = re.search(
            r"CSILGEN_GENERATOR_PACKAGES=\(\n(?P<packages>.*?)\n\)",
            tools,
            re.DOTALL,
        )
        self.assertIsNotNone(match)
        packages = tuple(match.group("packages").split())
        self.assertEqual(packages, PLUGIN.GENERATOR_PACKAGES)
        self.assertIn("build-install-all)", tools)
        self.assertIn("install-all)", tools)

    def test_toolchain_image_does_not_copy_tested_source(self) -> None:
        dockerfile = (ROOT / "tools/ci-image/Dockerfile").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("COPY .", dockerfile)
        self.assertIn("mkdir -p /job/src /job/ci", dockerfile)
        self.assertIn(
            "containers.catalystsquad.com/public/reactorcide/runnerbase:latest",
            dockerfile,
        )

    def test_slow_jobs_use_the_published_toolchain_image(self) -> None:
        expected = (
            "containers.catalystsquad.com/private/catalystcommunity/"
            "csilgen/ci-toolchain@sha256:"
            "3bc81e33f1abfac291e663e6692428a8376f3d1c9d44fd47dfb38d7726a07a90"
        )
        for name in ("test-interop.yaml", "test-transports.yaml"):
            with self.subTest(name=name):
                job = yaml.safe_load(
                    (ROOT / ".reactorcide/jobs" / name).read_text(
                        encoding="utf-8"
                    )
                )
                self.assertEqual(job["job"]["image"], expected)
                self.assertNotIn("image_pull_secrets", job["job"])

    def test_interop_launches_programs_without_shell_wrappers(self) -> None:
        for language in INTEROP.LANGUAGES:
            with self.subTest(language=language.name):
                command, _, _ = INTEROP._launch(ROOT, language)
                self.assertNotIn(command[0], {"bash", "sh"})
                self.assertFalse(command[0].endswith("/run"))
                self.assertFalse(command[0].endswith(".sh"))

    def test_interop_builds_the_selected_wasm_generators(self) -> None:
        languages = INTEROP._select_languages(("go", "typescript"))
        completed = mock.Mock()
        with mock.patch.object(
            INTEROP.subprocess,
            "run",
            return_value=completed,
        ) as run:
            INTEROP._build_generators(ROOT, languages)

        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual(
            commands[0],
            (
                "cargo",
                "build",
                "--release",
                "--quiet",
                "--target",
                "wasm32-unknown-unknown",
                "--package",
                "csilgen-go-generator",
                "--package",
                "csilgen-typescript-generator",
            ),
        )

    def test_interop_run_builds_generators_before_cli_use(self) -> None:
        with mock.patch.object(
            INTEROP,
            "_build_generators",
            side_effect=RuntimeError("stop after generator build"),
        ) as build_generators:
            with self.assertRaisesRegex(RuntimeError, "stop after generator build"):
                INTEROP.run(ROOT, ("rust",), ("rpc",))

        build_generators.assert_called_once_with(
            ROOT.resolve(),
            INTEROP._select_languages(("rust",)),
        )

    def test_interop_prepares_clients_and_sharded_servers(self) -> None:
        with mock.patch.object(
            INTEROP,
            "_build_generators",
            side_effect=RuntimeError("stop after generator build"),
        ) as build_generators:
            with self.assertRaisesRegex(RuntimeError, "stop after generator build"):
                INTEROP.run(
                    ROOT,
                    ("rust",),
                    ("rpc",),
                    ("go",),
                )

        build_generators.assert_called_once_with(
            ROOT.resolve(),
            INTEROP._select_languages(("rust", "go")),
        )

    def test_interop_report_counts_only_the_shard(self) -> None:
        clients = INTEROP._select_languages(("rust", "go"))
        servers = INTEROP._select_languages(("python",))
        results = [
            ("rpc", "rust", "python", True, []),
            ("rpc", "go", "python", True, []),
        ]
        output = io.StringIO()

        with redirect_stdout(output):
            INTEROP._report(clients, servers, ("rpc",), results)

        self.assertIn("interop summary: 2/2 cells passed", output.getvalue())

    def test_interop_shard_environment_is_parsed(self) -> None:
        with mock.patch.dict(
            os.environ,
            {"CSILGEN_INTEROP_SERVERS": "rust,go"},
            clear=False,
        ):
            self.assertEqual(
                PLUGIN._interop_server_names(),
                ("rust", "go"),
            )

    def test_pr_workflow_defines_four_interop_shards(self) -> None:
        workflow = yaml.safe_load(
            (ROOT / ".reactorcide/workflows/pr.yaml").read_text(
                encoding="utf-8"
            )
        )
        interop = workflow["jobs"]["test-interop"]
        shards = [
            name
            for shard in interop["for_each"]
            for name in shard.split(",")
        ]

        self.assertEqual(len(interop["for_each"]), 4)
        self.assertEqual(interop["item_var"], "CSILGEN_INTEROP_SERVERS")
        self.assertEqual(
            sorted(shards),
            sorted(language.name for language in INTEROP.LANGUAGES),
        )

    def test_unified_release_config_uses_one_target(self) -> None:
        config = yaml.safe_load(
            (ROOT / PLUGIN.SEMVER_TAGS_CONFIG).read_text(encoding="utf-8")
        )
        self.assertTrue(config["skip_short_versions"])
        self.assertEqual(
            config["targets"], [{"name": "csilgen", "paths": ["."]}]
        )

    def test_default_semver_config_remains_base_ci_compatible(self) -> None:
        config = yaml.safe_load(
            (ROOT / ".semver-tags.yaml").read_text(encoding="utf-8")
        )
        targets = [target["name"] for target in config["targets"]]
        generator_targets = [
            "generator-"
            + package.removeprefix("csilgen-").removesuffix("-generator")
            for package in PLUGIN.GENERATOR_PACKAGES
        ]
        expected = [
            "csilgen-core",
            *generator_targets,
            *(f"transport-{name}" for name in PLUGIN.TRANSPORTS),
        ]
        self.assertEqual(targets, expected)
        self.assertTrue(
            all(
                target["paths"]
                == [".reactorcide/legacy-base-ci-only"]
                for target in config["targets"]
            )
        )


class ReleaseTests(unittest.TestCase):
    def test_semver_tags_uses_unified_release_config(self) -> None:
        binary = Path("/tools/semver-tags")
        completed = mock.Mock(stdout="{}")
        with (
            mock.patch.object(
                PLUGIN,
                "_semver_tags_binary",
                return_value=(binary, {}),
            ),
            mock.patch.object(
                PLUGIN, "_run", return_value=completed
            ) as run,
        ):
            self.assertEqual(PLUGIN._semver_tags(ROOT, dry_run=True), {})

        command = run.call_args.args[0]
        self.assertEqual(
            command[:4],
            [
                str(binary),
                "--config",
                str(ROOT / PLUGIN.SEMVER_TAGS_CONFIG),
                "run",
            ],
        )
        self.assertIn("--dry_run", command)

    def test_release_has_twenty_unique_logical_assets(self) -> None:
        self.assertEqual(len(PLUGIN.EXPECTED_CACHE_ASSETS), 20)
        self.assertEqual(len(set(PLUGIN.EXPECTED_CACHE_ASSETS)), 20)
        self.assertEqual(len(PLUGIN.CLI_PLATFORMS), 4)
        self.assertEqual(len(PLUGIN.TRANSPORT_ASSETS), 15)
        self.assertIn(PLUGIN.GENERATOR_ASSET, PLUGIN.EXPECTED_CACHE_ASSETS)

    def test_generator_archive_contains_all_production_wasm_files(self) -> None:
        output = ROOT / "target" / "release-artifacts"
        with mock.patch.object(PLUGIN, "_tar_files") as tar_files:
            archive = PLUGIN._archive_generators(
                ROOT, output, "generators.tar.gz"
            )

        self.assertEqual(archive, output / "generators.tar.gz")
        files = tar_files.call_args.args[1]
        names = [name for _, name in files]
        self.assertEqual(len(names), 18)
        self.assertEqual(names[-1], "LICENSE")
        self.assertEqual(len(set(names[:-1])), 17)
        self.assertFalse(any("noop" in name for name in names))

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
        self.assertEqual(plans[0].package, "csilgen")
        self.assertEqual(plans[0].tag, "csilgen/v0.2.0")

    def test_release_tag_builds_one_unified_release(self) -> None:
        root = ROOT
        output = root / "target" / "release-artifacts"
        with (
            mock.patch.object(
                PLUGIN,
                "_build_all_release_artifacts",
                return_value=output,
            ) as build,
        ):
            result = PLUGIN._build_tag_artifacts(
                root,
                "csilgen/v1.2.3",
            )

        self.assertEqual(result, output)
        build.assert_called_once_with(
            root, {"csilgen": "1.2.3"}
        )


class AssetCacheTests(unittest.TestCase):
    class MemoryCache:
        def __init__(self, objects: dict[str, bytes] | None = None) -> None:
            self.objects = dict(objects or {})
            self.copies: list[tuple[str, str]] = []

        def get_bytes(self, key: str) -> bytes:
            if key not in self.objects:
                raise FileNotFoundError(key)
            return self.objects[key]

        def put_bytes(self, key: str, content: bytes) -> None:
            self.objects[key] = content

        def copy(self, source: str, destination: str) -> None:
            self.objects[destination] = self.get_bytes(source)
            self.copies.append((source, destination))

        def delete(self, key: str) -> None:
            self.objects.pop(key, None)

    def test_lane_names_identify_exact_source_states(self) -> None:
        sha = "a" * 40
        self.assertEqual(PLUGIN.ASSET_CACHE.pr_lane("12", sha), "pr-12-aaaaaaaaaaaa")
        self.assertEqual(PLUGIN.ASSET_CACHE.main_lane(sha), "main-aaaaaaaaaaaa")
        self.assertEqual(PLUGIN.ASSET_CACHE.version_lane("1.2.3"), "v1.2.3")

    def test_presign_does_not_include_the_secret_key(self) -> None:
        cache = PLUGIN.ASSET_CACHE.S3Cache(
            "https://cache.example.test",
            "cache-bucket",
            "access-key",
            "secret-key-value",
        )
        url = cache.presign("PUT", "csilgen/lane/asset.tar.gz", expires=60)

        self.assertNotIn("secret-key-value", url)
        self.assertIn("X-Amz-Signature=", url)
        self.assertIn("X-Amz-Expires=60", url)

    def test_seal_copies_staging_objects_before_manifest(self) -> None:
        lane = "pr-12-aaaaaaaaaaaa"
        objects: dict[str, bytes] = {}
        uploads: dict[str, dict[str, str]] = {}
        for asset in PLUGIN.EXPECTED_CACHE_ASSETS:
            content = ("content-" + asset).encode()
            staging = "staging-" + asset
            objects[PLUGIN.ASSET_CACHE.object_key(lane, staging)] = content
            objects[PLUGIN.ASSET_CACHE.object_key(lane, staging + ".sha256")] = (
                hashlib.sha256(content).hexdigest() + "\n"
            ).encode()
            uploads[asset] = {"asset": "hidden", "sha256": "hidden"}
        cache = self.MemoryCache(objects)
        variables = {
            "asset_cache_lane": lane,
            "asset_cache_source_sha": "a" * 40,
            "asset_cache_source_tree": "b" * 40,
            "asset_cache_uploads": uploads,
        }

        with (
            mock.patch.object(PLUGIN, "_workflow_vars", return_value=variables),
            mock.patch.object(
                PLUGIN.ASSET_CACHE.S3Cache,
                "from_environment",
                return_value=cache,
            ),
        ):
            PLUGIN._seal_asset_lane(ROOT)

        manifest_key = PLUGIN.ASSET_CACHE.object_key(
            lane, PLUGIN.ASSET_CACHE.MANIFEST
        )
        manifest = PLUGIN.ASSET_CACHE.decode_manifest(cache.objects[manifest_key])
        self.assertEqual(len(manifest["assets"]), 20)
        self.assertEqual(len(cache.copies), 20)
        self.assertFalse(any("/staging-" in key for key in cache.objects))

    def test_missing_pr_lane_uses_tag_build_fallback(self) -> None:
        pull = {
            "merged": True,
            "merge_commit_sha": "b" * 40,
            "base": {
                "ref": "main",
                "repo": {"full_name": "catalystcommunity/csilgen"},
            },
            "head": {
                "sha": "a" * 40,
                "repo": {"full_name": "example/csilgen"},
            },
        }
        plan = PLUGIN.ReleasePlan(
            "csilgen", True, "1.2.3", "csilgen/v1.2.3", "b" * 40, "notes"
        )
        cache = self.MemoryCache()
        with (
            mock.patch.dict(os.environ, {"REACTORCIDE_PR_NUMBER": "12"}),
            mock.patch.object(PLUGIN, "_github_request", return_value=pull),
            mock.patch.object(
                PLUGIN.ASSET_CACHE.S3Cache,
                "from_environment",
                return_value=cache,
            ),
        ):
            promoted = PLUGIN._promote_merged_pr_assets(
                ROOT, "token", "catalystcommunity/csilgen", plan
            )

        self.assertFalse(promoted)

    def test_release_workflow_has_fanout_seal_and_always_cleanup(self) -> None:
        workflow = yaml.safe_load(
            (ROOT / ".reactorcide/workflows/release.yaml").read_text(
                encoding="utf-8"
            )
        )
        jobs = workflow["jobs"]
        self.assertEqual(len(jobs["asset-cli"]["for_each"]), 4)
        self.assertEqual(len(jobs["asset-transports"]["for_each"]), 15)
        self.assertEqual(
            set(jobs["asset-seal"]["depends_on"]),
            {
                "asset-cli",
                "asset-generators",
                "asset-transports",
                "release-test-core",
                "release-test-generators",
                "release-test-transports",
                "release-test-interop",
            },
        )
        self.assertEqual(jobs["asset-cleanup"]["condition"], "always")
        self.assertEqual(jobs["asset-cleanup"]["depends_on"], ["release"])

    def test_cleanup_keeps_a_lane_with_recent_staging_objects(self) -> None:
        now = dt.datetime.now(dt.timezone.utc)
        objects: dict[str, bytes] = {}
        infos: list[object] = []

        def manifest(lane: str, created_at: float) -> None:
            key = PLUGIN.ASSET_CACHE.object_key(
                lane, PLUGIN.ASSET_CACHE.MANIFEST
            )
            value = {
                "schema": 1,
                "project": "csilgen",
                "lane": lane,
                "source_sha": "a" * 40,
                "source_tree": "b" * 40,
                "created_at": created_at,
                "assets": [
                    {"name": name, "sha256": "0" * 64, "size": 1}
                    for name in PLUGIN.EXPECTED_CACHE_ASSETS
                ],
            }
            objects[key] = PLUGIN.ASSET_CACHE.encode_manifest(value)
            infos.append(
                PLUGIN.ASSET_CACHE.ObjectInfo(
                    key, len(objects[key]), dt.datetime.fromtimestamp(created_at, dt.timezone.utc)
                )
            )

        for index in range(1, 7):
            manifest(f"v1.0.{index}", (now - dt.timedelta(days=index)).timestamp())
        retry_lane = "pr-10-aaaaaaaaaaaa"
        manifest(retry_lane, (now - dt.timedelta(days=30)).timestamp())
        staging_key = PLUGIN.ASSET_CACHE.object_key(retry_lane, "staging-generators.tar.gz")
        objects[staging_key] = b"active"
        infos.append(PLUGIN.ASSET_CACHE.ObjectInfo(staging_key, 6, now))
        abandoned_key = PLUGIN.ASSET_CACHE.object_key(
            "pr-9-bbbbbbbbbbbb", "staging-generators.tar.gz"
        )
        objects[abandoned_key] = b"old"
        infos.append(
            PLUGIN.ASSET_CACHE.ObjectInfo(
                abandoned_key, 3, now - dt.timedelta(days=40)
            )
        )
        cache = self.MemoryCache(objects)
        cache.list = mock.Mock(
            side_effect=lambda prefix: [item for item in infos if item.key.startswith(prefix)]
        )

        with mock.patch.object(
            PLUGIN.ASSET_CACHE.S3Cache,
            "from_environment",
            return_value=cache,
        ):
            PLUGIN._cleanup_asset_cache()

        self.assertIn(staging_key, cache.objects)
        self.assertNotIn(abandoned_key, cache.objects)


if __name__ == "__main__":
    unittest.main()
