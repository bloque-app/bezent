# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Shows that tools/ci/criteria_map.py fails closed: each way the map can go wrong turns into an error.
Run: python3 -m unittest discover -s tools/ci -p 'test_*.py'"""
import copy
import sys
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import criteria_map as cm  # noqa: E402


def real():
    crit, _, stories = cm.spec_criteria()
    layers = tomllib.loads((cm.ROOT / "e2e/criteria/layers.toml").read_text())["layer"]
    cfg = tomllib.loads((cm.ROOT / "e2e/criteria/exceptions.toml").read_text())
    return dict(crit=crit, stories=stories, v0=cm.v0_stories(), inv_layers=cm.invariant_layers(),
                layers=layers, cfg=cfg, found=cm.scan())


def run(**over):
    args = copy.deepcopy(real())
    args.update(over)
    return cm.check(args["crit"], args["stories"], args["v0"], args["inv_layers"], args["layers"], args["cfg"],
                    args["found"])[0]


class CriteriaMap(unittest.TestCase):
    def test_the_repository_passes(self):
        self.assertEqual(run(), [])

    def test_a_criterion_with_no_test_and_no_exception_fails(self):
        found = real()["found"]
        del found["AC-G1.2"]
        self.assertTrue(any("AC-G1.2" in e and "no test" in e for e in run(found=found)))

    def test_a_test_at_the_wrong_layer_does_not_count(self):
        found = real()["found"]
        found["AC-G1.2"] = [dict(h, layer="chain") for h in found["AC-G1.2"]]
        errors = run(found=found)
        self.assertTrue(any("AC-G1.2" in e and "chain" in e for e in errors), errors)

    def test_one_missing_layer_of_an_invariant_fails(self):
        # AC-B1.3 cites INV-8: store + service + chain. Without its store test it fails, though two layers hold.
        found = real()["found"]
        found["AC-B1.3"] = [h for h in found["AC-B1.3"] if h["layer"] != "store"]
        errors = run(found=found)
        self.assertTrue(any("AC-B1.3" in e and "store" in e for e in errors), errors)

    def test_a_key_bound_that_rests_on_one_component_only_fails(self):
        # AC-I1.1 needs a test under the EVM vault and one under the Kreivo contract (INV-3): the second is the
        # highest value at risk and a green map on the first alone hid it.
        found = real()["found"]
        self.assertTrue(any(h["file"].startswith("contracts/bezent/") for h in found["AC-I1.1"]))
        found["AC-I1.1"] = [h for h in found["AC-I1.1"] if not h["file"].startswith("contracts/bezent/")]
        errors = run(found=found)
        self.assertTrue(any("AC-I1.1" in e and "contracts/bezent/" in e for e in errors), errors)
        found["AC-I1.1"] = [h for h in real()["found"]["AC-I1.1"] if not h["file"].startswith("contracts/evm/")]
        self.assertTrue(any("AC-I1.1" in e and "contracts/evm/" in e for e in run(found=found)))

    def test_a_stale_exception_fails(self):
        cfg = real()["cfg"]
        cfg["exception"]["AC-G1.2"] = {"reason": "x", "blocked_by": "#1"}
        self.assertTrue(any("AC-G1.2" in e and "remove" in e for e in run(cfg=cfg)))

    def test_an_exception_needs_a_reason_and_a_blocker(self):
        cfg = real()["cfg"]
        cfg["exception"]["AC-H3.1"] = {"reason": "x"}
        self.assertTrue(any("AC-H3.1" in e and "blocked_by" in e for e in run(cfg=cfg)))

    def test_an_exception_names_a_live_criterion(self):
        cfg = real()["cfg"]
        cfg["exception"]["AC-A2.4"] = {"reason": "x", "blocked_by": "#1"}  # withdrawn by amendment 0007
        self.assertTrue(any("AC-A2.4" in e for e in run(cfg=cfg)))

    def test_a_new_criterion_needs_a_layer(self):
        layers = real()["layers"]
        del layers["AC-G1.2"]
        self.assertTrue(any("AC-G1.2" in e and "layer entry" in e for e in run(layers=layers)))

    def test_a_weaker_layer_than_section_9_needs_a_reason(self):
        layers = real()["layers"]
        layers["AC-B1.3"] = {"layers": ["service"], "why": "x"}
        self.assertTrue(any("AC-B1.3" in e and "narrowed" in e for e in run(layers=layers)))
        layers["AC-B1.3"]["narrowed"] = "a reviewed reason"
        self.assertEqual([e for e in run(layers=layers) if "AC-B1.3" in e], [])

    def test_an_empty_parse_fails(self):
        errors = run(crit={}, layers={}, found={})
        self.assertTrue(any("changed shape" in e for e in errors), errors)

    def test_a_v0_story_with_no_criterion_must_be_listed(self):
        cfg = real()["cfg"]
        del cfg["story_without_criteria"]["US-H2"]
        self.assertTrue(any("US-H2" in e for e in run(cfg=cfg)))

    def test_a_live_story_outside_the_v0_table_fails(self):
        v0 = real()["v0"] - {"US-G1"}
        self.assertTrue(any("US-G1" in e and "section 15" in e for e in run(v0=v0)))

    def test_the_scanner_ignores_what_is_not_attached_to_a_test(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / "t.rs"
            f.write_text("//! module header naming AC-G1.2\n\n// a section comment naming AC-G1.1\n\n"
                         "#[test]\nfn body_only() {\n    // AC-G1.3 in the body, and \"AC-H1.1\"\n}\n\n"
                         "/// AC-H1.2: attached\n#[test]\nfn attached() {\n}\n\n"
                         "#[test]\n#[ignore]\n/// AC-H1.3 on an ignored test\nfn ignored() {\n}\n\n"
                         "#[test]\nfn ac_e1_1_by_name() {\n}\n")
            got = {n: t for n, t in cm.rust_tests(f)}
        self.assertEqual(set(got), {"body_only", "attached", "ac_e1_1_by_name"})
        self.assertEqual(cm.AC.findall(got["attached"]), ["AC-H1.2"])
        self.assertEqual(cm.AC.findall(got["body_only"]), [])


if __name__ == "__main__":
    unittest.main()
