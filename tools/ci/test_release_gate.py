# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Shows that tools/ci/release_gate.py fails closed, and passes only on complete evidence.
Run: python3 -m unittest discover -s tools/ci -p 'test_*.py'"""
import copy
import sys
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import release_gate as rg  # noqa: E402

SHA = "a" * 40


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
        "signoff": {"spec-owner": {"by": "a", "pr": 1, "on": "2026-11-02"}, "maintainer": {"by": "b", "pr": 2, "on": "2026-11-02"}},
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

    # The online half, with a fake of the GitHub API.
    def api(self, runs, reviews=None, merged=True):
        def f(path):
            if "check-runs" in path:
                return {"check_runs": runs}
            if path.endswith("/reviews?per_page=100"):
                return reviews or []
            return {"merged": merged}
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

    def test_no_required_checks_fails(self):
        self.assertTrue(rg.online_checks({}, SHA, "o/r", self.api([])))

    def test_signoff_needs_a_merged_pr_and_an_approval_by_the_named_person(self):
        ev = complete_evidence()
        approved = [{"user": {"login": "a"}, "state": "APPROVED"}, {"user": {"login": "b"}, "state": "APPROVED"}]
        self.assertEqual(rg.online_signoffs(GATE, ev, "o/r", self.api([], approved)), [])
        self.assertEqual(len(rg.online_signoffs(GATE, ev, "o/r", self.api([], approved, merged=False))), 2)
        changes = approved + [{"user": {"login": "a"}, "state": "CHANGES_REQUESTED"}]
        self.assertEqual(len(rg.online_signoffs(GATE, ev, "o/r", self.api([], changes))), 1)

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
        approved = [{"user": {"login": "a"}, "state": "APPROVED"}, {"user": {"login": "b"}, "state": "APPROVED"}]
        with tempfile.TemporaryDirectory() as d:
            gate_file = Path(d) / "gate.toml"
            gate_file.write_text(gate_text)
            ev = Path(d) / "evidence.toml"
            ev.write_text(_dump(complete_evidence()))
            git = lambda *a: (0, "release/evidence.toml") if a[0] == "diff" else (0, "")  # noqa: E731
            with mock.patch.object(rg, "GATE_PATH", gate_file), mock.patch.object(rg, "mechanical_map", lambda: []):
                errors, _ = rg.run("", str(ev), True, "o/r", api=self.api(runs, approved), git_fn=git)
                self.assertEqual(errors, [])
                runs[0]["conclusion"] = "failure"
                errors, _ = rg.run("", str(ev), True, "o/r", api=self.api(runs, approved), git_fn=git)
                self.assertEqual(len(errors), 1)

    def test_no_release_commit_anywhere_fails(self):
        errors, _ = rg.run("", "/nonexistent/evidence.toml", online=False, repo="o/r")
        self.assertTrue(any("no release commit" in e for e in errors))

    def test_the_real_run_fails_closed_without_evidence(self):
        errors, _ = rg.run(SHA, "/nonexistent/evidence.toml", online=False, repo="o/r", git_fn=lambda *a: (1, ""))
        self.assertTrue(errors)
        self.assertTrue(any("not verified" in e for e in errors))


if __name__ == "__main__":
    unittest.main()
