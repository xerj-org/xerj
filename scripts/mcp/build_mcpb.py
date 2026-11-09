#!/usr/bin/env python3
"""Build an MCP Bundle (.mcpb) for `xerj mcp` from a published GitHub release.

The official MCP registry (registry.modelcontextprotocol.io), and the
catalogues that read from it (the VS Code / GitHub MCP gallery, PulseMCP),
need a downloadable package before `mcp-publisher publish` will list
XERJ. Smithery takes the same file. This script turns the release
archives we already ship into one cross-platform bundle:

  server/xerj        macOS universal binary (arm64 + x86_64 slices from
                     the two apple-darwin archives).
  server/linux/xerj  x86_64 musl build. The bundle spec picks the command
                     by OS only, so linux arm64 users should use the
                     release tarball directly.
  server/xerj.exe  x86_64-pc-windows-msvc build (Windows on Arm runs it
                   under emulation).

Each archive is checked against its `.sha256` asset before use.

Run manually; nothing here touches CI:

    python3 scripts/mcp/build_mcpb.py --tag v1.0.0-rc.100 --out /tmp/mcpb
    python3 scripts/mcp/build_mcpb.py --tag v1.0.0-rc.100 --out /tmp/mcpb --update-server-json

Then attach the bundle to the release and publish:

    gh release upload v1.0.0-rc.100 /tmp/mcpb/xerj-mcp-1.0.0-rc.100.mcpb
    mcp-publisher login github && mcp-publisher publish

`--update-server-json` rewrites the mcpb package entry in server.json
(identifier URL, version, fileSha256). The hash only matches the exact
file this run produced, so upload that file, not a rebuild.
"""
from __future__ import annotations

import argparse
import hashlib
import io
import json
import struct
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

REPO = "xerj-org/xerj"
ROOT = Path(__file__).resolve().parents[2]
TEMPLATE = ROOT / "packaging" / "mcpb" / "manifest.json"
SERVER_JSON = ROOT / "server.json"

TARGETS = {
    "darwin-arm64": "aarch64-apple-darwin.tar.gz",
    "darwin-x64": "x86_64-apple-darwin.tar.gz",
    "linux-x64": "x86_64-unknown-linux-musl.tar.gz",
    "win32-x64": "x86_64-pc-windows-msvc.zip",
}

# Mach-O constants for building a universal (fat) binary.
FAT_MAGIC = 0xCAFEBABE
MH_MAGIC_64 = 0xFEEDFACF
CPU_TYPE_X86_64 = 0x01000007
CPU_TYPE_ARM64 = 0x0100000C
FAT_ALIGN = 14  # 2**14 = 16 KiB, what lipo uses for arm64


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def download(tag: str, name: str, dest: Path) -> bytes:
    subprocess.run(
        ["gh", "release", "download", tag, "-R", REPO, "-p", name, "-p", name + ".sha256",
         "-D", str(dest), "--clobber"],
        check=True,
    )
    blob = (dest / name).read_bytes()
    want = (dest / (name + ".sha256")).read_text().split()[0].strip().lower()
    got = sha256(blob)
    if got != want:
        sys.exit(f"checksum mismatch for {name}: got {got}, release says {want}")
    return blob


def extract_binary(archive_name: str, blob: bytes) -> bytes:
    if archive_name.endswith(".zip"):
        with zipfile.ZipFile(io.BytesIO(blob)) as zf:
            for info in zf.infolist():
                if info.filename.endswith("/xerj.exe"):
                    return zf.read(info)
    else:
        with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as tf:
            for member in tf.getmembers():
                if member.isfile() and member.name.endswith("/xerj"):
                    return tf.extractfile(member).read()
    sys.exit(f"no xerj binary inside {archive_name}")


def macho_cpu(binary: bytes) -> tuple[int, int]:
    magic, cputype, cpusubtype = struct.unpack("<III", binary[:12])
    if magic != MH_MAGIC_64:
        sys.exit("expected a thin 64-bit Mach-O binary")
    return cputype, cpusubtype


def make_universal(slices: list[bytes]) -> bytes:
    """Write a fat Mach-O the same way `lipo -create` lays it out."""
    align = 1 << FAT_ALIGN
    header = struct.pack(">II", FAT_MAGIC, len(slices))
    offset = align  # first slice after the header page
    entries, body = [], bytearray()
    for data in sorted(slices, key=lambda b: macho_cpu(b)[0]):
        cputype, cpusubtype = macho_cpu(data)
        entries.append(struct.pack(">iiIII", cputype, cpusubtype, offset, len(data), FAT_ALIGN))
        pad = offset - align - len(body)
        body += b"\0" * pad + data
        offset = (offset + len(data) + align - 1) // align * align
    head = header + b"".join(entries)
    return head + b"\0" * (align - len(head)) + bytes(body)


def zip_add(zf: zipfile.ZipFile, name: str, data: bytes, mode: int) -> None:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = (0o100000 | mode) << 16
    info.create_system = 3  # unix, so the exec bit survives
    zf.writestr(info, data, compresslevel=9)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tag", required=True, help="release tag, e.g. v1.0.0-rc.100")
    ap.add_argument("--out", required=True, type=Path, help="output directory")
    ap.add_argument("--update-server-json", action="store_true",
                    help="point server.json's mcpb package at this bundle")
    args = ap.parse_args()

    version = args.tag.removeprefix("v")
    args.out.mkdir(parents=True, exist_ok=True)
    bins: dict[str, bytes] = {}
    with tempfile.TemporaryDirectory() as tmp:
        for key, suffix in TARGETS.items():
            name = f"xerj-{version}-{suffix}"
            bins[key] = extract_binary(name, download(args.tag, name, Path(tmp)))
            (Path(tmp) / name).unlink()

    universal = make_universal([bins["darwin-arm64"], bins["darwin-x64"]])
    manifest = TEMPLATE.read_text().replace("@VERSION@", version)
    json.loads(manifest)  # fail early on a broken template

    bundle = args.out / f"xerj-mcp-{version}.mcpb"
    with zipfile.ZipFile(bundle, "w") as zf:
        zip_add(zf, "manifest.json", manifest.encode(), 0o644)
        # One `server/xerj` path for both unix OSes is not possible with two
        # different binaries, so linux gets its own path via an override.
        zip_add(zf, "server/xerj", universal, 0o755)
        zip_add(zf, "server/linux/xerj", bins["linux-x64"], 0o755)
        zip_add(zf, "server/xerj.exe", bins["win32-x64"], 0o755)
        zip_add(zf, "LICENSE", (ROOT / "LICENSE").read_bytes(), 0o644)

    digest = sha256(bundle.read_bytes())
    url = f"https://github.com/{REPO}/releases/download/{args.tag}/{bundle.name}"
    print(f"bundle   {bundle} ({bundle.stat().st_size / 1e6:.1f} MB)")
    print(f"sha256   {digest}")
    print(f"url      {url}")

    if args.update_server_json:
        server = json.loads(SERVER_JSON.read_text())
        server["version"] = version
        for pkg in server.get("packages", []):
            if pkg.get("registryType") == "mcpb":
                pkg["identifier"] = url
                pkg["version"] = version
                pkg["fileSha256"] = digest
        SERVER_JSON.write_text(json.dumps(server, indent=2) + "\n")
        print(f"updated  {SERVER_JSON.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
