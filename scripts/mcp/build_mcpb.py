#!/usr/bin/env python3
"""Build the XERJ MCP Bundle (.mcpb) for one release.

The official MCP registry (registry.modelcontextprotocol.io), and the
catalogues that read from it (the VS Code / GitHub MCP gallery, PulseMCP),
list a server by pointing at a package. XERJ ships no npm/PyPI/crates.io/OCI
package, only GitHub Release binaries, so the registry route is an MCPB bundle
attached to a release. The registry requires the asset URL to contain "mcp"
and server.json to carry the bundle's SHA-256; MCP clients verify that hash.
Smithery takes the same file (see --omit-tools).

One cross-platform bundle holds the five release binaries:

  server/darwin-aarch64/xerj   server/darwin-x86_64/xerj
  server/linux-aarch64/xerj    server/linux-x86_64/xerj   (musl, static)
  server/windows-x86_64/xerj.exe

server/xerj-mcp.sh (packaging/mcpb/xerj-mcp.sh) picks the right binary on
macOS and Linux; Windows runs xerj.exe directly (manifest platform_overrides).
The manifest is packaging/mcpb/manifest.json with the version and the tool
list filled in; tools come from landing/docs/agents/schemas/mcp-tools.json.
Each archive is checked against its `.sha256` asset before use. Entries are
sorted and get a fixed timestamp, so the same release inputs always give the
same bytes and the same hash.

Run manually; nothing here touches CI. Either let the script download the
archives (needs `gh`):

    python3 scripts/mcp/build_mcpb.py --tag v1.0.0-rc.100 --out dist --update-server-json

or point it at archives you already downloaded:

    gh release download v1.0.0-rc.100 -R xerj-org/xerj -D dist/mcpb-in \\
        -p '*apple-darwin.tar.gz' -p '*linux-musl.tar.gz' \\
        -p '*x86_64-pc-windows-msvc.zip' -p '*.sha256'
    python3 scripts/mcp/build_mcpb.py --tag v1.0.0-rc.100 --assets dist/mcpb-in --out dist

Then attach the bundle to the release named by --upload-tag (default
mcpb-v<version>, the tag every published registry version uses) and publish:

    gh release create mcpb-v1.0.0-rc.100 -R xerj-org/xerj dist/xerj-mcp-1.0.0-rc.100.mcpb
    mcp-publisher login github && mcp-publisher publish

`--update-server-json` rewrites the mcpb package entry in server.json
(identifier URL, version, fileSha256). The hash only matches the exact file
this run produced, so upload that file, not a rebuild. Standard library only.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

REPO = "xerj-org/xerj"
ROOT = Path(__file__).resolve().parents[2]
PACKAGING = ROOT / "packaging" / "mcpb"
TEMPLATE = PACKAGING / "manifest.json"
LAUNCHER = PACKAGING / "xerj-mcp.sh"
TOOLS_SCHEMA = ROOT / "landing" / "docs" / "agents" / "schemas" / "mcp-tools.json"
SERVER_JSON = ROOT / "server.json"
FIXED = (2020, 1, 1, 0, 0, 0)

# bundle folder -> (release target triple, archive extension, binary name)
TARGETS = {
    "darwin-aarch64": ("aarch64-apple-darwin", "tar.gz", "xerj"),
    "darwin-x86_64": ("x86_64-apple-darwin", "tar.gz", "xerj"),
    "linux-aarch64": ("aarch64-unknown-linux-musl", "tar.gz", "xerj"),
    "linux-x86_64": ("x86_64-unknown-linux-musl", "tar.gz", "xerj"),
    "windows-x86_64": ("x86_64-pc-windows-msvc", "zip", "xerj.exe"),
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def archive_names(version: str) -> list[str]:
    return [f"xerj-{version}-{triple}.{ext}" for triple, ext, _ in TARGETS.values()]


def download(tag: str, version: str, dest: Path) -> None:
    cmd = ["gh", "release", "download", tag, "-R", REPO, "-D", str(dest), "--clobber"]
    for name in archive_names(version):
        cmd += ["-p", name, "-p", name + ".sha256"]
    subprocess.run(cmd, check=True)


def read_checked(archive: Path) -> bytes:
    if not archive.exists():
        sys.exit(f"missing {archive}")
    data = archive.read_bytes()
    side = archive.with_name(archive.name + ".sha256")
    if not side.exists():
        sys.exit(f"missing {side.name}: download the .sha256 files with the archives")
    want = side.read_text().split()[0].strip().lower()
    got = sha256(data)
    if got != want:
        sys.exit(f"checksum mismatch for {archive.name}: got {got}, release says {want}")
    return data


def extract_binary(archive: Path, member: str) -> bytes:
    data = read_checked(archive)
    stage = archive.name.removesuffix(".tar.gz").removesuffix(".zip")
    name = f"{stage}/{member}"
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as zf:
            return zf.read(name)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as tf:
        f = tf.extractfile(name)
        if f is None:
            sys.exit(f"{archive.name}: {name} not found")
        return f.read()


def tools_from_schema() -> list[dict]:
    schema = json.loads(TOOLS_SCHEMA.read_text())
    out = []
    for tool in schema["tools"]:
        first = re.split(r"(?<=\.)\s", tool["description"].strip(), maxsplit=1)[0]
        out.append({"name": tool["name"], "description": first})
    return out


def build_manifest(version: str, omit_tools: bool) -> bytes:
    manifest = json.loads(TEMPLATE.read_text())
    manifest["version"] = version
    if omit_tools:
        manifest.pop("tools", None)
        manifest["tools_generated"] = True
    else:
        manifest["tools"] = tools_from_schema()
    return json.dumps(manifest, indent=2, ensure_ascii=False).encode() + b"\n"


def zip_add(zf: zipfile.ZipFile, name: str, data: bytes, mode: int) -> None:
    info = zipfile.ZipInfo(name, date_time=FIXED)
    info.external_attr = (0o100000 | mode) << 16
    info.create_system = 3  # unix, so the exec bit survives
    info.compress_type = zipfile.ZIP_DEFLATED
    zf.writestr(info, data, compresslevel=9)


def build_bundle(version: str, assets: Path, omit_tools: bool) -> bytes:
    files = {
        "manifest.json": (build_manifest(version, omit_tools), 0o644),
        "server/xerj-mcp.sh": (LAUNCHER.read_bytes(), 0o755),
        "LICENSE": ((ROOT / "LICENSE").read_bytes(), 0o644),
    }
    for folder, (triple, ext, member) in TARGETS.items():
        archive = assets / f"xerj-{version}-{triple}.{ext}"
        files[f"server/{folder}/{member}"] = (extract_binary(archive, member), 0o755)
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        for name in sorted(files):
            data, mode = files[name]
            zip_add(zf, name, data, mode)
    return buf.getvalue()


def update_server_json(path: Path, version: str, url: str, digest: str) -> None:
    server = json.loads(path.read_text())
    server["version"] = version
    pkg = next((p for p in server.get("packages", []) if p.get("registryType") == "mcpb"), None)
    if pkg is None:
        sys.exit(f"{path}: no mcpb package entry")
    pkg["identifier"] = url
    pkg["fileSha256"] = digest
    pkg["version"] = version
    path.write_text(json.dumps(server, indent=2, ensure_ascii=False) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tag", required=True, help="release tag that holds the binaries, e.g. v1.0.0-rc.100")
    ap.add_argument("--assets", type=Path,
                    help="folder with the downloaded release archives and .sha256 files; "
                         "without it the archives are downloaded with gh")
    ap.add_argument("--out", type=Path, default=ROOT / "dist", help="output directory (default: dist/)")
    ap.add_argument("--upload-tag",
                    help="release the bundle is attached to, used in the server.json URL (default: mcpb-v<version>)")
    ap.add_argument("--update-server-json", action="store_true",
                    help="point server.json's mcpb package at this bundle")
    ap.add_argument("--server-json", type=Path, default=SERVER_JSON, help=argparse.SUPPRESS)
    ap.add_argument("--omit-tools", action="store_true",
                    help="leave the manifest tool list out (Smithery's CLI rejects declared tools); "
                         "never use this copy as the registry asset, its hash differs")
    args = ap.parse_args()
    if args.omit_tools and args.update_server_json:
        ap.error("--omit-tools builds the Smithery copy; it cannot be the server.json asset")

    version = args.tag.removeprefix("v")
    upload_tag = args.upload_tag or f"mcpb-v{version}"
    if args.assets:
        blob = build_bundle(version, args.assets, args.omit_tools)
    else:
        with tempfile.TemporaryDirectory() as tmp:
            download(args.tag, version, Path(tmp))
            blob = build_bundle(version, Path(tmp), args.omit_tools)

    args.out.mkdir(parents=True, exist_ok=True)
    bundle = args.out / f"xerj-mcp-{version}.mcpb"
    bundle.write_bytes(blob)
    digest = sha256(blob)
    url = f"https://github.com/{REPO}/releases/download/{upload_tag}/{bundle.name}"
    print(f"bundle   {bundle} ({len(blob)} bytes)")
    print(f"sha256   {digest}")
    print(f"url      {url}")
    if args.update_server_json:
        update_server_json(args.server_json, version, url, digest)
        print(f"updated  {args.server_json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
