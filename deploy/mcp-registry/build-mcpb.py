#!/usr/bin/env python3
"""Build the XERJ MCP Bundle (.mcpb) for one release and stamp server.json.

The official MCP Registry (registry.modelcontextprotocol.io) lists a server by
pointing at a package. XERJ ships no npm/PyPI/crates.io/OCI package, only
GitHub Release binaries, so the registry route is an MCPB bundle uploaded as
a release asset. The registry requires the asset URL to contain "mcp" and
server.json to carry the bundle's SHA-256; MCP clients verify that hash.

One cross-platform bundle holds the five release binaries:
  server/darwin-aarch64/xerj   server/darwin-x86_64/xerj
  server/linux-aarch64/xerj    server/linux-x86_64/xerj   (musl, static)
  server/windows-x86_64/xerj.exe
server/xerj-mcp.sh picks the right one on macOS/Linux; Windows runs xerj.exe
directly (manifest platform_overrides). Every entry gets a fixed timestamp and
the list is sorted, so the same release inputs always give the same bytes and
the same hash.

Usage (from the repo root):
  gh release download v1.0.0-rc.97 -R xerj-org/xerj -D dist/mcpb-in \
      -p '*apple-darwin.tar.gz' -p '*linux-musl.tar.gz' \
      -p '*x86_64-pc-windows-msvc.zip' -p '*.sha256'
  python3 deploy/mcp-registry/build-mcpb.py --version 1.0.0-rc.97 --assets dist/mcpb-in
  gh release upload v1.0.0-rc.97 dist/xerj-mcp-1.0.0-rc.97.mcpb -R xerj-org/xerj
  mcp-publisher publish

Writes dist/xerj-mcp-<version>.mcpb and rewrites ./server.json (version,
package identifier, fileSha256). Standard library only.
"""
import argparse
import hashlib
import io
import json
import pathlib
import re
import sys
import tarfile
import zipfile

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent
FIXED = (2020, 1, 1, 0, 0, 0)
TARGETS = {
    "darwin-aarch64": ("aarch64-apple-darwin", "tar.gz", "xerj"),
    "darwin-x86_64": ("x86_64-apple-darwin", "tar.gz", "xerj"),
    "linux-aarch64": ("aarch64-unknown-linux-musl", "tar.gz", "xerj"),
    "linux-x86_64": ("x86_64-unknown-linux-musl", "tar.gz", "xerj"),
    "windows-x86_64": ("x86_64-pc-windows-msvc", "zip", "xerj.exe"),
}


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def check_sidecar(archive: pathlib.Path, data: bytes) -> None:
    side = archive.with_name(archive.name + ".sha256")
    if not side.exists():
        sys.exit(f"missing {side.name}: download the .sha256 files with the archives")
    want = side.read_text().split()[0].lower()
    if sha256(data) != want:
        sys.exit(f"{archive.name}: sha256 mismatch with {side.name}")


def extract_binary(archive: pathlib.Path, member: str) -> bytes:
    data = archive.read_bytes()
    check_sidecar(archive, data)
    stage = archive.name.rsplit(".tar.gz", 1)[0].rsplit(".zip", 1)[0]
    name = f"{stage}/{member}"
    if archive.name.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            return z.read(name)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as t:
        f = t.extractfile(name)
        if f is None:
            sys.exit(f"{archive.name}: {name} not found")
        return f.read()


def tools_from_schema() -> list:
    schema = json.loads((ROOT / "landing/docs/agents/schemas/mcp-tools.json").read_text())
    out = []
    for tool in schema["tools"]:
        first = re.split(r"(?<=\.)\s", tool["description"].strip(), maxsplit=1)[0]
        out.append({"name": tool["name"], "description": first})
    return out


def add(z: zipfile.ZipFile, name: str, data: bytes, mode: int) -> None:
    info = zipfile.ZipInfo(name, date_time=FIXED)
    info.external_attr = (0o100000 | mode) << 16
    info.create_system = 3
    info.compress_type = zipfile.ZIP_DEFLATED
    z.writestr(info, data, compresslevel=9)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True, help="release version without the leading v, e.g. 1.0.0-rc.97")
    ap.add_argument("--assets", required=True, type=pathlib.Path, help="folder holding the downloaded release archives")
    ap.add_argument("--out", type=pathlib.Path, default=ROOT / "dist")
    ap.add_argument("--server-json", type=pathlib.Path, default=ROOT / "server.json")
    ap.add_argument("--omit-tools", action="store_true",
                    help="leave the manifest tools list empty (Smithery's CLI rejects declared tools) and do not touch server.json")
    a = ap.parse_args()
    v = a.version.lstrip("v")

    manifest = json.loads((HERE / "manifest.template.json").read_text())
    manifest["version"] = v
    if a.omit_tools:
        manifest.pop("tools", None)
        manifest["tools_generated"] = True
    else:
        manifest["tools"] = tools_from_schema()

    files = {
        "manifest.json": (json.dumps(manifest, indent=2, ensure_ascii=False).encode() + b"\n", 0o644),
        "server/xerj-mcp.sh": ((HERE / "xerj-mcp.sh").read_bytes(), 0o755),
        "LICENSE": ((ROOT / "LICENSE").read_bytes(), 0o644),
    }
    for folder, (triple, ext, member) in TARGETS.items():
        archive = a.assets / f"xerj-{v}-{triple}.{ext}"
        if not archive.exists():
            sys.exit(f"missing {archive}")
        files[f"server/{folder}/{member}"] = (extract_binary(archive, member), 0o755)

    a.out.mkdir(parents=True, exist_ok=True)
    bundle = a.out / f"xerj-mcp-{v}.mcpb"
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name in sorted(files):
            data, mode = files[name]
            add(z, name, data, mode)
    bundle.write_bytes(buf.getvalue())
    digest = sha256(buf.getvalue())

    print(f"{bundle}  {bundle.stat().st_size} bytes")
    print(f"sha256 {digest}")
    if a.omit_tools:
        return

    sj = json.loads(a.server_json.read_text())
    sj["version"] = v
    pkg = next(p for p in sj["packages"] if p["registryType"] == "mcpb")
    pkg["identifier"] = f"https://github.com/xerj-org/xerj/releases/download/v{v}/{bundle.name}"
    pkg["fileSha256"] = digest
    pkg.pop("version", None)
    a.server_json.write_text(json.dumps(sj, indent=2, ensure_ascii=False) + "\n")
    print(f"updated {a.server_json}")


if __name__ == "__main__":
    main()
