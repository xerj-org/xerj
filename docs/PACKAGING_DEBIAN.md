# Debian packaging: the release asset, and the archive question

This answers [issue #810](https://github.com/xerj-org/xerj/issues/810). "Packaging
for Debian" is two different problems, and Christian Kastner (Debian Developer)
split them cleanly when we asked:

1. **A `.deb` shipped as a GitHub release asset** — "fairly simple". Done; see below.
2. **Inclusion in the official Debian archive** — requires that every build
   dependency is itself packaged in Debian and rebuildable there. Measured
   below, and on the strength of that measurement: **not claimed, and not being
   pursued**.

## The `.deb` release asset

From the first release cut after this document landed, every GitHub release
carries two extra assets alongside the tar.gz/zip set:

```
xerj-<version>-x86_64-unknown-linux-gnu.deb    (Architecture: amd64)
xerj-<version>-aarch64-unknown-linux-gnu.deb   (Architecture: arm64)
```

Debian control-file prereleases use `~`: Rust version `1.0.0-rc.76` becomes
`1.0.0~rc.76`, so the final `1.0.0` sorts newer for upgrades. Asset filenames,
the staged executable and its version banner keep the original Rust version.
A hyphen occurring only in `+build` metadata is not rewritten.

Install with `sudo dpkg -i xerj-<version>-x86_64-unknown-linux-gnu.deb`; the
binary lands in `/usr/bin/xerj` with docs under `/usr/share/doc/xerj`
(README, LICENSE, machine-readable `copyright`). There is no systemd unit and
no `postinst`: xerj is one self-contained binary with no default data
directory, so the package installs the binary and nothing pretends to know
where your data lives. Run it under whatever supervisor you already use.

Properties, all enforced at build time by `scripts/build-deb.sh`:

- **Same bits as the tar.gz.** The `.deb` is packed from the exact staged
  binary the tar.gz for the same target packs — no rebuild, no strip — so the
  two assets cannot diverge. (`cargo-deb` was rejected for exactly this
  reason: it rebuilds.)
- **Dependencies measured, not assumed.** TLS is rustls + ring in every
  release target, so the binary is expected to need nothing beyond glibc. The
  build script reads the ELF `NEEDED` entries, **fails the release build** if
  anything outside the glibc set appears (a `libssl.so.3` here would mean a
  feature regression, not a packaging tweak), and derives the `libc6` floor
  from the newest `GLIBC_*` symbol version the binary actually references.
  Consequence worth knowing before you install on anything old: the floor is
  the CI runner's glibc (at the time of writing the official linux-gnu
  binaries reference symbols up to `GLIBC_2.39`, measured by running
  `scripts/verify-release.sh` on a glibc-2.35 host and reading its failure),
  so the `.deb` will not install-and-run on Debian 12 (bookworm, glibc 2.36)
  or older — this is a property of the binaries we already ship, not of the
  `.deb` format.
- The musl targets stay tar.gz-only (a static-musl `.deb` is unusual and
  nobody asked); macOS and Windows are unaffected.

`scripts/verify-release.sh` checks the `.deb`s like every other asset from the
first tag that ships them: presence for both gnu targets, `.sha256` companion,
`dpkg-deb -x` extraction (`ar` + `tar` on hosts without `dpkg-deb`), binary +
LICENSE + README inside, and the same tag-vs-banner version-drift check that
was added after v1.0.0-rc.10.

## The Debian archive: measured, not claimed

Archive inclusion requires the whole dependency tree to be packaged in Debian.
We measured it rather than guess. Method: unique third-party crate names in
`engine/Cargo.lock`, mapped to Debian source packages (`serde_derive` →
`rust-serde-derive`) and looked up in Debian **unstable**'s `Sources` index.
Presence only — the packaged *versions* were not compared against what
`Cargo.lock` pins, so even the "present" set is a lower bound on the remaining
work.

Measured 2026-09-20, `Cargo.lock` at main (`9c64a7c6`), against
`dists/unstable/main/source/Sources.gz` of the same day:

| | |
|---|---|
| `[[package]]` entries in `Cargo.lock` | 737 |
| workspace `xerj-*` crates (built from this source) | 17 |
| third-party entries | 720 |
| **third-party crate names (unique)** | **654** |
| already packaged in Debian unstable | **506 (77.4%)** |
| **not in Debian unstable** | **148** |

The 720→654 difference is crates pinned at two versions in the tree (53 names).

What the 148 contains: the whole AWS SDK family the object-storage support
pulls in (17 `aws-*`/`aws-smithy-*` crates), `axum-core`/`axum-extra`/
`axum-macros` (Debian carries `rust-axum` itself), Windows/Android shims that
only exist for cross-platform builds, and a long tail of small crates. The
full list prints with:

```sh
scripts/debian-rust-coverage.sh --verbose
```

which re-runs the measurement against the live unstable index (or
`--index <saved Sources.gz>` offline). The join is a single `awk` pass on
purpose: an earlier shell-loop implementation gave run-to-run varying counts
on an idle machine, and a measurement that changes between runs is worse than
none.

**Conclusion.** 148 missing crates — each of which would need its own Debian
packaging and maintenance, several of which (notably `ring`) are famously
nontrivial — plus an unaudited version-sufficiency question on the 506 that
are present. That is a multi-year Debian Rust team effort, not an XERJ
release task. Debian archive inclusion is therefore **not claimed, not
implied, and not pursued**; if the number above changes materially (either
Debian catches up or our dependency tree shrinks), re-run the script and
update this page before saying anything new.
