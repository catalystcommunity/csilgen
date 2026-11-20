"""Check release selection and installation with local release assets."""

import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]


class ReleaseInstallTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.modules = {
            path.name.replace("-", "_") + ".wasm"
            for path in (ROOT / "wasm").glob("csilgen-*-generator")
            if path.name != "csilgen-noop-generator"
        }
        self.metadata = self.root / "releases.json"
        # Select a known platform for both current and legacy asset names.
        uname = self.root / "uname"
        uname.write_text('#!/bin/sh\ncase "$1" in -s) echo Linux;; -m) echo x86_64;; esac\n')
        uname.chmod(0o755)
        self.env = {
            **os.environ,
            "PATH": f"{self.root}:{os.environ['PATH']}",
            "CSILGEN_GITHUB_API": self.metadata.as_uri(),
            "CARGO_HOME": str(self.root / "cargo"),
            "CSILGEN_GENERATOR_DIR": str(self.root / "generators"),
            "CSILGEN_BIN_DIR": str(self.root / "bin"),
        }

    def archive(self, name, members):
        path = self.root / name
        with tarfile.open(path, "w:gz") as archive:
            for member, data in members.items():
                info = tarfile.TarInfo(member)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        return {"name": name, "browser_download_url": path.as_uri()}

    def release(self, version, platform):
        binary = f"#!/bin/sh\necho csilgen {version}\n".encode()
        modules = {name: version.encode() for name in self.modules}
        return {
            "tag_name": f"csilgen/v{version}",
            "assets": [
                self.archive(f"csilgen-{version}-{platform}.tar.gz", {"csilgen": binary}),
                self.archive(f"csilgen-generators-{version}.tar.gz", modules),
            ],
        }

    def install(self, metadata, *args):
        self.metadata.write_text(json.dumps(metadata))
        return subprocess.run(
            [ROOT / "tools.sh", "install-all", *args],
            env=self.env, capture_output=True, text=True,
        )

    def test_latest_and_exact_install_matching_cli_and_generators(self):
        newest = self.release("1.2.0", "x86_64-unknown-linux-gnu")
        older = self.release("1.1.0", "linux-x86_64")
        tag = older["tag_name"]
        for metadata, args, version in [
            ([newest, older], [], "1.2.0"),
            ([newest, older], [tag], "1.1.0"),
            (older, [tag], "1.1.0"),
        ]:
            with self.subTest(args=args, version=version):
                result = self.install(metadata, *args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(f"Installed GitHub Release: csilgen/v{version}", result.stdout)
                cli = self.root / "bin" / "csilgen"
                self.assertEqual(subprocess.check_output([cli, "--version"], text=True).strip(),
                                 f"csilgen {version}")
                installed = list((self.root / "generators").glob("*.wasm"))
                self.assertEqual({module.name for module in installed}, self.modules)
                self.assertTrue(all(module.read_text() == version for module in installed))

    def test_missing_tag_and_platform_fail_before_install(self):
        release = {"tag_name": "csilgen/v1.0.0", "assets": []}
        for tag, diagnostic in [
            ("csilgen/v9.0.0", "No published release matches csilgen/v9.0.0"),
            ("csilgen/v1.0.0", "csilgen-1.0.0-x86_64-unknown-linux-gnu.tar.gz"),
        ]:
            result = self.install(release, tag)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(diagnostic, result.stderr)
            self.assertFalse((self.root / "bin").exists())
