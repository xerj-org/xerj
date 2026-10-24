#!/usr/bin/env python3
"""Regression tests for the Co-authored-by CLA check (issue #269).

Before this check existed, cla-bot matched only commit *authors* against
.contributors, so a maintainer re-land of a fork PR — commits authored
xerj-org, contributor credited via a Co-authored-by trailer — went green
without the contributor ever signing (live instance: PR #248 re-landing
buger's #166). These tests pin the trailer-aware behaviour.

Run: python3 scripts/test_cla_coauthors.py
"""

import argparse
import base64
import contextlib
import io
import pathlib
import re
import sys
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import check_cla_coauthors
from check_cla_coauthors import (
    FIELD_REPORT_DIR,
    check_commits,
    is_contributors_only_change,
    is_field_report_only,
    is_self_cla_signature,
    noreply_login,
    parse_coauthors,
)

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

# The shape of PR #248: maintainer-authored re-land, real attribution
# carried only by the trailer.
RELAND_COMMITS = [
    {
        "sha": "c337eb8f",
        "message": "perf(autoindex): reuse the phase-A PDF extraction\n\n"
        "Body text.\n\nCo-authored-by: Leonid Bugaev <leonsbox@gmail.com>",
    },
    {
        "sha": "81c64883",
        "message": "docs(changelog): record the change\n\n"
        "Co-authored-by: Leonid Bugaev <leonsbox@gmail.com>",
    },
]


def no_network(email):
    raise AssertionError(f"unexpected network resolution for {email!r}")


class ParseTests(unittest.TestCase):
    def test_extracts_name_and_email(self):
        got = parse_coauthors(RELAND_COMMITS[0]["message"])
        self.assertEqual(got, [("Leonid Bugaev", "leonsbox@gmail.com")])

    def test_trailer_key_is_case_insensitive(self):
        # git trailers are case-insensitive; GitHub emits "Co-authored-by".
        msg = "subject\n\nCO-AUTHORED-BY: A B <a@b.c>\nco-authored-by: C D <c@d.e>"
        self.assertEqual(
            parse_coauthors(msg), [("A B", "a@b.c"), ("C D", "c@d.e")]
        )

    def test_no_trailers(self):
        self.assertEqual(parse_coauthors("fix: something\n\nplain body"), [])


class NoreplyTests(unittest.TestCase):
    def test_modern_noreply(self):
        self.assertEqual(
            noreply_login("1234567+buger@users.noreply.github.com"), "buger"
        )

    def test_legacy_noreply(self):
        self.assertEqual(noreply_login("buger@users.noreply.github.com"), "buger")

    def test_bot_noreply(self):
        self.assertEqual(
            noreply_login("49699333+dependabot[bot]@users.noreply.github.com"),
            "dependabot[bot]",
        )

    def test_ordinary_email_is_not_a_login(self):
        self.assertIsNone(noreply_login("leonsbox@gmail.com"))


class GateTests(unittest.TestCase):
    def test_unsigned_coauthor_fails_the_gate(self):
        # THE regression for issue #269: every commit author is signed, the
        # co-author is not — the old gate said green here.
        problems = check_commits(
            RELAND_COMMITS,
            contributors=["xerj-org", "dependabot[bot]"],
            resolve=lambda email: "buger",
        )
        self.assertTrue(problems, "gate passed a re-land with an unsigned co-author")
        self.assertTrue(any("buger" in p for p in problems), problems)

    def test_unresolvable_coauthor_fails_the_gate(self):
        # An identity the gate cannot attribute is exactly what it exists to
        # flag — unresolvable must fail, not silently pass.
        problems = check_commits(
            RELAND_COMMITS,
            contributors=["xerj-org", "dependabot[bot]"],
            resolve=lambda email: None,
        )
        self.assertTrue(problems, "gate passed an unresolvable co-author")

    def test_signed_coauthor_passes(self):
        problems = check_commits(
            RELAND_COMMITS,
            contributors=["xerj-org", "dependabot[bot]", "buger"],
            resolve=lambda email: "buger",
        )
        self.assertEqual(problems, [])

    def test_login_match_is_case_insensitive(self):
        # GitHub logins are case-insensitive; a case mismatch between the
        # resolved login and the .contributors entry must not flag a signer.
        problems = check_commits(
            RELAND_COMMITS,
            contributors=["xerj-org", "Buger"],
            resolve=lambda email: "buger",
        )
        self.assertEqual(problems, [])

    def test_noreply_coauthor_needs_no_network(self):
        commits = [
            {
                "sha": "abc12345",
                "message": "subject\n\n"
                "Co-authored-by: B <99+buger@users.noreply.github.com>",
            }
        ]
        problems = check_commits(
            commits, contributors=["buger"], resolve=no_network
        )
        self.assertEqual(problems, [])

    def test_no_trailers_needs_no_network(self):
        commits = [{"sha": "abc12345", "message": "fix: plain commit"}]
        self.assertEqual(
            check_commits(commits, contributors=["xerj-org"], resolve=no_network), []
        )

    def test_duplicate_emails_resolved_once(self):
        calls = []

        def counting(email):
            calls.append(email)
            return "buger"

        check_commits(RELAND_COMMITS, contributors=["buger"], resolve=counting)
        self.assertEqual(calls, ["leonsbox@gmail.com"])


class FieldReportExemptionTests(unittest.TestCase):
    """The narrow carve-out: a field-report-only diff is exempt; anything else
    is not. Bundling a code change with a field report must NOT bypass the gate
    — that would be a security hole, so the mixed case is asserted explicitly."""

    def _report(self, name):
        return f"{FIELD_REPORT_DIR}{name}"

    def test_single_field_report_is_exempt(self):
        self.assertTrue(
            is_field_report_only([self._report("2026-08-18-reference-coding.md")])
        )

    def test_several_field_reports_are_exempt(self):
        self.assertTrue(
            is_field_report_only(
                [
                    self._report("2026-08-18-a.md"),
                    self._report("2026-08-18-b.md"),
                ]
            )
        )

    def test_field_report_plus_code_is_NOT_exempt(self):
        # THE security case: a code change riding along with a field report must
        # get the full gate, never the exemption.
        self.assertFalse(
            is_field_report_only(
                [
                    self._report("2026-08-18-a.md"),
                    "engine/crates/xerj-server/src/main.rs",
                ]
            )
        )

    def test_field_report_plus_other_docs_is_NOT_exempt(self):
        self.assertFalse(
            is_field_report_only([self._report("2026-08-18-a.md"), "README.md"])
        )

    def test_the_folder_readme_is_NOT_a_field_report(self):
        self.assertFalse(is_field_report_only([self._report("README.md")]))

    def test_a_nested_path_is_NOT_a_field_report(self):
        self.assertFalse(is_field_report_only([self._report("sub/2026-08-18-a.md")]))

    def test_a_non_markdown_file_in_the_folder_is_NOT_exempt(self):
        self.assertFalse(is_field_report_only([self._report("2026-08-18-a.txt")]))

    def test_an_empty_diff_is_NOT_exempt(self):
        # "nothing changed" must never read as "exempt".
        self.assertFalse(is_field_report_only([]))
        self.assertFalse(is_field_report_only([""]))

    def test_a_lookalike_prefix_outside_the_folder_is_NOT_exempt(self):
        # A path that merely starts with the folder name but is not inside it.
        self.assertFalse(
            is_field_report_only(["user-feedback/16-agent-field-reports-notes.md"])
        )


CONTRIB = {"filename": ".contributors", "status": "modified"}


class SelfClaSignatureExitCodeTests(unittest.TestCase):
    """The one branch the workflow's `case` depends on: a `.contributors`-only
    PR whose head file does not parse exits 2 (loud), everything else 1."""

    BASE_SHA = "a" * 40
    HEAD_SHA = "b" * 40

    def _run(self, files, head_contributors, broken_base=False):
        base_contributors = None if broken_base else ["xerj-org", "dependabot[bot]"]
        pr = {
            "head": {"sha": self.HEAD_SHA},
            "state": "open",
            "user": {"login": "todie"},
        }
        cmp = {
            "total_commits": 1,
            "merge_base_commit": {"sha": self.BASE_SHA},
            "commits": [
                {
                    "sha": "c" * 40,
                    "commit": {"message": "chore: sign the CLA"},
                    "author": {"login": "todie"},
                }
            ],
            "files": files,
        }
        args = argparse.Namespace(
            pr=1, base_sha=self.BASE_SHA, head_sha=self.HEAD_SHA, repo="o/r"
        )
        with mock.patch.object(
            check_cla_coauthors, "_gh_api_one", side_effect=[pr, cmp]
        ), mock.patch.object(
            check_cla_coauthors,
            "_fetch_contributors_at",
            side_effect=lambda _repo, sha: (
                base_contributors if sha == self.BASE_SHA else head_contributors
            ),
        ), contextlib.redirect_stderr(io.StringIO()) as err:
            rc = check_cla_coauthors._self_cla_signature_main(args)
        return rc, err.getvalue()

    def test_broken_contributors_on_a_signature_pr_exits_2(self):
        rc, err = self._run([CONTRIB], None)
        self.assertEqual(rc, 2)
        self.assertIn("::error::", err)

    def test_broken_contributors_on_a_mixed_pr_exits_1(self):
        rc, err = self._run(
            [CONTRIB, {"filename": "x", "status": "modified"}], None
        )
        self.assertEqual(rc, 1)
        self.assertNotIn("::error::", err)

    def test_a_valid_signature_exits_0(self):
        rc, _ = self._run([CONTRIB], ["xerj-org", "dependabot[bot]", "todie"])
        self.assertEqual(rc, 0)

    def test_a_broken_base_file_is_not_this_pr_fault(self):
        # The merge base is `main`'s own file: a defect there is not something
        # the contributor's diff can repair, so the job must not fail on it.
        rc, err = self._run(
            [CONTRIB],
            ["xerj-org", "dependabot[bot]", "todie"],
            broken_base=True,
        )
        self.assertEqual(rc, 1)
        self.assertNotIn("::error::", err)


class ContributorsOnlyChangeTests(unittest.TestCase):
    """The CLI separates "a signature PR whose file is broken" (exit 2, loud)
    from "some other PR" (exit 1, cla-bot's call). That split is this shape
    test, so it has to be exact: one file, `.contributors`, modified."""

    def test_one_modified_contributors_is_the_signature_shape(self):
        self.assertTrue(is_contributors_only_change([CONTRIB]))

    def test_any_second_changed_file_breaks_the_shape(self):
        self.assertFalse(
            is_contributors_only_change(
                [CONTRIB, {"filename": "engine/src/lib.rs", "status": "modified"}]
            )
        )

    def test_another_status_is_not_the_signature_shape(self):
        for status in ("added", "removed", "renamed", "copied"):
            self.assertFalse(
                is_contributors_only_change(
                    [{"filename": ".contributors", "status": status}]
                )
            )

    def test_a_malformed_file_list_is_not_the_signature_shape(self):
        for files in (None, [], "x", [".contributors"], [None], {}):
            self.assertFalse(is_contributors_only_change(files))


class FetchContributorsTests(unittest.TestCase):
    """`_fetch_contributors_at` returning None is what the CLI turns into the
    loud exit 2 when `.contributors` is the whole diff."""

    def _fetch(self, raw):
        item = {
            "encoding": "base64",
            "content": base64.b64encode(raw).decode("ascii"),
        }
        with mock.patch.object(
            check_cla_coauthors, "_gh_api_one", return_value=item
        ):
            return check_cla_coauthors._fetch_contributors_at("o/r", "0" * 40)

    def test_invalid_json_returns_none(self):
        # The #1619 file once a merge resolution dropped the comma.
        self.assertIsNone(self._fetch(b'[\n  "a",\n  "b"\n  "c"\n]'))

    def test_a_json_value_that_is_not_a_list_still_parses(self):
        # Shape is the predicate's business, not the fetcher's: a dict parses
        # and is rejected later, which is exit 1, not exit 2.
        self.assertEqual(self._fetch(b'{"a": 1}'), {"a": 1})

    def test_a_valid_list_round_trips(self):
        self.assertEqual(self._fetch(b'["a", "b"]'), ["a", "b"])



class SelfClaSignatureTests(unittest.TestCase):
    """Self-signing .contributors PRs (e.g. #1375, #1366, #1355, #1342) deadlock
    on cla-bot because cla-bot reads .contributors from `main`. The narrow
    self-signature predicate satisfies `verification/cla-signed` only when a PR
    exclusively adds its own author to .contributors."""

    BASE = ["xerj-org", "dependabot[bot]", "buger"]
    COMMITS = [
        {
            "sha": "9bd098c0",
            "message": "chore: add todie to .contributors (CLA signature)",
            "author_login": "todie",
        }
    ]

    def test_valid_self_signature_passes(self):
        self.assertTrue(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["todie"],
                pr_author="todie",
                commits=self.COMMITS,
                resolve=no_network,
            )
        )

    def test_case_insensitive_author_match_passes(self):
        self.assertTrue(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["todie"],
                pr_author="Todie",
                commits=self.COMMITS,
                resolve=no_network,
            )
        )

    def test_touching_any_other_file_is_rejected(self):
        self.assertFalse(
            is_self_cla_signature(
                files=[CONTRIB, {"filename": "engine/src/lib.rs", "status": "modified"}],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["todie"],
                pr_author="todie",
                commits=self.COMMITS,
                resolve=no_network,
            )
        )

    def test_adding_another_user_is_rejected(self):
        self.assertFalse(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["someone-else"],
                pr_author="todie",
                commits=self.COMMITS,
                resolve=no_network,
            )
        )

    def test_adding_self_plus_another_user_is_rejected(self):
        self.assertFalse(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["todie", "someone-else"],
                pr_author="todie",
                commits=self.COMMITS,
                resolve=no_network,
            )
        )

    def test_removing_existing_contributor_is_rejected(self):
        self.assertFalse(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=["xerj-org", "dependabot[bot]", "todie"],
                pr_author="todie",
                commits=self.COMMITS,
                resolve=no_network,
            )
        )

    def test_commit_by_different_author_is_rejected(self):
        commits = [
            {
                "sha": "9bd098c0",
                "message": "chore: add todie to .contributors",
                "author_login": "other-user",
            }
        ]
        self.assertFalse(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["todie"],
                pr_author="todie",
                commits=commits,
                resolve=no_network,
            )
        )

    def test_unsigned_coauthor_trailer_is_rejected(self):
        commits = [
            {
                "sha": "9bd098c0",
                "message": "chore: add todie to .contributors\n\n"
                "Co-authored-by: Unsigned <99+unsigned@users.noreply.github.com>",
                "author_login": "todie",
            }
        ]
        self.assertFalse(
            is_self_cla_signature(
                files=[CONTRIB],
                base_contributors=self.BASE,
                head_contributors=self.BASE + ["todie"],
                pr_author="todie",
                commits=commits,
                resolve=no_network,
            )
        )


    def _ok(self, **kw):
        args = dict(
            files=[CONTRIB],
            base_contributors=self.BASE,
            head_contributors=self.BASE + ["todie"],
            pr_author="todie",
            commits=self.COMMITS,
            resolve=no_network,
        )
        args.update(kw)
        return is_self_cla_signature(**args)

    def test_baseline_helper_passes(self):
        self.assertTrue(self._ok())

    def test_rename_onto_contributors_is_rejected(self):
        # A rename lists only the NEW path; the deleted source would be hidden.
        renamed = dict(CONTRIB, status="renamed", previous_filename="engine/x.rs")
        self.assertFalse(self._ok(files=[renamed]))
        sneaky = dict(CONTRIB, previous_filename="engine/x.rs")
        self.assertFalse(self._ok(files=[sneaky]))

    def test_added_or_removed_contributors_file_is_rejected(self):
        self.assertFalse(self._ok(files=[dict(CONTRIB, status="added")]))
        self.assertFalse(self._ok(files=[dict(CONTRIB, status="removed")]))
        self.assertFalse(self._ok(files=[]))

    def test_altering_an_existing_entry_is_rejected(self):
        # Same count, same author added, but an existing entry rewritten.
        head = ["xerj-org", "dependabot[bot]", "Buger", "todie"]
        self.assertFalse(self._ok(head_contributors=head))

    def test_non_ascii_lookalike_login_is_rejected(self):
        # KELVIN SIGN U+212A lowercases to ASCII "k" under str.lower().
        self.assertFalse(
            self._ok(head_contributors=self.BASE + ["\u212aate"], pr_author="kate",
                     commits=[dict(self.COMMITS[0], author_login="kate")])
        )

    def test_whitespace_padded_entry_is_rejected(self):
        self.assertFalse(self._ok(head_contributors=self.BASE + [" todie "]))
        self.assertFalse(self._ok(head_contributors=self.BASE + ["todie\n"]))

    def test_already_signed_author_is_rejected(self):
        base = self.BASE + ["Todie"]
        self.assertFalse(self._ok(base_contributors=base, head_contributors=base + ["todie"]))

    def test_malformed_contributors_is_rejected(self):
        self.assertFalse(self._ok(head_contributors={"todie": True}))
        self.assertFalse(self._ok(head_contributors=self.BASE + ["todie", 1]))
        self.assertFalse(self._ok(head_contributors=None))
        self.assertFalse(self._ok(head_contributors=["xerj-org", "buger", "todie"]))

    def test_commit_with_no_linked_github_user_is_rejected(self):
        self.assertFalse(self._ok(commits=[dict(self.COMMITS[0], author_login=None)]))
        self.assertFalse(self._ok(commits=[]))

    def test_one_foreign_commit_among_own_commits_is_rejected(self):
        commits = self.COMMITS + [dict(self.COMMITS[0], sha="deadbeef", author_login="mallory")]
        self.assertFalse(self._ok(commits=commits))

    def test_too_many_commits_is_rejected(self):
        self.assertFalse(self._ok(commits=self.COMMITS * 21))

    def test_invalid_pr_author_is_rejected(self):
        self.assertFalse(self._ok(pr_author=""))
        self.assertFalse(self._ok(pr_author="todie[bot]",
                                  head_contributors=self.BASE + ["todie[bot]"]))


class WiringTests(unittest.TestCase):
    """A checker CI never runs is not a gate (cf. the #207 lesson)."""

    def _ci(self):
        return (REPO_ROOT / ".github" / "workflows" / "ci.yml").read_text()

    def _cla_job(self):
        ci = self._ci()
        m = re.search(r"\n  cla-config:\n(.*?)(?=\n  \S)", ci, re.S)
        self.assertIsNotNone(m, "cla-config job missing from ci.yml")
        return m.group(1)

    def test_ci_runs_the_coauthor_check(self):
        self.assertIn("check_cla_coauthors.py", self._cla_job())

    def test_ci_runs_these_tests(self):
        self.assertIn("test_cla_coauthors.py", self._cla_job())

    def test_cla_config_computes_changed_files_for_the_carveout(self):
        # The carve-out only works if the job actually passes the PR's changed
        # files to the checker — otherwise every PR runs the full gate.
        self.assertIn("--changed-files", self._cla_job())

    def test_a_separate_job_provides_the_field_report_status(self):
        # cla-bot cannot path-scope itself, so the required `verification/cla-signed`
        # status for a strictly field-report-only PR (or a self-CLA signature PR)
        # is provided by a dedicated workflow, guarded by the same predicates.
        wf = REPO_ROOT / ".github" / "workflows" / "cla-field-report-exempt.yml"
        self.assertTrue(wf.exists(), "field-report exemption workflow missing")
        text = wf.read_text()
        self.assertIn("verification/cla-signed", text)
        self.assertIn("--is-field-report-only", text)
        self.assertIn("--is-self-cla-signature", text)
        # Both verdicts must be judged on the exact SHA the status is posted to.
        self.assertIn("compare/${BASE_SHA}...${HEAD_SHA}", text)
        self.assertIn('--head-sha "$HEAD_SHA"', text)
        # A rename must expose its source path to the path predicates.
        self.assertIn("previous_filename", text)
        self.assertIn("persist-credentials: false", text)
        self.assertNotIn("head.ref", text)
        # It must never run the PR's own code under the elevated token.
        self.assertIn("pull_request_target", text)
        self.assertIn("base.sha", text)


if __name__ == "__main__":
    unittest.main()
