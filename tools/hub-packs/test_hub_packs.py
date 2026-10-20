#!/usr/bin/env python3
"""Tests for hub_packs.py — the planning and re-pin logic behind
.github/workflows/hub-packs.yml.

Stdlib `unittest`, no network: the "upstream" repositories are local ones, spoken
to over `file://` (the one place the production code's https-only protocol
allowlist is widened, on purpose), and `gh` is a stub that records its calls.

    python3 tools/hub-packs/test_hub_packs.py
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import hub_packs as hp  # noqa: E402

SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40


def manifest_text(slug: str, repos: list[tuple[str, str, str]], extra: str = "") -> str:
    """A manifest in the registry's own hand-formatted style (one repo per line)."""
    body = ",\n".join(
        f'  {{"repo":"{n}","url":"{u}","licence":"MIT","sha":"{s}","files":3,"bytes":99,'
        f'"review":{{"spdx":"MIT","use":"adapt-with-attribution","by":"xerj-org","at":"2026-08-12"}}}}'
        for n, u, s in repos
    )
    return f'{{"corpus":"{slug}",{extra}"cloned_at":"2026-08-06T20:24:00Z","repos":[\n{body}\n]}}\n'


def write_registry(root: pathlib.Path, rows: list[dict], manifests: dict[str, str]) -> tuple[pathlib.Path, pathlib.Path]:
    reg = root / "tools" / "xerj-code" / "hub"
    (reg / "backlog").mkdir(parents=True, exist_ok=True)
    backlog = reg / "backlog" / "backlog-100.json"
    backlog.write_text(json.dumps({"corpora": rows}))
    for slug, text in manifests.items():
        (reg / f"{slug}.json").write_text(text)
    return reg, backlog


def row(slug: str, status="live", lane="A", refresh="pinned") -> dict:
    return {"slug": slug, "status": status, "lane": lane, "refresh": refresh, "cat": "DB"}


def git(*args: str, cwd: pathlib.Path, env=None) -> str:
    out = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=True,
        env={**os.environ, **(env or {})},
    )
    return out.stdout.strip()


GIT_ID = ["-c", "user.name=t", "-c", "user.email=t@example.test"]


def make_upstream(root: pathlib.Path, name: str, commits: int = 2) -> tuple[pathlib.Path, list[str]]:
    """A local repository with `commits` commits; returns (path, [shas oldest first])."""
    path = root / name
    path.mkdir(parents=True)
    git("init", "-q", "-b", "main", cwd=path)
    shas = []
    for i in range(commits):
        (path / "f.txt").write_text(f"v{i}\n")
        git("add", "f.txt", cwd=path)
        git(*GIT_ID, "commit", "-q", "-m", f"c{i}", cwd=path)
        shas.append(git("rev-parse", "HEAD", cwd=path))
    return path, shas


class RegistryTests(unittest.TestCase):
    def test_load_live_keeps_only_live_lane_a_corpora_that_have_a_manifest(self):
        with tempfile.TemporaryDirectory() as t:
            reg, backlog = write_registry(
                pathlib.Path(t),
                [
                    row("alpha"),
                    row("beta", status="candidate"),
                    row("gamma", lane="B"),
                    row("delta"),  # live, but its manifest is missing
                    row("eps", refresh="weekly"),
                ],
                {
                    "alpha": manifest_text("alpha", [("alpha", "https://example.test/a", SHA_A)]),
                    "beta": manifest_text("beta", [("beta", "https://example.test/b", SHA_B)]),
                    "gamma": manifest_text("gamma", [("gamma", "https://example.test/g", SHA_B)]),
                    "eps": manifest_text("eps", [("eps", "https://example.test/e", SHA_C)]),
                },
            )
            corpora, warnings = hp.load_live(reg, backlog)
            self.assertEqual([c.slug for c in corpora], ["alpha", "eps"])
            self.assertEqual([c.refresh for c in corpora], ["pinned", "weekly"])
            self.assertEqual(corpora[0].pin8, "aaaaaaaa")
            self.assertEqual(len(warnings), 1)
            self.assertIn("delta", warnings[0])

    def test_the_catalogue_pin_is_the_first_repos_sha(self):
        with tempfile.TemporaryDirectory() as t:
            reg, backlog = write_registry(
                pathlib.Path(t),
                [row("multi")],
                {"multi": manifest_text("multi", [("one", "https://x.test/1", SHA_A), ("two", "https://x.test/2", SHA_B)])},
            )
            (c,), _ = hp.load_live(reg, backlog)
            self.assertEqual(c.pin_sha, SHA_A)

    def test_a_malformed_live_manifest_is_a_warning_not_a_crash(self):
        with tempfile.TemporaryDirectory() as t:
            reg, backlog = write_registry(
                pathlib.Path(t),
                [row("good"), row("shortsha"), row("renamed")],
                {
                    "good": manifest_text("good", [("good", "https://x.test/g", SHA_A)]),
                    "shortsha": manifest_text("shortsha", [("r", "https://x.test/r", "abc123")]),
                    "renamed": manifest_text("other", [("r", "https://x.test/r", SHA_A)]),
                },
            )
            corpora, warnings = hp.load_live(reg, backlog)
            self.assertEqual([c.slug for c in corpora], ["good"])
            self.assertEqual(len(warnings), 2)


class PlanTests(unittest.TestCase):
    def corpora(self, t: str, slugs: list[tuple[str, str, str]]):
        rows = [row(s, refresh=r) for s, _, r in slugs]
        mans = {s: manifest_text(s, [(s, f"https://x.test/{s}", sha)]) for s, sha, _ in slugs}
        reg, backlog = write_registry(pathlib.Path(t), rows, mans)
        return hp.load_live(reg, backlog)[0]

    def test_a_pin_without_a_release_is_due_and_a_released_one_is_not(self):
        with tempfile.TemporaryDirectory() as t:
            cs = self.corpora(t, [("alpha", SHA_A, "pinned"), ("beta", SHA_B, "pinned")])
            plan = hp.plan(cs, ["pack-alpha-aaaaaaaa"])
            self.assertEqual([m["slug"] for m in plan["include"]], ["beta"])
            self.assertEqual(plan["include"][0]["tag"], "pack-beta-bbbbbbbb")
            self.assertEqual((plan["count"], plan["due"], plan["capped"]), (1, 1, False))

    def test_a_retagged_release_counts_as_released(self):
        # Immutable releases burn a tag; the publish step falls back to -rN.
        with tempfile.TemporaryDirectory() as t:
            cs = self.corpora(t, [("alpha", SHA_A, "pinned")])
            self.assertEqual(hp.plan(cs, ["pack-alpha-aaaaaaaa-r2"])["count"], 0)
            self.assertEqual(hp.plan(cs, ["pack-alpha-aaaaaaaa-rc"])["count"], 1, "-rc is not -rN")
            self.assertEqual(hp.plan(cs, ["pack-alpha-aaaaaaaa9"])["count"], 1, "a longer pin8 is another pin")

    def test_only_the_current_pin_matters(self):
        # Released for the OLD pin; the manifest moved on: due again.
        with tempfile.TemporaryDirectory() as t:
            cs = self.corpora(t, [("alpha", SHA_B, "pinned")])
            self.assertEqual(hp.plan(cs, ["pack-alpha-aaaaaaaa"])["count"], 1)

    def test_slugs_restrict_and_force_rebuilds_a_released_pin(self):
        with tempfile.TemporaryDirectory() as t:
            cs = self.corpora(t, [("alpha", SHA_A, "pinned"), ("beta", SHA_B, "pinned")])
            tags = ["pack-alpha-aaaaaaaa", "pack-beta-bbbbbbbb"]
            self.assertEqual(hp.plan(cs, tags)["count"], 0)
            forced = hp.plan(cs, tags, slugs={"alpha"}, force=True)
            self.assertEqual([m["slug"] for m in forced["include"]], ["alpha"])

    def test_the_cap_limits_a_run_and_says_so(self):
        with tempfile.TemporaryDirectory() as t:
            cs = self.corpora(t, [(f"c{i}", SHA_A, "pinned") for i in range(5)])
            plan = hp.plan(cs, [], max_builds=2)
            self.assertEqual((plan["count"], plan["due"], plan["capped"]), (2, 5, True))
            self.assertEqual([m["slug"] for m in plan["include"]], ["c0", "c1"])

    def test_cadence_follows_the_refresh_class(self):
        with tempfile.TemporaryDirectory() as t:
            cs = self.corpora(t, [("a", SHA_A, "daily"), ("b", SHA_A, "weekly"), ("c", SHA_A, "pinned")])
            got = {m["slug"]: m["cadence_days"] for m in hp.plan(cs, [])["include"]}
            self.assertEqual(got, {"a": 1, "b": 7, "c": 7})


class RewriteTests(unittest.TestCase):
    def test_only_the_sha_and_pin_date_change(self):
        old = manifest_text("quinn", [("quinn", "https://x.test/q", SHA_A)])
        new = hp.rewrite_manifest(old, [hp.Move("quinn", "https://x.test/q", SHA_A, SHA_B)], "2026-10-14")
        self.assertEqual(
            new,
            old.replace(SHA_A, SHA_B).replace('{"corpus":"quinn",', '{"corpus":"quinn","pin_date":"2026-10-14",'),
            "a textual one-line change, not a reformat",
        )
        a, b = json.loads(old), json.loads(new)
        self.assertEqual(b["pin_date"], "2026-10-14")
        self.assertEqual(b["repos"][0]["sha"], SHA_B)
        self.assertEqual(b["repos"][0]["review"], a["repos"][0]["review"], "the licence review is carried over")

    def test_an_existing_pin_date_is_replaced_not_duplicated(self):
        old = manifest_text("quinn", [("quinn", "https://x.test/q", SHA_A)], extra='"pin_date":"2026-01-01",')
        new = hp.rewrite_manifest(old, [hp.Move("quinn", "https://x.test/q", SHA_A, SHA_B)], "2026-10-14")
        self.assertEqual(new.count("pin_date"), 1)
        self.assertEqual(json.loads(new)["pin_date"], "2026-10-14")

    def test_two_repos_at_the_same_commit_move_independently(self):
        old = manifest_text("pair", [("one", "https://x.test/1", SHA_A), ("two", "https://x.test/2", SHA_A)])
        new = hp.rewrite_manifest(old, [hp.Move("two", "https://x.test/2", SHA_A, SHA_B)], "2026-10-14")
        shas = [r["sha"] for r in json.loads(new)["repos"]]
        self.assertEqual(shas, [SHA_A, SHA_B], "the repo that did not move keeps its pin")

    def test_both_repos_can_move_in_one_rewrite(self):
        old = manifest_text("pair", [("one", "https://x.test/1", SHA_A), ("two", "https://x.test/2", SHA_B)])
        new = hp.rewrite_manifest(
            old,
            [hp.Move("one", "https://x.test/1", SHA_A, SHA_C), hp.Move("two", "https://x.test/2", SHA_B, SHA_A)],
            "2026-10-14",
        )
        self.assertEqual([r["sha"] for r in json.loads(new)["repos"]], [SHA_C, SHA_A])

    def test_a_sha_that_is_not_there_is_refused(self):
        old = manifest_text("quinn", [("quinn", "https://x.test/q", SHA_A)])
        with self.assertRaises(hp.RegistryError):
            hp.rewrite_manifest(old, [hp.Move("quinn", "u", SHA_B, SHA_C)], "2026-10-14")
        with self.assertRaises(hp.RegistryError):
            hp.rewrite_manifest(old, [hp.Move("nope", "u", SHA_A, SHA_C)], "2026-10-14")

    def test_a_manifest_that_does_not_open_with_corpus_cannot_take_a_pin_date(self):
        old = '{"repos":[{"repo":"q","url":"u","sha":"%s"}],"corpus":"q"}' % SHA_A
        with self.assertRaises(hp.RegistryError):
            hp.rewrite_manifest(old, [hp.Move("q", "u", SHA_A, SHA_B)], "2026-10-14")

    def test_the_rewrite_check_catches_a_change_beyond_the_pins(self):
        old = manifest_text("quinn", [("quinn", "https://x.test/q", SHA_A)])
        sneaky = old.replace(SHA_A, SHA_B).replace('"files":3', '"files":4').replace(
            '{"corpus":"quinn",', '{"corpus":"quinn","pin_date":"2026-10-14",'
        )
        with self.assertRaises(hp.RegistryError):
            hp._assert_only_pins_changed(old, sneaky, [hp.Move("quinn", "u", SHA_A, SHA_B)], "2026-10-14")


class RepinTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def corpora(self, spec: list[tuple[str, str, str, str]]):
        """(slug, refresh, url, sha) -> loaded corpora."""
        rows = [row(s, refresh=r) for s, r, _, _ in spec]
        mans = {s: manifest_text(s, [(s, u, sha)]) for s, _, u, sha in spec}
        reg, backlog = write_registry(self.root, rows, mans)
        return hp.load_live(reg, backlog)[0]

    def test_pinned_corpora_are_never_proposed_unless_asked(self):
        cs = self.corpora([("rel", "pinned", "https://x.test/rel", SHA_A), ("feed", "weekly", "https://x.test/feed", SHA_A)])
        heads = {"https://x.test/rel": SHA_B, "https://x.test/feed": SHA_B}
        props, _ = hp.find_proposals(cs, hp.REPIN_CLASSES, heads.get, "2026-10-14")
        self.assertEqual([p.slug for p in props], ["feed"], "the pin is the promise for release-tag corpora")
        props, _ = hp.find_proposals(cs, ["pinned", "weekly"], heads.get, "2026-10-14")
        self.assertEqual([p.slug for p in props], ["feed", "rel"])

    def test_an_unmoved_head_proposes_nothing(self):
        cs = self.corpora([("feed", "weekly", "https://x.test/feed", SHA_A)])
        props, notes = hp.find_proposals(cs, hp.REPIN_CLASSES, lambda u: SHA_A, "2026-10-14")
        self.assertEqual((props, notes), ([], []))

    def test_an_unreachable_remote_is_a_note_and_never_a_proposal(self):
        cs = self.corpora([("feed", "weekly", "https://x.test/feed", SHA_A)])
        props, notes = hp.find_proposals(cs, hp.REPIN_CLASSES, lambda u: None, "2026-10-14")
        self.assertEqual(props, [])
        self.assertIn("cannot read HEAD", notes[0])

    def test_a_proposal_names_itself_by_what_it_would_pin(self):
        cs = self.corpora([("feed", "weekly", "https://github.com/o/r", SHA_A)])
        (p,), _ = hp.find_proposals(cs, hp.REPIN_CLASSES, lambda u: SHA_B, "2026-10-14")
        (q,), _ = hp.find_proposals(cs, hp.REPIN_CLASSES, lambda u: SHA_C, "2026-10-14")
        self.assertRegex(p.branch, r"^hub-repin/feed-[0-9a-f]{8}$")
        self.assertNotEqual(p.branch, q.branch, "a newer upstream head is a new proposal")
        self.assertEqual(p.title, "hub: re-pin feed to bbbbbbbb")
        self.assertIn("compare/aaaaaaaaaaaa...bbbbbbbbbbbb", p.body)
        self.assertIn("pack-feed-bbbbbbbb", p.body)
        self.assertIn("licence file **at the new sha**", p.body)

    def test_a_move_that_leaves_the_catalogue_pin_alone_says_so(self):
        reg, backlog = write_registry(
            self.root,
            [row("multi", refresh="weekly")],
            {"multi": manifest_text("multi", [("one", "https://x.test/1", SHA_A), ("two", "https://x.test/2", SHA_B)])},
        )
        cs, _ = hp.load_live(reg, backlog)
        heads = {"https://x.test/1": SHA_A, "https://x.test/2": SHA_C}
        (p,), _ = hp.find_proposals(cs, hp.REPIN_CLASSES, heads.get, "2026-10-14")
        self.assertEqual(p.title, "hub: re-pin multi: two to cccccccc")
        self.assertIn("does not change", p.body)
        self.assertEqual([r["sha"] for r in json.loads(p.new_text)["repos"]], [SHA_A, SHA_C])

    def test_ls_remote_reads_the_default_branch_head_of_a_real_repository(self):
        up, shas = make_upstream(self.root, "up")
        url = f"file://{up}"
        self.assertEqual(hp.git_head_of(url, "file:https"), shas[-1])
        cs = self.corpora([("feed", "weekly", url, shas[0])])
        (p,), notes = hp.find_proposals(cs, hp.REPIN_CLASSES, lambda u: hp.git_head_of(u, "file:https"), "2026-10-14")
        self.assertEqual(notes, [])
        self.assertEqual(p.moves, [hp.Move("feed", url, shas[0], shas[1])])
        # ...and once re-pinned there is nothing left to propose.
        cs2 = self.corpora([("feed", "weekly", url, shas[1])])
        self.assertEqual(hp.find_proposals(cs2, hp.REPIN_CLASSES, lambda u: hp.git_head_of(u, "file:https"), "2026-10-14")[0], [])

    def test_only_https_is_spoken_by_default(self):
        up, _ = make_upstream(self.root, "up")
        marker = self.root / "pwned"
        self.assertIsNone(hp.git_head_of(f"file://{up}"), "file:// needs the test-only widening")
        self.assertIsNone(hp.git_head_of(f"ext::sh -c touch% {marker}"), "ext:: would run a command")
        self.assertIsNone(hp.git_head_of("git://example.test/r"))
        self.assertIsNone(hp.git_head_of(f"--upload-pack=touch {marker}"))
        self.assertFalse(marker.exists())

    def test_the_protocol_allowlist_also_holds_inside_git_itself(self):
        # Defence in depth: even if the scheme gate above were bypassed, the
        # environment `git_head_of` hands git must refuse an `ext::` transport.
        marker = self.root / "pwned-by-ext"
        out = subprocess.run(
            ["git", "ls-remote", f"ext::sh -c touch% {marker}", "HEAD"],
            capture_output=True, text=True, timeout=30,
            env={**os.environ, "GIT_ALLOW_PROTOCOL": "https", "GIT_TERMINAL_PROMPT": "0"},
        )
        self.assertNotEqual(out.returncode, 0)
        self.assertFalse(marker.exists(), "GIT_ALLOW_PROTOCOL=https stops an ext:: URL from running anything")

    def test_a_dead_remote_is_none_not_an_exception(self):
        self.assertIsNone(hp.git_head_of(f"file://{self.root}/nowhere", "file:https"))


GH_STUB = textwrap.dedent(
    """\
    #!{python}
    # A stand-in for `gh`: records every call, keeps a tiny PR table in a JSON file.
    import json, sys
    STATE = {state!r}
    st = json.load(open(STATE))
    st["calls"].append(sys.argv[1:])
    a = sys.argv[1:]
    def save(): json.dump(st, open(STATE, "w"))
    if a[:2] == ["pr", "list"]:
        if "--head" in a:
            head = a[a.index("--head") + 1]
            print(json.dumps([p for p in st["prs"] if p["headRefName"] == head]))
        else:
            print(json.dumps([p for p in st["prs"] if p["state"] == "OPEN"]))
    elif a[:2] == ["pr", "create"]:
        head = a[a.index("--head") + 1]
        if head in st.get("fail_create", []):
            save(); print("create refused", file=sys.stderr); sys.exit(1)
        n = st["next"]; st["next"] += 1
        st["prs"].append({{"number": n, "state": "OPEN", "headRefName": head}})
        print(f"https://github.com/o/r/pull/{{n}}")
    elif a[:2] == ["pr", "close"]:
        for p in st["prs"]:
            if p["number"] == int(a[2]): p["state"] = "CLOSED"
    save()
    """
)


class ApplyTests(unittest.TestCase):
    """The real git against a local bare "origin"; only `gh` is a stub."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self._tmp.name)
        origin = self.root / "origin.git"
        git("init", "-q", "--bare", "-b", "corpus-hub", str(origin), cwd=self.root)
        self.work = self.root / "work"
        git("clone", "-q", str(origin), str(self.work), cwd=self.root)
        git("checkout", "-q", "-b", "corpus-hub", cwd=self.work)
        self.reg = self.work / "tools" / "xerj-code" / "hub"
        self.reg.mkdir(parents=True)
        self.slugs = ["feed", "other"]
        for s in self.slugs:
            (self.reg / f"{s}.json").write_text(manifest_text(s, [(s, f"https://github.com/o/{s}", SHA_A)]))
        git("add", "-A", cwd=self.work)
        git(*GIT_ID, "commit", "-q", "-m", "registry", cwd=self.work)
        git("push", "-q", "origin", "corpus-hub", cwd=self.work)
        self.origin = origin
        self.state = self.root / "gh-state.json"
        self.state.write_text(json.dumps({"calls": [], "prs": [], "next": 100}))
        self.gh = self.root / "gh"
        self.gh.write_text(GH_STUB.format(python=sys.executable, state=str(self.state)))
        self.gh.chmod(0o755)

    def tearDown(self):
        self._tmp.cleanup()

    def proposals(self, slugs=None, new=SHA_B):
        corpora, _ = hp.load_live(
            self.reg,
            self._backlog([row(s, refresh="weekly") for s in (slugs or self.slugs)]),
        )
        return hp.find_proposals(corpora, hp.REPIN_CLASSES, lambda u: new, "2026-10-14")[0]

    def _backlog(self, rows):
        path = self.reg / "backlog" / "backlog-100.json"
        path.parent.mkdir(exist_ok=True)
        path.write_text(json.dumps({"corpora": rows}))
        return path

    def apply(self, props, max_prs=25):
        return hp.apply_proposals(props, self.work, "tools/xerj-code/hub", "corpus-hub", max_prs, gh=str(self.gh))

    def gh_state(self):
        return json.loads(self.state.read_text())

    def test_it_opens_a_pr_whose_commit_changes_exactly_one_manifest(self):
        (p,) = self.proposals(["feed"])
        log, failures = self.apply([p])

        self.assertEqual(failures, 0)
        self.assertEqual(log, ["feed: opened https://github.com/o/r/pull/100"])
        tip = git("rev-parse", p.branch, cwd=self.origin)
        self.assertEqual(
            git("diff", "--name-only", "corpus-hub", p.branch, cwd=self.origin).splitlines(),
            ["tools/xerj-code/hub/feed.json"],
        )
        self.assertEqual(git("show", f"{p.branch}:tools/xerj-code/hub/feed.json", cwd=self.origin) + "\n", p.new_text)
        self.assertEqual(git("log", "-1", "--format=%an <%ae>", tip, cwd=self.origin), "xerj-org <xerj-org@users.noreply.github.com>")
        self.assertEqual(git("log", "-1", "--format=%s", tip, cwd=self.origin), "hub: re-pin feed to bbbbbbbb")
        # The checkout is left where the job found it: detached at the base.
        self.assertEqual(git("rev-parse", "HEAD", cwd=self.work), git("rev-parse", "origin/corpus-hub", cwd=self.work))
        create = [c for c in self.gh_state()["calls"] if c[:2] == ["pr", "create"]][0]
        self.assertEqual(create[create.index("--base") + 1], "corpus-hub")

    def test_a_second_run_with_the_same_upstream_opens_nothing(self):
        (p,) = self.proposals(["feed"])
        self.apply([p])
        before = len(self.gh_state()["calls"])

        log, failures = self.apply(self.proposals(["feed"]))

        self.assertEqual(failures, 0)
        self.assertIn("already proposed as PR #100", log[0])
        creates = [c for c in self.gh_state()["calls"][before:] if c[:2] == ["pr", "create"]]
        self.assertEqual(creates, [], "idempotent: no duplicate PR")

    def test_a_declined_proposal_is_not_reopened(self):
        (p,) = self.proposals(["feed"])
        self.apply([p])
        st = self.gh_state()
        st["prs"][0]["state"] = "CLOSED"  # a human closed it unmerged
        self.state.write_text(json.dumps(st))

        log, _ = self.apply(self.proposals(["feed"]))

        self.assertIn("already proposed", log[0])
        self.assertEqual([p for p in self.gh_state()["prs"] if p["state"] == "OPEN"], [])

    def test_the_cap_counts_new_prs_only(self):
        first, second = self.proposals()
        self.apply([first])  # `feed` is already proposed

        log, _ = self.apply([first, second], max_prs=1)

        self.assertIn("already proposed", log[0])
        self.assertIn("opened", log[1], "skipping an existing proposal does not spend the cap")
        log, _ = self.apply(self.proposals(new=SHA_C), max_prs=1)
        self.assertEqual(sum("opened" in l for l in log), 1)
        self.assertTrue(any("over the 1-PR cap" in l for l in log))

    def test_a_newer_proposal_supersedes_the_open_older_one(self):
        (old,) = self.proposals(["feed"], new=SHA_B)
        self.apply([old])
        (new,) = self.proposals(["feed"], new=SHA_C)

        log, _ = self.apply([new])

        self.assertNotEqual(old.branch, new.branch)
        self.assertTrue(any("closed superseded PR #100" in l for l in log), log)
        open_heads = [p["headRefName"] for p in self.gh_state()["prs"] if p["state"] == "OPEN"]
        self.assertEqual(open_heads, [new.branch])

    def test_one_failure_does_not_stop_the_rest(self):
        first, second = self.proposals()
        st = self.gh_state()
        st["fail_create"] = [first.branch]
        self.state.write_text(json.dumps(st))

        log, failures = self.apply([first, second])

        self.assertEqual(failures, 1)
        self.assertIn("FAILED", log[0])
        self.assertIn("opened", log[1], "the next corpus still gets its PR")
        self.assertEqual(git("rev-parse", "HEAD", cwd=self.work), git("rev-parse", "origin/corpus-hub", cwd=self.work))


class CatalogueTests(unittest.TestCase):
    REPO = "xerj-org/xerj"

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self._tmp.name)
        rows = [row("alpha"), row("beta")]
        mans = {
            "alpha": manifest_text("alpha", [("alpha", "https://x.test/a", SHA_A)]),
            "beta": manifest_text("beta", [("beta", "https://x.test/b", SHA_B)]),
        }
        self.reg, self.backlog = write_registry(self.root, rows, mans)
        self.corpora, _ = hp.load_live(self.reg, self.backlog)

    def tearDown(self):
        self._tmp.cleanup()

    def entry(self, slug: str, sha: str, retag: str = "", **over) -> dict:
        pin8 = sha[:8]
        url = (
            f"https://github.com/{self.REPO}/releases/download/pack-{slug}-{pin8}{retag}/"
            f"{slug}-{pin8}.hubpack.tar.zst"
        )
        e = {
            "slug": slug,
            "pin_sha": sha,
            "pack_url": url,
            "sig_url": url + ".sig",
            "pack_sha256": "ab" * 32,
            "pack_bytes": 12345,
        }
        e.update(over)
        return e

    def cat(self, *entries: dict, version: int = 1) -> bytes:
        return json.dumps({"format_version": version, "generated_at": "x", "corpora": list(entries)}).encode()

    def check(self, raw: bytes, previous=None) -> list[str]:
        return hp.check_catalogue(raw, self.corpora, self.REPO, previous)

    def test_fetch_list_names_the_newest_retag_of_each_current_pin(self):
        tags = ["pack-alpha-aaaaaaaa", "pack-alpha-aaaaaaaa-r2", "pack-alpha-aaaaaaaa-r10", "pack-beta-00000000"]
        # beta's release is for an OLD pin, so beta has no pack for its current one.
        self.assertEqual(hp.fetch_list(self.corpora, tags), [("alpha", "aaaaaaaa", "pack-alpha-aaaaaaaa-r10")])
        self.assertEqual(hp.fetch_list(self.corpora, ["pack-alpha-aaaaaaaa"])[0][2], "pack-alpha-aaaaaaaa")

    def test_a_consistent_catalogue_passes(self):
        raw = self.cat(self.entry("alpha", SHA_A), self.entry("beta", SHA_B, retag="-r2"))
        self.assertEqual(self.check(raw), [])

    def test_every_kind_of_inconsistency_is_reported(self):
        bad = self.cat(
            self.entry("alpha", SHA_C),  # not the manifest's pin
            self.entry("beta", SHA_B, pack_url="https://evil.test/beta.zst"),
            self.entry("ghost", SHA_A),  # not live
        )
        errors = self.check(bad)
        joined = "\n".join(errors)
        self.assertIn("alpha: pin_sha cccccccccccc is not the manifest's aaaaaaaa", joined)
        self.assertIn("beta: pack_url 'https://evil.test/beta.zst' is not the release asset of pin bbbbbbbb", joined)
        self.assertIn("beta: sig_url is not pack_url + '.sig'", joined)
        self.assertIn("ghost: not a live lane-A corpus", joined)

    def test_a_pack_url_from_another_repository_or_pin_is_refused(self):
        other_repo = self.entry("alpha", SHA_A).copy()
        other_repo["pack_url"] = other_repo["pack_url"].replace(self.REPO, "someone/else")
        other_repo["sig_url"] = other_repo["pack_url"] + ".sig"
        self.assertTrue(any("pack_url" in e for e in self.check(self.cat(other_repo))))
        wrong_pin = self.entry("alpha", SHA_A, pack_url=self.entry("alpha", SHA_B)["pack_url"])
        wrong_pin["sig_url"] = wrong_pin["pack_url"] + ".sig"
        self.assertTrue(any("pack_url" in e for e in self.check(self.cat(wrong_pin))))

    def test_hash_and_size_fields_must_be_plausible(self):
        errors = self.check(
            self.cat(self.entry("alpha", SHA_A, pack_sha256="xyz", pack_bytes=0), self.entry("beta", SHA_B, pack_bytes=True))
        )
        joined = "\n".join(errors)
        self.assertIn("alpha: pack_sha256 is not 64 hex", joined)
        self.assertIn("alpha: pack_bytes is not a positive integer", joined)
        self.assertIn("beta: pack_bytes is not a positive integer", joined, "a JSON true is not a size")

    def test_shape_failures_stop_early(self):
        self.assertIn("no corpora", self.check(self.cat())[-1])
        self.assertTrue(any("format_version" in e for e in self.check(self.cat(self.entry("alpha", SHA_A), version=2))))
        self.assertTrue(any("not valid JSON" in e for e in self.check(b"{nope")))
        big = self.cat(self.entry("alpha", SHA_A, pad="x" * hp.CATALOGUE_MAX_BYTES))
        self.assertTrue(any("budget" in e for e in self.check(big)))

    def test_a_corpus_listed_twice_is_refused(self):
        errors = self.check(self.cat(self.entry("alpha", SHA_A), self.entry("alpha", SHA_A)))
        self.assertIn("alpha: listed twice", errors)

    def test_delisting_a_corpus_that_is_still_live_is_refused(self):
        live = self.cat(self.entry("alpha", SHA_A), self.entry("beta", SHA_B))
        shrunk = self.cat(self.entry("alpha", SHA_A))
        errors = self.check(shrunk, previous=live)
        self.assertEqual(errors, ["beta: in the live catalogue and still live, but missing from the new one"])
        self.assertEqual(self.check(live, previous=live), [])

    def test_a_previous_catalogue_that_is_not_one_never_switches_the_guard_off(self):
        raw = self.cat(self.entry("alpha", SHA_A))
        for prev in (b"<!doctype html><html><body>hub</body></html>", b"", b"[]", b'{"corpora": "x"}',
                     b'{"corpora": [1]}', b"\xff\xfe"):
            errors = self.check(raw, previous=prev)
            self.assertTrue(any("not a catalogue" in e for e in errors), (prev, errors))

    def test_a_corpus_that_is_no_longer_live_may_leave_the_catalogue(self):
        previous = self.cat(self.entry("alpha", SHA_A), self.entry("retired", SHA_C))
        raw = self.cat(self.entry("alpha", SHA_A))
        self.assertEqual(self.check(raw, previous=previous), [], "`retired` is not live in the registry")

    def test_command_line_exit_codes(self):
        cat_path = self.root / "catalogue.json"
        live_path = self.root / "live.json"
        live_path.write_bytes(self.cat(self.entry("alpha", SHA_A), self.entry("beta", SHA_B)))

        def run(*extra: str):
            return subprocess.run(
                [sys.executable, hp.__file__, "check-catalogue", "--registry", str(self.reg), "--backlog",
                 str(self.backlog), "--catalogue", str(cat_path), "--repo", self.REPO, *extra],
                capture_output=True, text=True,
            )

        cat_path.write_bytes(self.cat(self.entry("alpha", SHA_A), self.entry("beta", SHA_B)))
        ok = run("--previous", str(live_path))
        self.assertEqual(ok.returncode, 0, ok.stderr)
        self.assertIn("2 corpora", ok.stderr)

        cat_path.write_bytes(self.cat(self.entry("alpha", SHA_A)))
        refused = run("--previous", str(live_path))
        self.assertEqual(refused.returncode, 1)
        self.assertIn("::error title=catalogue::beta", refused.stderr)
        self.assertIn("NOT fit to publish", refused.stderr)
        self.assertEqual(run("--previous", str(live_path), "--allow-drops").returncode, 0, "the explicit override")

        live_path.write_text("<!doctype html><html></html>")
        html = run("--previous", str(live_path), "--allow-drops")
        self.assertEqual(html.returncode, 1, "--allow-drops does not excuse an unreadable live catalogue")
        self.assertIn("not a catalogue", html.stderr)

    def test_fetch_list_command_prints_tab_separated_rows(self):
        tags = self.root / "tags.txt"
        tags.write_text("pack-alpha-aaaaaaaa-r2\n")
        out = subprocess.run(
            [sys.executable, hp.__file__, "fetch-list", "--registry", str(self.reg), "--backlog", str(self.backlog),
             "--published-tags", str(tags)],
            capture_output=True, text=True,
        )
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(out.stdout, "alpha\taaaaaaaa\tpack-alpha-aaaaaaaa-r2\n")
        self.assertIn("1 of 2 live corpora", out.stderr)


class ReportTests(unittest.TestCase):
    STAGES = ["start", "build", "verify", "tag", "upload", "e2e", "publish"]

    def steps(self, **outcomes) -> dict:
        return {sid: {"outcome": outcomes.get(sid, "success")} for sid in self.STAGES}

    def stages(self, logs: dict) -> list:
        return [(sid, [logs[sid]] if sid in logs else []) for sid in self.STAGES]

    def test_a_published_pack_records_bytes_defs_and_its_tag(self):
        with tempfile.TemporaryDirectory() as t:
            out = pathlib.Path(t) / "build.txt"
            out.write_text("cloning...\npack lz4 aaaaaaaa 1234 3 56\n")
            r = hp.result_row("lz4", "aaaaaaaa", self.steps(), self.stages({}), out,
                              "pack-lz4-aaaaaaaa", False, "77", "2026-10-16")
            self.assertEqual((r["result"], r["tag"], r["bytes"], r["defs"]),
                             ("published", "pack-lz4-aaaaaaaa", 1234, 56))

    def test_a_failed_stage_names_itself_and_its_most_telling_log_line(self):
        with tempfile.TemporaryDirectory() as t:
            log = pathlib.Path(t) / "e2e.log"
            log.write_text("pulling\n\x1b[31m::error::def LZ4_x not answered from the local pack\x1b[0m\ncleanup\n")
            r = hp.result_row("lz4", "aaaaaaaa", self.steps(e2e="failure", publish="skipped"),
                              self.stages({"e2e": log}), None, "", False, "77", "2026-10-16")
            self.assertEqual((r["result"], r["stage"]), ("failed", "e2e"))
            self.assertEqual(r["error"], "def LZ4_x not answered from the local pack")
            self.assertIsNone(r["bytes"])

    def test_the_first_failed_stage_wins_and_a_cancelled_one_is_not_a_success(self):
        r = hp.result_row("x", "aaaaaaaa", self.steps(build="cancelled", verify="skipped"),
                          self.stages({}), None, "", False, "1", "d")
        self.assertEqual((r["result"], r["stage"]), ("cancelled", "build"))

    def test_nothing_failed_but_nothing_published_is_not_reported_as_success(self):
        r = hp.result_row("x", "aaaaaaaa", self.steps(), self.stages({}), None, "", False, "1", "d")
        self.assertEqual(r["result"], "no result")
        self.assertTrue(r["error"])

    def test_a_job_that_died_before_its_first_stage_is_not_called_unexplained(self):
        steps = {sid: {"outcome": "skipped"} for sid in self.STAGES}
        r = hp.result_row("x", "aaaaaaaa", steps, self.stages({}), None, "", False, "1", "d", "failure")
        self.assertEqual((r["result"], r["stage"]), ("failed", "before start"))

    def test_the_error_line_skips_success_counts_and_redacts_secrets(self):
        with tempfile.TemporaryDirectory() as t:
            log = pathlib.Path(t) / "b.log"
            log.write_text("cannot clone https://ghp_" + "A" * 36 + "@github.com/x\nsummary: 0 errors\nexit 1\n")
            line = hp._log_tail_error([log])
            self.assertTrue(line.startswith("cannot clone"), line)
            self.assertNotIn("ghp_", line)
            self.assertIn("[redacted]", line)

    def test_the_report_shows_the_newest_runs_and_says_how_many_it_hides(self):
        hist = [{"run": str(i), "date": "d", "slug": "a", "pin8": "aaaaaaaa", "result": "published"}
                for i in range(hp.REPORT_RUNS_SHOWN + 2)]
        md = hp.render_report(hist)
        self.assertIn(f"The {hp.REPORT_RUNS_SHOWN} newest of {hp.REPORT_RUNS_SHOWN + 2} runs", md)
        self.assertNotIn("### Run 0,", md)
        self.assertIn(f"### Run {hp.REPORT_RUNS_SHOWN + 1},", md)

    def test_a_dry_run_that_passed_says_so(self):
        r = hp.result_row("x", "aaaaaaaa", self.steps(tag="skipped", upload="skipped", publish="skipped"),
                          self.stages({}), None, "", True, "1", "d")
        self.assertEqual(r["result"], "dry-run ok")

    def test_a_planned_corpus_without_a_row_still_gets_one(self):
        planned = [{"slug": "a", "pin8": "11111111"}, {"slug": "b", "pin8": "22222222"}]
        rows = [{"slug": "a", "result": "published"}]
        extra = hp.missing_rows(planned, rows, "9", "d", "build xerj: failure")
        self.assertEqual([(r["slug"], r["result"], r["error"]) for r in extra],
                         [("b", "no result", "build xerj: failure")])

    def test_the_report_keeps_a_failure_visible_until_a_later_run_publishes_it(self):
        def r(run, slug, result, error=""):
            return {"run": run, "date": "d", "slug": slug, "pin8": "aaaaaaaa", "bytes": None,
                    "defs": None, "result": result, "tag": "", "stage": "", "error": error}
        history = [r("1", "a", "failed", "boom | pipe"), r("1", "b", "published"),
                   r("2", "c", "failed", "later")]
        md = hp.render_report(history)
        self.assertIn("3 corpora attempted: 1 published at their latest attempt, 2 not.", md)
        self.assertIn("boom \\| pipe", md, "a pipe in an error must not break the table")
        self.assertLess(md.index("### Run 2"), md.index("### Run 1"), "newest run first")
        md = hp.render_report(history + [r("3", "a", "published")])
        self.assertIn("3 corpora attempted: 2 published at their latest attempt, 1 not.", md)
        self.assertIn("boom", md, "the old failing row stays in run 1's table")

    def test_report_cli_appends_this_run_and_replaces_a_rerun_of_the_same_id(self):
        with tempfile.TemporaryDirectory() as t:
            root = pathlib.Path(t)
            rows = root / "rows"; rows.mkdir()
            (rows / "a.json").write_text(json.dumps(
                {"slug": "a", "pin8": "11111111", "result": "published", "tag": "pack-a-11111111",
                 "bytes": 5, "defs": 2, "stage": "", "error": "", "date": "d", "run": "x"}))
            matrix = root / "plan.json"
            matrix.write_text(json.dumps({"include": [{"slug": "a", "pin8": "11111111"},
                                                      {"slug": "b", "pin8": "22222222"}]}))
            hist, rep = root / "out" / "rows.jsonl", root / "out" / "REPORT.md"
            args = ["report", "--rows-dir", str(rows), "--matrix", str(matrix), "--run-id", "5",
                    "--run-url", "https://x.test/runs/5", "--history", str(hist), "--report", str(rep),
                    "--missing-reason", "pack: cancelled", "--write"]
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(hp.main(args), 0)
                self.assertEqual(hp.main(args), 0)  # a re-run of the report job must not duplicate rows
            self.assertIn("1 of 2 corpora published in this run.", out.getvalue())
            got = hp.read_rows(hist)
            self.assertEqual([(r["slug"], r["result"], r["run"]) for r in got],
                             [("a", "published", "5"), ("b", "no result", "5")])
            md = rep.read_text()
            self.assertIn("### Run [5](https://x.test/runs/5), d", md.replace(hp._today(), "d"))
            self.assertIn("| `b` | `22222222` |  |  | no result | pack: cancelled |", md)


class RecordStepShellTests(unittest.TestCase):
    """The record-result step is a shell block in hub-packs.yml, not Python, so a
    quoting slip there is invisible to the rest of this suite. One such slip —
    `error: "...this job's log"` inside a single-quoted jq program — closed the
    quote early and broke the whole script at parse time, so EVERY pack job's
    `if: always()` record step died with `unexpected EOF` and no row was written.
    These tests parse and run the real block so that class of bug cannot return."""

    REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
    WORKFLOW = REPO_ROOT / ".github" / "workflows" / "hub-packs.yml"
    STEP_NAME = "Record this corpus's result for the backfill report"

    def record_step_run_block(self) -> str:
        lines = self.WORKFLOW.read_text().splitlines()
        start = next(i for i, ln in enumerate(lines)
                     if ln.strip() == f"- name: {self.STEP_NAME}")
        run_i = next(i for i in range(start, len(lines))
                     if lines[i].strip() == "run: |")
        run_indent = len(lines[run_i]) - len(lines[run_i].lstrip())
        body = []
        for ln in lines[run_i + 1:]:
            if ln.strip() and (len(ln) - len(ln.lstrip())) <= run_indent:
                break
            body.append(ln)
        block = textwrap.dedent("\n".join(body)) + "\n"
        self.assertIn("args=(result-row", block, "extracted the wrong block")
        return block

    def _env(self, tmp: str, **over) -> dict:
        e = dict(os.environ)
        e.update({"RUNNER_TEMP": tmp, "SLUG": "aioquic", "PIN8": "974e9349",
                  "GITHUB_RUN_ID": "42", "JOB_STATUS": "success", "DRY_RUN": "false",
                  "PUBLISHED_TAG": "pack-aioquic-974e9349-r3"})
        e.update(over)
        for sub in ("log", "node", "result"):
            pathlib.Path(tmp, sub).mkdir(parents=True, exist_ok=True)
        return e

    def test_the_record_step_shell_block_parses(self):
        block = self.record_step_run_block()
        out = subprocess.run(["bash", "-n", "/dev/stdin"], input=block,
                             text=True, capture_output=True)
        self.assertEqual(out.returncode, 0,
                         f"record step does not parse (a quoting bug is back):\n{out.stderr}")

    def test_the_record_step_writes_a_valid_row_for_a_success(self):
        block = self.record_step_run_block()
        # STEPS carries an apostrophe and a newline (as a real toJSON(steps) can).
        steps = json.dumps({"checkout": {"outcome": "success"},
                            "build": {"outcome": "success",
                                      "outputs": {"n": "this job's build\nline two"}},
                            "verify": {"outcome": "success"},
                            "publish": {"outcome": "success"}})
        with tempfile.TemporaryDirectory() as t:
            out = subprocess.run(["bash", "-euo", "pipefail", "/dev/stdin"], input=block,
                                 text=True, capture_output=True, cwd=self.REPO_ROOT,
                                 env=self._env(t, STEPS=steps))
            self.assertEqual(out.returncode, 0, out.stderr)
            row = json.loads((pathlib.Path(t) / "result" / "aioquic.json").read_text())
            self.assertEqual((row["result"], row["tag"]),
                             ("published", "pack-aioquic-974e9349-r3"))

    def test_the_record_step_fallback_still_writes_a_row_when_result_row_fails(self):
        block = self.record_step_run_block()
        with tempfile.TemporaryDirectory() as t:
            # Invalid STEPS JSON makes result-row exit non-zero; the fallback jq
            # row (the line the apostrophe used to break) must run and exit 0.
            out = subprocess.run(["bash", "-euo", "pipefail", "/dev/stdin"], input=block,
                                 text=True, capture_output=True, cwd=self.REPO_ROOT,
                                 env=self._env(t, STEPS="{not json", JOB_STATUS="failure",
                                               PUBLISHED_TAG=""))
            self.assertEqual(out.returncode, 0, out.stderr)
            row = json.loads((pathlib.Path(t) / "result" / "aioquic.json").read_text())
            self.assertEqual((row["result"], row["stage"]), ("failed", "record"))
            self.assertNotIn("'", row["error"], "no apostrophe in the fallback error string")


class CommandLineTests(unittest.TestCase):
    def run_cli(self, *argv: str) -> tuple[int, str, str]:
        out = subprocess.run(
            [sys.executable, str(pathlib.Path(hp.__file__)), *argv], capture_output=True, text=True
        )
        return out.returncode, out.stdout, out.stderr

    def test_plan_prints_the_matrix_json_the_workflow_feeds_to_fromjson(self):
        with tempfile.TemporaryDirectory() as t:
            reg, backlog = write_registry(
                pathlib.Path(t),
                [row("alpha"), row("beta")],
                {
                    "alpha": manifest_text("alpha", [("alpha", "https://x.test/a", SHA_A)]),
                    "beta": manifest_text("beta", [("beta", "https://x.test/b", SHA_B)]),
                },
            )
            tags = pathlib.Path(t) / "tags.txt"
            tags.write_text("pack-alpha-aaaaaaaa\nrust-v1.0.0\n")
            rc, out, err = self.run_cli(
                "plan", "--registry", str(reg), "--backlog", str(backlog), "--published-tags", str(tags)
            )
            self.assertEqual(rc, 0, err)
            plan = json.loads(out)
            self.assertEqual([m["slug"] for m in plan["include"]], ["beta"])
            self.assertEqual(plan["count"], 1)
            self.assertIn("plan: 1 due, 1 scheduled", err)

    def test_plan_with_nothing_due_still_emits_valid_json(self):
        with tempfile.TemporaryDirectory() as t:
            reg, backlog = write_registry(pathlib.Path(t), [], {})
            rc, out, _ = self.run_cli("plan", "--registry", str(reg), "--backlog", str(backlog))
            self.assertEqual(rc, 0)
            self.assertEqual(json.loads(out), {"include": [], "count": 0, "due": 0, "capped": False})

    def test_plan_warns_about_a_requested_slug_that_is_not_live(self):
        with tempfile.TemporaryDirectory() as t:
            reg, backlog = write_registry(
                pathlib.Path(t), [row("alpha")], {"alpha": manifest_text("alpha", [("alpha", "https://x.test/a", SHA_A)])}
            )
            rc, out, err = self.run_cli(
                "plan", "--registry", str(reg), "--backlog", str(backlog), "--slugs", "alpha,ghost"
            )
            self.assertEqual(rc, 0)
            self.assertIn("::warning::ghost", err)
            self.assertEqual(json.loads(out)["count"], 1)

    def test_repin_dry_run_lists_what_it_would_open_and_touches_nothing(self):
        with tempfile.TemporaryDirectory() as t:
            root = pathlib.Path(t)
            up, shas = make_upstream(root, "up")
            url = f"file://{up}"
            reg, backlog = write_registry(
                root, [row("feed", refresh="daily")], {"feed": manifest_text("feed", [("feed", url, shas[0])])}
            )
            before = (reg / "feed.json").read_text()
            rc, out, err = self.run_cli(
                "repin", "--registry", str(reg), "--backlog", str(backlog), "--classes", "daily",
                "--allow-file-urls", "--today", "2026-10-14",
            )
            self.assertEqual(rc, 0, err)
            self.assertIn(f"would open: hub: re-pin feed to {shas[1][:8]}", out)
            self.assertEqual((reg / "feed.json").read_text(), before, "dry run leaves the manifest alone")


class ConsumerE2eDefsDetectionTests(unittest.TestCase):
    """The per-pack consumer gate (tools/hub-packs/consumer_e2e.sh, §12.3) runs
    its `--def` assertion only when the pack carries a usable code symbol. A
    defs-0 pack (the awesome-* lists, docs-only, dataset and regulation corpora,
    e.g. awesome-mcp-servers at bytes:2.8MB defs:0) has nothing to look up, so it
    must fall back to the question check alone rather than failing outright — the
    bug that stalled the §12.1 backfill at ~134/514, since ~380 corpora are
    defs-0. These tests pin (a) that the script parses, (b) that the usable-symbol
    detection is correct across every defs.jsonl shape the builder can write, and
    (c) that the question-only fallback and its branches are wired, with no
    `fail` the moment defs is empty."""

    REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
    SCRIPT = REPO_ROOT / "tools" / "hub-packs" / "consumer_e2e.sh"

    @classmethod
    def setUpClass(cls):
        if not cls.SCRIPT.is_file():
            raise unittest.SkipTest(f"{cls.SCRIPT} not found")
        cls.text = cls.SCRIPT.read_text()

    def test_the_script_parses(self):
        out = subprocess.run(["bash", "-n", str(self.SCRIPT)], capture_output=True, text=True)
        self.assertEqual(out.returncode, 0, f"consumer_e2e.sh does not parse:\n{out.stderr}")

    def _def_program(self) -> str:
        """The exact jq program the script uses to pick the first usable symbol."""
        m = re.search(r"DEF=\$\(jq -cn '([^']*)'", self.text)
        self.assertIsNotNone(m, "could not find the DEF=jq picker in consumer_e2e.sh")
        return m.group(1)

    def _pick(self, jsonl: str) -> str:
        """Run the script's own DEF picker over a defs.jsonl body; '' means no usable symbol."""
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
            f.write(jsonl)
            path = f.name
        try:
            out = subprocess.run(["jq", "-cn", self._def_program(), path],
                                 capture_output=True, text=True)
            self.assertEqual(out.returncode, 0, out.stderr)
            return out.stdout.strip()
        finally:
            os.unlink(path)

    def test_an_empty_defs_jsonl_has_no_usable_symbol(self):
        # The defs-0 shape the builder writes for a docs/list/dataset corpus.
        self.assertEqual(self._pick(""), "")

    def test_blank_lines_only_have_no_usable_symbol(self):
        self.assertEqual(self._pick("\n   \n\n"), "")

    def test_symbols_shorter_than_four_chars_are_not_usable(self):
        # Records exist, but no `.symbol` is a 4+-char identifier -> question-only.
        self.assertEqual(self._pick('{"symbol":"ab","file":"x","line":1}\n'
                                    '{"symbol":"c","file":"y","line":2}\n'), "")

    def test_the_first_four_char_identifier_is_picked(self):
        got = self._pick('{"symbol":"ab","file":"a","line":1}\n'
                         '{"symbol":"parse_block","file":"src/lz4.c","line":42}\n')
        self.assertEqual(json.loads(got),
                         {"symbol": "parse_block", "file": "src/lz4.c", "line": 42})

    def test_non_identifier_symbols_are_skipped(self):
        # A punctuation/operator symbol never matches; the next real identifier wins.
        got = self._pick('{"symbol":"<=>","file":"a","line":1}\n'
                         '{"symbol":"LZ4_decompress","file":"src/lz4.c","line":99}\n')
        self.assertEqual(json.loads(got)["symbol"], "LZ4_decompress")

    def test_the_defs_zero_fallback_is_wired_and_the_old_hard_fail_is_gone(self):
        # The branch: usable symbol -> --def required; otherwise question-only.
        self.assertIn('if [ -n "$DEF" ]; then', self.text)
        self.assertIn("--def", self.text)
        self.assertIn("question-only check", self.text)
        # The pre-fix behaviour -- failing the whole pack the instant defs is
        # empty -- must not come back.
        self.assertNotIn("defs.jsonl has no identifier to look up", self.text,
                         "the defs-0 hard fail is back; defs-0 packs would stall the backfill again")

    def test_a_record_count_is_computed_for_logging(self):
        # Robust defs count: non-blank lines of defs.jsonl, read as the builder wrote it.
        self.assertRegex(self.text, r"DEFS_RECORDS=\$\(grep -c")


if __name__ == "__main__":
    unittest.main(verbosity=2)
