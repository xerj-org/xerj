#!/usr/bin/env python3
"""Exercise control-file generation; packaging executables are local test doubles."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class DebianVersionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.tools = self.root / "bin"
        self.tools.mkdir()
        for name, body in {
            "dpkg": "exit 0",
            "readelf": "printf 'Shared library: [libc.so.6]\\n'",
            "objdump": "printf 'GLIBC_2.17\\n'",
            "sha256sum": "printf 'fixture-checksum  %s\\n' \"$1\"",
            "dpkg-deb": '''case "$1" in
--root-owner-group) cp "$3/DEBIAN/control" "$4"; cp "$3/usr/bin/xerj" "$4.payload";;
--info) cat "$2";;
*) exit 90;;
esac''',
        }.items():
            path = self.tools / name
            path.write_text("#!/bin/sh\n" + body + "\n")
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.tools) + os.pathsep + os.environ.get("PATH", ""))
        self.script = Path(__file__).with_name("build-deb.sh")

    def package(self, version):
        name = "xerj-" + version + "-x86_64-unknown-linux-gnu"
        stage = self.root / name
        stage.mkdir()
        binary = stage / "xerj"
        binary.write_bytes(b"staged executable fixture, never executed\n")
        binary.chmod(0o755)
        (stage / "README.md").write_text("fixture")
        (stage / "LICENSE").write_text("fixture")
        output = self.root / "dist"
        result = subprocess.run(["bash", str(self.script), "--stage-dir", str(stage),
            "--target", "x86_64-unknown-linux-gnu", "--version", version, "--out-dir", str(output)],
            env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        artifact = output / (name + ".deb")
        self.assertTrue(artifact.exists())
        self.assertEqual(Path(str(artifact) + ".payload").read_bytes(), binary.read_bytes())
        fields = dict(line.split(": ", 1) for line in artifact.read_text().splitlines() if ": " in line and not line.startswith(" "))
        return fields["Version"]

    def test_release_candidate_sorts_before_final(self):
        self.assertEqual(self.package("1.0.0-rc.76"), "1.0.0~rc.76")

    def test_final_version_is_unchanged(self):
        self.assertEqual(self.package("1.0.0"), "1.0.0")

    def test_build_metadata_hyphen_is_not_a_prerelease(self):
        self.assertEqual(self.package("1.0.0+build-2026"), "1.0.0+build-2026")

    @unittest.skipUnless(shutil.which("dpkg"), "native dpkg unavailable; metadata generation still tested")
    def test_native_debian_upgrade_order(self):
        old, new = self.package("1.0.0-rc.76"), self.package("1.0.0")
        result = subprocess.run([shutil.which("dpkg"), "--compare-versions", old, "lt", new], timeout=10)
        self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
