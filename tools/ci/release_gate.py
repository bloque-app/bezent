#!/usr/bin/env python3
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""F-013 release gate (SPEC section 15, PLAN DEC-19 and M-4). Fails closed: anything not shown is a failure.

What it checks, in two kinds.

Mechanical, here and now:
  1. the criteria map (tools/ci/criteria_map.py): every V0 acceptance criterion has a test at its layer;
  2. with --online: every check named in release/gate.toml succeeded on the release commit (a skipped or missing
     check is a failure; every check is read at the release commit itself), and each sign-off's pull request was merged with
     an approving review by the person named.

Human, recorded in release/evidence.toml and demanded here, each tied to this release commit:
  3. every runbook exercised on a test network (NFR-9, REQ-IC-6): one per named condition (SPEC section 10) and one
     per incident class (notes/BRIDGE-REQUIREMENTS.md section 5);
  4. the exit criteria of section 15 as measured on the test bed: the reserve equation at zero divergence for the
     sustained window of release/gate.toml (unruled: SPEC gives no duration, amendment 0021 proposes one, so the
     gate fails until the owner sets it), and every final source leg delivered, refunded or held with an owner;
  5. the 48-hour mainnet soak of DEC-19, with at least two verified daily statements and zero divergence;
  6. sign-offs of the spec owner and a maintainer.

Items 3 to 6 need a live environment (a test bed running the core against zombienet and the testnets, a mainnet
instance) that does not exist in CI; this script cannot produce them, only refuse to pass without them.

    release_gate.py --release <sha> [--online --repo owner/name] [--evidence release/evidence.toml]
"""
import argparse
import datetime
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import criteria_map  # noqa: E402

ROOT = criteria_map.ROOT
NETWORKS = {"zombienet", "anvil", "testnet"}


def named_conditions():
    """The live `CND-*` rows of SPEC section 10 (a withdrawn row is out)."""
    text = (ROOT / "SPEC.md").read_text()
    sec = text[text.index("### Named conditions"):text.index("## 11. ")]
    return sorted(m.group(1) for m in re.finditer(r"^\| `(CND-[A-Za-z]+)` \| (?!\*Withdrawn)", sec, re.M))


def incident_classes():
    """I-1 .. I-n of notes/BRIDGE-REQUIREMENTS.md section 5 (REQ-IC-6)."""
    text = (ROOT / "notes/BRIDGE-REQUIREMENTS.md").read_text()
    sec = text[text.index("## 5. Incidents"):text.index("## Appendix A")]
    return sorted((m.group(1) for m in re.finditer(r"^\| (I-\d+) \|", sec, re.M)), key=lambda x: int(x[2:]))


def need(table, key, errors, where, pred=bool, what="set"):
    v = table.get(key)
    if v is None or not pred(v):
        errors.append(f"{where}: `{key}` is not {what}")
        return None
    return v


def is_url(v):
    return isinstance(v, str) and v.startswith("https://")


def is_date(v):
    if isinstance(v, datetime.date):
        return True
    try:
        datetime.date.fromisoformat(str(v))
        return True
    except ValueError:
        return False


def human(gate, ev, release, conds, incidents):
    """Items 3 to 6. Returns (errors, lines)."""
    errors, lines = [], []
    if not conds or not incidents:
        errors.append(f"runbooks: parsed {len(conds)} named conditions and {len(incidents)} incident classes; "
                      "the SPEC or the notes changed shape, fix the parser, do not skip the check")
    if not ev:
        errors.append("evidence: release/evidence.toml is missing or empty")
        ev = {}  # carry on: the report names every item still owed
    if ev.get("release_commit") != release:
        errors.append(f"evidence: release_commit is {ev.get('release_commit')!r}, the release is {release!r} "
                      "(evidence of another commit proves nothing about this one)")
    rb = ev.get("runbook", {})
    for item in conds + incidents:
        e, where = rb.get(item), f"runbook {item}"
        if not isinstance(e, dict):
            errors.append(f"{where}: not exercised (no entry)")
            continue
        n0 = len(errors)
        if e.get("status") != "exercised":
            errors.append(f"{where}: status is {e.get('status')!r}, not 'exercised'")
        need(e, "network", errors, where, lambda v: v in NETWORKS, f"one of {sorted(NETWORKS)} (NFR-9: a test network)")
        need(e, "on", errors, where, is_date, "an ISO date")
        need(e, "evidence", errors, where, is_url, "an https link")
        need(e, "by", errors, where)
        if len(errors) == n0:
            lines.append(f"runbook {item}: exercised on {e['network']} by {e['by']}")
    for item in sorted(set(rb) - set(conds) - set(incidents)):
        errors.append(f"runbook {item}: names nothing in SPEC section 10 or the incident classes (misspelt, or withdrawn)")

    window = gate.get("exit", {}).get("sustained_divergence_free_hours")
    if not isinstance(window, int) or window <= 0:
        errors.append("exit: release/gate.toml [exit].sustained_divergence_free_hours is unset. SPEC section 15 says "
                      "'sustained' and gives no duration; amendment 0021 proposes one and is not ruled")
    div = ev.get("exit", {}).get("divergence", {})
    if need(div, "window_hours", errors, "exit.divergence", lambda v: isinstance(v, int) and v > 0, "a positive integer"):
        if isinstance(window, int) and div["window_hours"] < window:
            errors.append(f"exit.divergence: {div['window_hours']} h measured, {window} h required")
    if div.get("divergences") != 0:
        errors.append(f"exit.divergence: `divergences` is {div.get('divergences')!r}, must be exactly 0 (zero tolerance)")
    need(div, "evidence", errors, "exit.divergence", is_url, "an https link")
    need(div, "by", errors, "exit.divergence")
    legs = ev.get("exit", {}).get("legs", {})
    if legs.get("unresolved") != 0:
        errors.append(f"exit.legs: `unresolved` is {legs.get('unresolved')!r}: every final source leg must be "
                      "delivered, refunded or held in a named condition with an owner (must be exactly 0)")
    need(legs, "evidence", errors, "exit.legs", is_url, "an https link")
    need(legs, "by", errors, "exit.legs")

    soak, want = ev.get("soak", {}), gate.get("soak", {})
    hours, per_day = want.get("hours"), want.get("statements_per_day")
    if not isinstance(hours, int) or not isinstance(per_day, int):
        errors.append("soak: release/gate.toml [soak] needs integer `hours` and `statements_per_day` (DEC-19: 48, 2)")
    else:
        if not isinstance(soak.get("hours"), int) or soak["hours"] < hours:
            errors.append(f"soak: {soak.get('hours')!r} h on mainnet, {hours} h required (DEC-19)")
        elif not isinstance(soak.get("statements_verified"), int) or soak["statements_verified"] < per_day * (soak["hours"] // 24):
            errors.append(f"soak: {soak.get('statements_verified')!r} verified statements, at least {per_day} a day required")
        if soak.get("divergences") != 0:
            errors.append(f"soak: `divergences` is {soak.get('divergences')!r}; a divergence restarts the clock (DEC-19)")
    need(soak, "evidence", errors, "soak", is_url, "an https link")
    need(soak, "by", errors, "soak")

    for role in gate.get("signoff", {}).get("required", []):
        s = ev.get("signoff", {}).get(role, {})
        for k, pred in (("by", bool), ("pr", lambda v: isinstance(v, int) and v > 0), ("on", is_date)):
            need(s, k, errors, f"signoff {role}", pred)
    if not gate.get("signoff", {}).get("required"):
        errors.append("signoff: release/gate.toml lists no required sign-off")
    return errors, lines


def online_checks(gate, release, repo, api):
    """Item 2. `api(path)` returns parsed JSON. Every check is read at the release commit itself: a squash merge
    carries none of its pull request's runs, and a path-filtered workflow does not run on a commit that touches
    none of its paths, so a missing run is a failure, and the release procedure dispatches the workflow on the
    release commit."""
    errors = []
    reqs = gate.get("check", [])
    if not reqs:
        errors.append("checks: release/gate.toml lists no required check")
    for r in reqs:
        sha = release
        runs = api(f"repos/{repo}/commits/{sha}/check-runs?per_page=100&filter=latest")["check_runs"]
        mine = [c for c in runs if c["name"] == r["name"]]
        if not mine:
            errors.append(f"check {r['name']!r}: no run at {sha[:10]} (dispatch its workflow on that commit)")
        elif any(c["conclusion"] != "success" for c in mine):
            errors.append(f"check {r['name']!r} at {sha[:10]}: {sorted({str(c['conclusion']) for c in mine})} (only success passes)")
    return errors


def online_signoffs(gate, ev, repo, api):
    errors = []
    for role in gate.get("signoff", {}).get("required", []):
        s = (ev or {}).get("signoff", {}).get(role, {})
        if not (s.get("pr") and s.get("by")):
            continue  # already an error from human()
        pr = api(f"repos/{repo}/pulls/{s['pr']}")
        reviews = api(f"repos/{repo}/pulls/{s['pr']}/reviews?per_page=100")
        if not pr.get("merged"):
            errors.append(f"signoff {role}: PR #{s['pr']} is not merged")
        last = {}
        for r in reviews:
            last[r["user"]["login"]] = r["state"]
        if last.get(s["by"]) != "APPROVED":
            errors.append(f"signoff {role}: {s['by']} has no standing approval on PR #{s['pr']}")
    return errors


def mechanical_map():
    crit, _, stories = criteria_map.spec_criteria()
    layers = tomllib.loads((ROOT / "e2e/criteria/layers.toml").read_text()).get("layer", {})
    cfg = tomllib.loads((ROOT / "e2e/criteria/exceptions.toml").read_text())
    errors, table = criteria_map.check(crit, stories, criteria_map.v0_stories(), criteria_map.invariant_layers(),
                                       layers, cfg, criteria_map.scan())
    excepted = [t["ac"] for t in table if t["status"] == "exception"]
    if excepted:
        # A criterion without a test at its layer is a release blocker; the map allows it while the work is in flight.
        errors.append(f"criteria: {len(excepted)} V0 criteria have no test yet and are excepted: {', '.join(excepted)}")
    return errors


def gh_api(path):
    out = subprocess.run(["gh", "api", path], check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def run(release, evidence_path, online, repo, api=gh_api):
    errors, notes = [], []
    gate_path = ROOT / "release/gate.toml"
    try:
        gate = tomllib.loads(gate_path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        return [f"release/gate.toml: {e}"], notes
    try:
        ev = tomllib.loads(Path(evidence_path).read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        ev = {}
        errors.append(f"evidence: {evidence_path}: {e}")
    errors += mechanical_map()
    if online:
        errors += online_checks(gate, release, repo, api)
        errors += online_signoffs(gate, ev, repo, api)
    else:
        errors.append("checks and sign-off reviews were not verified (run with --online): a gate that skips them fails")
    h_errors, notes = human(gate, ev, release, named_conditions(), incident_classes())
    return errors + h_errors, notes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--release", required=True, help="the full commit sha being released")
    ap.add_argument("--evidence", default=str(ROOT / "release/evidence.toml"))
    ap.add_argument("--online", action="store_true")
    ap.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", "bloque-app/bezent"))
    a = ap.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", a.release):
        print("error: --release must be a full 40-hex commit sha")
        sys.exit(2)
    errors, notes = run(a.release, a.evidence, a.online, a.repo)
    print(f"### Release gate for {a.release[:10]}: {'FAILED' if errors else 'passed'}\n")
    for n in notes:
        print(f"- ok: {n}")
    for e in errors:
        print(f"::error::{e}" if os.environ.get("GITHUB_ACTIONS") else f"- FAIL: {e}")
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
