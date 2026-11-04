# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Shows that tools/ci/release_gate.py fails closed, and passes only on complete evidence.
Run: python3 -m unittest discover -s tools/ci -p 'test_*.py'"""
import copy
import json
import re
import sys
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import release_gate as rg  # noqa: E402

SHA = "a" * 40
MERGED = "c" * 40  # the merge commit of the sign-off pull request
HEADSHA = "d" * 40  # the pull request's final commit
FIXTURES = Path(__file__).parent / "fixtures/github"


def _dump(d, prefix=""):
    """A minimal TOML writer for the evidence shape (tables of scalars)."""
    out, tables = [], []
    for k, v in d.items():
        if isinstance(v, dict):
            tables.append((k, v))
        else:
            out.append(f"{k} = {v!r}".replace("'", '"') if isinstance(v, str) else f"{k} = {v}")
    text = "\n".join(out) + "\n"
    for k, v in tables:
        name = f"{prefix}{k}" if not prefix else f"{prefix}.{k}"
        key = ".".join(f'"{p}"' for p in name.split("."))
        text += f"\n[{key}]\n" + _dump(v, name)
    return text
GATE = tomllib.loads((rg.ROOT / "release/gate.toml").read_text())


def complete_gate():
    g = copy.deepcopy(GATE)
    g.setdefault("exit", {})["sustained_divergence_free_hours"] = 48
    return g


def complete_evidence():
    ok = {"status": "exercised", "network": "zombienet", "on": "2026-11-01", "evidence": "https://example.test/r", "by": "someone"}
    items = rg.named_conditions() + rg.incident_classes()
    return {
        "release_commit": SHA,
        "runbook": {i: dict(ok) for i in items},
        "exit": {"divergence": {"window_hours": 48, "divergences": 0, "evidence": "https://example.test/d", "by": "someone"},
                 "legs": {"unresolved": 0, "evidence": "https://example.test/l", "by": "someone"}},
        "soak": {"hours": 48, "statements_verified": 4, "divergences": 0, "evidence": "https://example.test/s", "by": "someone"},
        "signoff": {"spec-owner": {"by": "a", "pr": 1, "on": "2026-11-02"}, "maintainer": {"by": "b", "pr": 1, "on": "2026-11-02"}},
    }


def human(gate=None, ev=None):
    return rg.human(gate or complete_gate(), complete_evidence() if ev is None else ev, SHA,
                    rg.named_conditions(), rg.incident_classes())[0]


class Gate(unittest.TestCase):
    def test_spec_lists_are_not_empty(self):
        self.assertGreaterEqual(len(rg.named_conditions()), 10)
        self.assertEqual(len(rg.incident_classes()), 16)
        self.assertNotIn("CND-AwaitingInstruction", rg.named_conditions())  # withdrawn by amendment 0007

    def test_complete_evidence_passes(self):
        self.assertEqual(human(), [])

    def test_the_committed_gate_fails_until_the_window_is_ruled(self):
        self.assertTrue(any("sustained_divergence_free_hours" in e for e in human(gate=GATE)))

    def test_no_evidence_fails_with_every_item_named(self):
        errors = human(ev={})
        self.assertTrue(any("CND-Paused" in e for e in errors))
        self.assertTrue(any("I-16" in e for e in errors))
        self.assertGreater(len(errors), 30)

    def test_one_missing_runbook_fails(self):
        ev = complete_evidence()
        del ev["runbook"]["CND-CutOff"]
        errors = human(ev=ev)
        self.assertEqual(len(errors), 1)
        self.assertIn("CND-CutOff", errors[0])

    def test_a_runbook_not_on_a_test_network_fails(self):
        ev = complete_evidence()
        ev["runbook"]["I-3"]["network"] = "mainnet"
        self.assertTrue(any("I-3" in e and "network" in e for e in human(ev=ev)))

    def test_evidence_of_another_commit_fails(self):
        ev = complete_evidence()
        ev["release_commit"] = "b" * 40
        self.assertTrue(any("release_commit" in e for e in human(ev=ev)))

    def test_a_misspelt_runbook_is_not_ignored(self):
        ev = complete_evidence()
        ev["runbook"]["CND-Pausd"] = ev["runbook"]["CND-Paused"]
        self.assertTrue(any("CND-Pausd" in e for e in human(ev=ev)))

    def test_a_short_window_or_a_divergence_fails(self):
        ev = complete_evidence()
        ev["exit"]["divergence"]["window_hours"] = 47
        self.assertTrue(any("47 h" in e for e in human(ev=ev)))
        ev = complete_evidence()
        ev["exit"]["divergence"]["divergences"] = 1
        self.assertTrue(any("exactly 0" in e for e in human(ev=ev)))

    def test_an_unresolved_leg_fails(self):
        ev = complete_evidence()
        ev["exit"]["legs"]["unresolved"] = 2
        self.assertTrue(any("exit.legs" in e for e in human(ev=ev)))

    def test_a_short_or_unstated_soak_fails(self):
        ev = complete_evidence()
        ev["soak"]["hours"] = 47
        self.assertTrue(any("soak" in e and "47" in e for e in human(ev=ev)))
        ev = complete_evidence()
        ev["soak"]["statements_verified"] = 1
        self.assertTrue(any("statements" in e for e in human(ev=ev)))

    def test_a_missing_signoff_fails(self):
        ev = complete_evidence()
        del ev["signoff"]["maintainer"]
        self.assertTrue(any("signoff maintainer" in e for e in human(ev=ev)))

    def test_a_gate_file_without_signoffs_fails(self):
        g = complete_gate()
        g["signoff"]["required"] = []
        self.assertTrue(any("no required sign-off" in e for e in human(gate=g)))

    # The online half, with a fake of the GitHub API that pages like the real one (100 to a page).
    def api(self, runs, reviews=None, merged=True, files=("release/evidence.toml",), pr=None):
        def page(items, path):
            n = int(re.search(r"[?&]page=(\d+)", path).group(1))
            return items[(n - 1) * 100:n * 100]

        def f(path):
            if "/check-runs" in path:
                return {"check_runs": page(runs, path)}
            if "/reviews" in path:
                return page(reviews or [], path)
            if "/files" in path:
                return page([{"filename": x} for x in files], path)
            return {"merged": merged, "merge_commit_sha": MERGED, "head": {"sha": HEADSHA}, **(pr or {})}
        return f

    def test_a_missing_check_fails(self):
        errors = rg.online_checks(GATE, SHA, "o/r", self.api([]))
        self.assertEqual(len(errors), len(GATE["check"]))

    def test_a_skipped_check_fails_and_success_passes(self):
        runs = [{"name": c["name"], "conclusion": "success"} for c in GATE["check"]]
        self.assertEqual(rg.online_checks(GATE, SHA, "o/r", self.api(runs)), [])
        runs[-1]["conclusion"] = "skipped"  # the self-hosted zombienet job, when ZOMBIENET_RUNS_ON is unset
        errors = rg.online_checks(GATE, SHA, "o/r", self.api(runs))
        self.assertEqual(len(errors), 1)
        self.assertIn("skipped", errors[0])

    def test_a_failure_past_the_first_hundred_check_runs_is_seen(self):
        runs = [{"name": f"other {i}", "conclusion": "success"} for i in range(150)]
        runs += [{"name": c["name"], "conclusion": "success"} for c in GATE["check"]]
        runs.append({"name": GATE["check"][0]["name"], "conclusion": "failure"})  # on page 2 of 3
        errors = rg.online_checks(GATE, SHA, "o/r", self.api(runs))
        self.assertEqual(len(errors), 1)
        self.assertIn("failure", errors[0])

    def test_no_required_checks_fails(self):
        self.assertTrue(rg.online_checks({}, SHA, "o/r", self.api([])))

    def test_a_list_too_long_to_read_is_an_error_not_a_partial_read(self):
        with self.assertRaises(RuntimeError):
            rg.paged(lambda path: [{}] * 100, "repos/o/r/pulls/1/reviews")

    # Sign-off. `ROLES` is the reviewed list; both roles cite the one pull request that merged the evidence.
    ROLES = {"role": {"spec-owner": {"members": ["A", "x"]}, "maintainer": {"members": ["b", "y"]}}}
    APPROVED = [{"user": {"login": "a"}, "state": "APPROVED", "commit_id": HEADSHA, "submitted_at": "2026-11-02T10:00:00Z"},
                {"user": {"login": "b"}, "state": "APPROVED", "commit_id": HEADSHA, "submitted_at": "2026-11-02T11:00:00Z"}]

    def git(self, ancestors=("release", "head"), evidence_same=True):
        """A fake `git`: the release is an ancestor of the merge commit, which is an ancestor of HEAD."""
        def f(*a):
            if a[0] == "merge-base":
                _, _, x, y = a
                ok = (x == SHA and y == MERGED and "release" in ancestors) or (x == MERGED and y == "HEAD" and "head" in ancestors)
                return (0 if ok else 1), ""
            if a[0] == "rev-parse":
                return 0, ("blob1" if a[1].startswith(MERGED) or evidence_same else "blob2")
            return 1, ""
        return f

    def signoffs(self, ev=None, roles=None, reviews=None, git=None, **kw):
        return rg.online_signoffs(GATE, ev or complete_evidence(), "o/r",
                                  self.api([], self.APPROVED if reviews is None else reviews, **kw),
                                  self.ROLES if roles is None else roles, SHA, git or self.git())

    def test_a_proper_signoff_passes(self):
        self.assertEqual(self.signoffs(), [])  # `A` in the list, `a` approving: logins are case-insensitive

    def test_signoff_needs_a_merged_pr_and_an_approval_by_the_named_person(self):
        self.assertEqual(len(self.signoffs(merged=False)), 2)
        changes = self.APPROVED + [{"user": {"login": "a"}, "state": "CHANGES_REQUESTED", "commit_id": HEADSHA,
                                    "submitted_at": "2026-11-02T12:00:00Z"}]
        errors = self.signoffs(reviews=changes)
        self.assertEqual(len(errors), 1)
        self.assertIn("no standing approval", errors[0])

    def test_a_comment_does_not_withdraw_an_approval_and_a_dismissal_does(self):
        # The real repository's reviews are mostly COMMENTED (tools/ci/fixtures/github/reviews.json).
        comment = {"user": {"login": "a"}, "state": "COMMENTED", "commit_id": HEADSHA, "submitted_at": "2026-11-02T12:00:00Z"}
        self.assertEqual(self.signoffs(reviews=self.APPROVED + [comment]), [])
        dismissed = dict(comment, state="DISMISSED")
        self.assertEqual(len(self.signoffs(reviews=self.APPROVED + [dismissed])), 1)

    def test_a_dismissal_past_the_first_hundred_reviews_is_seen(self):
        noise = [{"user": {"login": f"u{i}"}, "state": "COMMENTED", "commit_id": HEADSHA} for i in range(120)]
        dismissed = {"user": {"login": "a"}, "state": "DISMISSED", "commit_id": HEADSHA, "submitted_at": "2026-11-03T00:00:00Z"}
        self.assertEqual(len(self.signoffs(reviews=self.APPROVED + noise + [dismissed])), 1)

    def test_an_approval_of_an_earlier_commit_does_not_stand(self):
        old = [dict(r, commit_id="e" * 40) for r in self.APPROVED]
        errors = self.signoffs(reviews=old)
        self.assertEqual(len(errors), 2)
        self.assertTrue(all("final commit" in e for e in errors))

    def test_a_review_by_a_deleted_user_is_nobodys(self):
        self.assertEqual(self.signoffs(reviews=self.APPROVED + [{"user": None, "state": "APPROVED", "commit_id": HEADSHA}]), [])

    def test_the_signer_must_be_in_the_roles_list_and_an_empty_list_fails_closed(self):
        ev = complete_evidence()
        ev["signoff"]["maintainer"]["by"] = "a"  # approved, and in the other role's list only
        self.assertTrue(any("not in the role's list" in e for e in self.signoffs(ev=ev)))
        errors = self.signoffs(roles={})
        self.assertEqual(len([e for e in errors if "lists no member" in e]), 2)

    def test_one_person_cannot_sign_both_roles(self):
        ev = complete_evidence()
        ev["signoff"]["maintainer"]["by"] = "A"
        roles = {"role": {"spec-owner": {"members": ["a"]}, "maintainer": {"members": ["a"]}}}
        errors = self.signoffs(ev=ev, roles=roles)
        self.assertEqual(len(errors), 1)
        self.assertIn("different person", errors[0])

    def test_an_old_pull_request_cannot_sign_a_new_release(self):
        # merged before the release commit (not a descendant of it): approved by the right people, still refused
        errors = self.signoffs(git=self.git(ancestors=("head",)))
        self.assertEqual(len(errors), 2)
        self.assertTrue(all("does not follow the release commit" in e for e in errors))
        errors = self.signoffs(git=self.git(ancestors=("release",)))
        self.assertTrue(all("history of the commit checked out" in e for e in errors) and errors)

    def test_a_pull_request_that_does_not_carry_the_evidence_cannot_sign_it(self):
        errors = self.signoffs(files=("README.md",))
        self.assertEqual(len(errors), 2)
        self.assertTrue(all("does not change release/evidence.toml" in e for e in errors))

    def test_evidence_changed_after_the_pull_request_merged_is_not_signed_off(self):
        errors = self.signoffs(git=self.git(evidence_same=False))
        self.assertEqual(len(errors), 2)
        self.assertTrue(all("not the file being gated" in e for e in errors))

    # The real answers of the GitHub API (tools/ci/fixtures/github, public data of this repository).
    def fixture(self, name):
        return json.loads((FIXTURES / name).read_text())

    def test_the_gate_reads_a_real_check_runs_answer(self):
        real = self.fixture("check-runs.json")
        self.assertEqual(real["total_count"], len(real["check_runs"]))
        want = {"rustfmt": "success", "Kreivo test bed": "skipped", "criteria map and release gate self-test": "success"}
        self.assertEqual({c["name"]: c["conclusion"] for c in real["check_runs"]}, want)
        gate = {"check": [{"name": n} for n in want]}
        errors = rg.online_checks(gate, SHA, "o/r", lambda path: real)
        self.assertEqual(len(errors), 1)  # the skipped one, and only it
        self.assertIn("Kreivo test bed", errors[0])

    def test_the_gate_reads_a_real_pull_request_and_reviews(self):
        pr, reviews = self.fixture("pull.json"), self.fixture("reviews.json")
        self.assertTrue(pr["merged"])
        self.assertRegex(pr["merge_commit_sha"], r"^[0-9a-f]{40}$")
        self.assertEqual({r["state"] for r in reviews}, {"COMMENTED"})
        self.assertEqual(rg.standing_approvers(reviews), {})  # comments approve nothing
        # The same reviews, one made an approval of the final commit, are a standing approval.
        approving = [dict(reviews[-1], state="APPROVED", commit_id=pr["head"]["sha"])]
        self.assertEqual(rg.standing_approvers(reviews + approving), {reviews[-1]["user"]["login"].lower(): pr["head"]["sha"]})
        files = [f["filename"] for f in self.fixture("pull-files.json")]
        self.assertTrue(files)
        self.assertEqual(rg.evidence_pr_errors("pr", pr, files, "0" * 40, lambda *a: (0, "x"))[0].split(": ")[1][:15], "does not change")

    def test_a_login_that_is_not_a_string_is_an_error_in_both_halves(self):
        # `by = 1` or `true` is truthy: it once passed human() and was skipped online, so no approval, roles-list,
        # merged-PR or two-person check ran at all.
        for bad in (1, True, ["a"], "", "  "):
            ev = complete_evidence()
            ev["signoff"]["maintainer"]["by"] = bad
            self.assertTrue(any("signoff maintainer" in e and "`by`" in e for e in human(ev=ev)), bad)
            errors = self.signoffs(ev=ev)
            self.assertTrue(any("signoff maintainer" in e and "cannot run" in e for e in errors), (bad, errors))
        ev = complete_evidence()
        ev["signoff"]["maintainer"]["pr"] = True
        self.assertTrue(any("cannot run" in e for e in self.signoffs(ev=ev)))

    def test_a_name_that_is_not_a_string_fails_in_every_item_that_names_who_did_it(self):
        for bad in (True, 1, ["x"], ""):
            for path in (("exit", "divergence"), ("exit", "legs"), ("soak",)):
                ev = complete_evidence()
                d = ev
                for k in path:
                    d = d[k]
                d["by"] = bad
                self.assertTrue(any(f"{'.'.join(path)}: `by`" in e for e in human(ev=ev)), (path, bad))
            ev = complete_evidence()
            ev["runbook"][rg.named_conditions()[0]]["by"] = bad
            self.assertTrue(any("`by`" in e for e in human(ev=ev)), bad)

    def test_an_unhashable_network_is_an_error_not_a_traceback(self):
        ev = complete_evidence()
        ev["runbook"][rg.named_conditions()[0]]["network"] = ["zombienet"]
        self.assertTrue(any("one of" in e for e in human(ev=ev)))

    def test_a_missing_or_mistyped_signoff_table_is_never_skipped_online(self):
        ev = complete_evidence()
        del ev["signoff"]["maintainer"]
        self.assertTrue(any("signoff maintainer" in e for e in self.signoffs(ev=ev)))
        ev["signoff"] = ["spec-owner"]
        self.assertEqual(len([e for e in self.signoffs(ev=ev) if "no sign-off table" in e]), 2)
        self.assertTrue(any("must be a table" in e for e in human(ev=ev)))

    def test_a_wrong_type_in_the_evidence_is_an_error_not_a_skipped_check(self):
        for path in (("runbook",), ("exit",), ("soak",), ("exit", "divergence"), ("exit", "legs"), ("signoff",),
                     ("signoff", "maintainer")):
            ev = complete_evidence()
            d = ev
            for k in path[:-1]:
                d = d[k]
            d[path[-1]] = "oops"
            errors = human(ev=ev)  # no traceback, and an error naming the table
            self.assertTrue(any("must be a table" in e for e in errors), (path, errors))
        g = complete_gate()
        g["signoff"]["required"] = "spec-owner"
        self.assertTrue(any("list of role names" in e for e in human(gate=g)))

    def test_a_roles_list_that_is_a_string_does_not_make_everyone_a_member(self):
        # members = "alice" iterates to the letters, so a login `a` would be in the list.
        roles = {"role": {"spec-owner": {"members": "alice"}, "maintainer": {"members": ["b"]}}}
        errors = self.signoffs(roles=roles)
        self.assertTrue(any("must be a list" in e for e in errors))
        self.assertTrue(any("not in the role's list" in e or "lists no member" in e for e in errors))

    def test_a_flag_or_a_float_is_not_a_zero_count(self):
        for bad in (False, 0.0, None, "0"):
            ev = complete_evidence()
            ev["exit"]["divergence"]["divergences"] = bad
            self.assertTrue(any("exactly 0" in e for e in human(ev=ev)), bad)
            ev = complete_evidence()
            ev["exit"]["legs"]["unresolved"] = bad
            self.assertTrue(any("exit.legs" in e for e in human(ev=ev)), bad)
            ev = complete_evidence()
            ev["soak"]["divergences"] = bad
            self.assertTrue(any("soak" in e for e in human(ev=ev)), bad)
        ev = complete_evidence()
        ev["exit"]["divergence"]["window_hours"] = True
        self.assertTrue(human(ev=ev))
        ev = complete_evidence()
        ev["signoff"]["maintainer"]["pr"] = True
        self.assertTrue(human(ev=ev))

    def test_the_evidence_commit_may_differ_from_the_release_only_by_the_evidence(self):
        def git(diff, ancestor=0):
            return lambda *a: (ancestor, "") if a[0] == "merge-base" else (0, diff)
        self.assertEqual(rg.evidence_follows_release(SHA, git("release/evidence.toml\n")), [])
        self.assertEqual(rg.evidence_follows_release(SHA, git("")), [])
        errors = rg.evidence_follows_release(SHA, git("release/evidence.toml\nservices/core/src/engine.rs\n"))
        self.assertTrue(errors and "engine.rs" in errors[0])
        self.assertTrue(rg.evidence_follows_release(SHA, git("", ancestor=1)))

    def test_a_complete_release_passes_end_to_end(self):
        # Everything satisfied, with the repository's own gate.toml plus the ruled window: the one path to a pass.
        import tempfile
        from unittest import mock
        gate_text = (rg.ROOT / "release/gate.toml").read_text().replace(
            "[exit]\n", "[exit]\nsustained_divergence_free_hours = 48\n")
        runs = [{"name": c["name"], "conclusion": "success"} for c in GATE["check"]]
        approved = self.APPROVED
        with tempfile.TemporaryDirectory() as d:
            gate_file = Path(d) / "gate.toml"
            gate_file.write_text(gate_text)
            roles_file = Path(d) / "roles.toml"
            roles_file.write_text('[role.spec-owner]\nmembers = ["a"]\n[role.maintainer]\nmembers = ["b"]\n')
            ev = Path(d) / "evidence.toml"
            ev.write_text(_dump(complete_evidence()))
            inner = self.git()
            git = lambda *a: (0, "release/evidence.toml") if a[0] == "diff" else (  # noqa: E731
                (0, "") if a[:2] == ("merge-base", "--is-ancestor") and a[2] == SHA and a[3] == "HEAD" else inner(*a))
            with mock.patch.object(rg, "GATE_PATH", gate_file), mock.patch.object(rg, "ROLES_PATH", roles_file), \
                    mock.patch.object(rg, "mechanical_map", lambda: []):
                errors, _ = rg.run("", str(ev), True, "o/r", api=self.api(runs, approved), git_fn=git)
                self.assertEqual(errors, [])
                runs[0]["conclusion"] = "failure"
                errors, _ = rg.run("", str(ev), True, "o/r", api=self.api(runs, approved), git_fn=git)
                self.assertEqual(len(errors), 1)

    def test_a_known_unmet_criterion_fails_the_gate_as_an_exception_does(self):
        table = [{"ac": "AC-X1.1", "status": "covered"},
                 {"ac": "AC-X1.2", "status": "known unmet", "known_unmet": {"missing": "the pause", "closed_by": "#77"}}]
        errors = rg.blockers(table)
        self.assertEqual(len(errors), 1)
        self.assertIn("AC-X1.2", errors[0])
        self.assertIn("#77", errors[0])
        self.assertEqual(rg.blockers(table[:1]), [])
        self.assertIn("AC-X1.3", rg.blockers([{"ac": "AC-X1.3", "status": "exception"}])[0])

    def test_a_config_error_is_a_gate_error_not_a_traceback(self):
        from unittest import mock

        def broken(root=None):
            raise rg.criteria_map.ConfigError("e2e/criteria/exceptions.toml: table(s) [known_unmet] not allowed here")
        with mock.patch.object(rg.criteria_map, "load_config", broken):
            errors = rg.mechanical_map()
        self.assertEqual(len(errors), 1)
        self.assertIn("[known_unmet] not allowed", errors[0])

    def test_the_committed_known_unmet_list_fails_the_real_gate_and_empties_to_pass(self):
        from unittest import mock
        listed = tomllib.loads((rg.ROOT / "e2e/criteria/known_unmet.toml").read_text())["known_unmet"]
        self.assertEqual(sorted(listed), ["AC-H3.1", "AC-I1.2", "AC-I3.1"])
        errors = rg.mechanical_map()
        self.assertTrue(any("known not met" in e and all(a in e for a in listed) for e in errors), errors)
        # Mutation: with every entry removed the same gate raises no such error, so the entries are what fail it.
        real_load = rg.criteria_map.load_config

        def emptied():
            cfg = real_load()
            cfg["known_unmet"] = {}
            return cfg
        with mock.patch.object(rg.criteria_map, "load_config", emptied):
            self.assertEqual(rg.mechanical_map(), [])

    def test_no_release_commit_anywhere_fails(self):
        errors, _ = rg.run("", "/nonexistent/evidence.toml", online=False, repo="o/r")
        self.assertTrue(any("no release commit" in e for e in errors))

    def test_the_real_run_fails_closed_without_evidence(self):
        errors, _ = rg.run(SHA, "/nonexistent/evidence.toml", online=False, repo="o/r", git_fn=lambda *a: (1, ""))
        self.assertTrue(errors)
        self.assertTrue(any("not verified" in e for e in errors))


if __name__ == "__main__":
    unittest.main()
