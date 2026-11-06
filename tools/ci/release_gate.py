#!/usr/bin/env python3
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""F-013 release gate (SPEC section 15, PLAN DEC-19 and M-4). Fails closed: anything not shown is a failure.

What it checks, in two kinds, and for two classes of release (amendment 0024, SPEC section 15).

A release is `value-moving` (the first release that lets a bridged asset carry value, and each that adds an asset or a
network to what carries value) or `ordinary` (every other). The class is the top-level `class` of release/evidence.toml,
the one source of it; a missing, non-string or unknown class is refused and never read as ordinary. A value-moving
release needs everything below. An ordinary release needs 1 (the map itself, not its exceptions), 2 and the sign-off of
item 6 for the spec owner alone, and skips items 3 to 5, which are what is proven before value moves.

Mechanical, here and now:
  1. the criteria map (tools/ci/criteria_map.py): every V0 acceptance criterion has a test at its layer; for a
     value-moving release, none is an exception (no test yet) or known unmet (a test, yet the criterion is not met:
     e2e/criteria/known_unmet.toml);
  2. with --online: every check named in release/gate.toml succeeded on the release commit (a skipped or missing
     check is a failure; every check is read at the release commit itself), and each sign-off is backed by the pull
     request that merged this release's release/evidence.toml, approved on its final commit by the person named, who
     is a member of the role's team (release/roles.toml), the roles of the class being different people.

Human, recorded in release/evidence.toml and demanded here, each tied to this release commit (value-moving only, but 6):
  3. every runbook exercised on a test network (NFR-9, REQ-IC-6): one per named condition (SPEC section 10) and one
     per incident class (notes/BRIDGE-REQUIREMENTS.md section 5);
  4. the exit criteria of section 15 as measured on the test bed: the reserve equation at zero divergence for the
     sustained window of release/gate.toml (24 hours, amendment 0021), and every final source leg delivered, refunded
     or held with an owner;
  5. the 48-hour mainnet soak of DEC-19, with at least two verified daily statements and zero divergence;
  6. sign-offs: the spec owner and a maintainer for a value-moving release, one spec owner for an ordinary one
     (release/gate.toml, [class.*].signoff).

Items 3 to 6 need a live environment (a test bed running the core against zombienet and the testnets, a mainnet
instance) that does not exist in CI; this script cannot produce them, only refuse to pass without them.

A file cannot hold the hash of the commit it is in, so the release and the evidence are two commits: `--release`
is the commit of code being released (default: the `release_commit` the evidence names), the checkout is a later
commit (the workflow's own ref) that adds the evidence, and the only file that may differ between the two is
release/evidence.toml. The checks and the criteria map are read at the release commit.

    release_gate.py [--release <sha>] [--online --repo owner/name] [--evidence release/evidence.toml]
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
VALUE_MOVING, ORDINARY = "value-moving", "ordinary"
CLASSES = (VALUE_MOVING, ORDINARY)
# The role every class needs: whatever gate.toml says, no class is signed off without a spec owner.
SPEC_OWNER = "spec-owner"
GATE_PATH = ROOT / "release/gate.toml"
ROLES_PATH = ROOT / "release/roles.toml"
EVIDENCE = "release/evidence.toml"
MAX_PAGES = 50  # of 100: a list longer than 5000 is refused rather than read in part


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


def is_int(v):
    """A true integer: `True` is an int to Python and must not pass as a count."""
    return type(v) is int


def is_zero(v):
    """Exactly the integer 0: `False` and `0.0` equal 0 in Python and are not a count of nothing."""
    return type(v) is int and v == 0


def is_name(v):
    """A login: a non-blank string. `1` and `true` are truthy and name nobody."""
    return isinstance(v, str) and bool(v.strip())


def sub(table, key, errors, where):
    """`table[key]` when it is a table. Absent is an empty table (the checks below name what is owed); present but
    not a table (a list, a string, a number) is an error and also an empty table, so a wrong type never skips a check
    and never raises a traceback."""
    v = table.get(key, {}) if isinstance(table, dict) else {}
    if not isinstance(v, dict):
        errors.append(f"{where}: `{key}` must be a table, found {type(v).__name__}")
        return {}
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


def release_class(ev):
    """The release's class from its evidence, and an error when it is not one of CLASSES. There is no default:
    absent, a non-string and an unknown value are all refused, so a missing class never reads as `ordinary`."""
    c = ev.get("class") if isinstance(ev, dict) else None
    if isinstance(c, str) and c in CLASSES:
        return c, []
    return None, [f"class: release/evidence.toml `class` is {c!r}, it must be one of {list(CLASSES)} "
                  "(amendment 0024: a release that does not declare its class is refused, never treated as ordinary)"]


def class_roles(gate, cls, errors):
    """The sign-off roles of `cls` from release/gate.toml `[class.<cls>].signoff`. It must be a non-empty list of
    role names that includes the spec owner (a file edit cannot drop them); a value-moving release also needs a
    second role, so that two people sign it."""
    table = sub(sub(gate, "class", errors, "release/gate.toml"), cls, errors, "release/gate.toml [class]")
    roles = table.get("signoff", [])
    if not isinstance(roles, list) or not roles or not all(is_name(r) for r in roles):
        errors.append(f"signoff: release/gate.toml [class.{cls}].signoff must be a non-empty list of role names")
        return []
    if SPEC_OWNER not in roles:
        errors.append(f"signoff: release/gate.toml [class.{cls}].signoff must include {SPEC_OWNER!r}")
    if cls == VALUE_MOVING and len(set(roles)) < 2:
        errors.append(f"signoff: release/gate.toml [class.{VALUE_MOVING}].signoff needs two different roles")
    return roles


def human(gate, ev, release, conds, incidents, cls=None):
    """Items 3 to 6. Returns (errors, lines). `cls` is the release's class (default: the evidence's own)."""
    errors, lines = [], []
    if cls is None:
        cls, class_errors = release_class(ev)
        errors += class_errors
    if not conds or not incidents:
        errors.append(f"runbooks: parsed {len(conds)} named conditions and {len(incidents)} incident classes; "
                      "the SPEC or the notes changed shape, fix the parser, do not skip the check")
    if not ev:
        errors.append("evidence: release/evidence.toml is missing or empty")
        ev = {}  # carry on: the report names every item still owed
    if ev.get("release_commit") != release:
        errors.append(f"evidence: release_commit is {ev.get('release_commit')!r}, the release is {release!r} "
                      "(evidence of another commit proves nothing about this one)")
    # Items 3 to 5 are what is proven before value moves: a value-moving release needs them, an ordinary one does
    # not. An unknown class (already an error) is held to them too: it never gets the lighter path.
    if cls != ORDINARY:
        rb = sub(ev, "runbook", errors, "evidence")
        for item in conds + incidents:
            e, where = rb.get(item), f"runbook {item}"
            if not isinstance(e, dict):
                errors.append(f"{where}: not exercised (no entry)")
                continue
            n0 = len(errors)
            if e.get("status") != "exercised":
                errors.append(f"{where}: status is {e.get('status')!r}, not 'exercised'")
            need(e, "network", errors, where, lambda v: isinstance(v, str) and v in NETWORKS, f"one of {sorted(NETWORKS)} (NFR-9: a test network)")
            need(e, "on", errors, where, is_date, "an ISO date")
            need(e, "evidence", errors, where, is_url, "an https link")
            need(e, "by", errors, where, is_name, "a name (a string)")
            if len(errors) == n0:
                lines.append(f"runbook {item}: exercised on {e['network']} by {e['by']}")
        for item in sorted(set(rb) - set(conds) - set(incidents)):
            errors.append(f"runbook {item}: names nothing in SPEC section 10 or the incident classes (misspelt, or withdrawn)")

        window = sub(gate, "exit", errors, "release/gate.toml").get("sustained_divergence_free_hours")
        if not is_int(window) or window <= 0:
            errors.append("exit: release/gate.toml [exit].sustained_divergence_free_hours must be a positive integer "
                          "of hours (24, ruled in amendment 0021)")
        ex = sub(ev, "exit", errors, "evidence")
        div = sub(ex, "divergence", errors, "exit")
        if need(div, "window_hours", errors, "exit.divergence", lambda v: is_int(v) and v > 0, "a positive integer"):
            if is_int(window) and div["window_hours"] < window:
                errors.append(f"exit.divergence: {div['window_hours']} h measured, {window} h required")
        if not is_zero(div.get("divergences")):
            errors.append(f"exit.divergence: `divergences` is {div.get('divergences')!r}, must be exactly 0 (zero tolerance)")
        need(div, "evidence", errors, "exit.divergence", is_url, "an https link")
        need(div, "by", errors, "exit.divergence", is_name, "a name (a string)")
        legs = sub(ex, "legs", errors, "exit")
        if not is_zero(legs.get("unresolved")):
            errors.append(f"exit.legs: `unresolved` is {legs.get('unresolved')!r}: every final source leg must be "
                          "delivered, refunded or held in a named condition with an owner (must be exactly 0)")
        need(legs, "evidence", errors, "exit.legs", is_url, "an https link")
        need(legs, "by", errors, "exit.legs", is_name, "a name (a string)")

        soak, want = sub(ev, "soak", errors, "evidence"), sub(gate, "soak", errors, "release/gate.toml")
        hours, per_day = want.get("hours"), want.get("statements_per_day")
        if not is_int(hours) or not is_int(per_day):
            errors.append("soak: release/gate.toml [soak] needs integer `hours` and `statements_per_day` (DEC-19: 48, 2)")
        else:
            if not is_int(soak.get("hours")) or soak["hours"] < hours:
                errors.append(f"soak: {soak.get('hours')!r} h on mainnet, {hours} h required (DEC-19)")
            elif not is_int(soak.get("statements_verified")) or soak["statements_verified"] < per_day * (soak["hours"] // 24):
                errors.append(f"soak: {soak.get('statements_verified')!r} verified statements, at least {per_day} a day required")
            if not is_zero(soak.get("divergences")):
                errors.append(f"soak: `divergences` is {soak.get('divergences')!r}; a divergence restarts the clock (DEC-19)")
        need(soak, "evidence", errors, "soak", is_url, "an https link")
        need(soak, "by", errors, "soak", is_name, "a name (a string)")

    required = class_roles(gate, cls if cls in CLASSES else VALUE_MOVING, errors)
    signed = sub(ev, "signoff", errors, "evidence")
    for role in required:
        s = sub(signed, role, errors, "signoff")
        for k, pred, what in (("by", is_name, "a login (a string)"), ("pr", lambda v: is_int(v) and v > 0, "a positive integer"),
                              ("on", is_date, "an ISO date")):
            need(s, k, errors, f"signoff {role}", pred, what)
    return errors, lines


def paged(api, path, key=None):
    """Every item of a GitHub list: `api` is asked for page after page of 100 until one is short. A list that
    would need more than MAX_PAGES pages is an error, never a partial read (a later dismissal, change request
    or failed check could be on a page not read). `key` names the list inside an object (check-runs)."""
    items = []
    sep = "&" if "?" in path else "?"
    for page in range(1, MAX_PAGES + 1):
        data = api(f"{path}{sep}per_page=100&page={page}")
        batch = data[key] if key else data
        items += batch
        if len(batch) < 100:
            return items
    raise RuntimeError(f"{path}: more than {MAX_PAGES * 100} items; not reading part of a list")


def online_checks(gate, release, repo, api):
    """Item 2. `api(path)` returns parsed JSON. Every check is read at the release commit itself: a squash merge
    carries none of its pull request's runs, and a path-filtered workflow does not run on a commit that touches
    none of its paths, so a missing run is a failure, and the release procedure dispatches the workflow on the
    release commit."""
    errors = []
    reqs = gate.get("check", [])
    if not reqs:
        errors.append("checks: release/gate.toml lists no required check")
        return errors
    sha = release
    runs = paged(api, f"repos/{repo}/commits/{sha}/check-runs?filter=latest", "check_runs")
    for r in reqs:
        mine = [c for c in runs if c.get("name") == r["name"]]
        if not mine:
            errors.append(f"check {r['name']!r}: no run at {sha[:10]} (dispatch its workflow on that commit)")
        elif any(c.get("conclusion") != "success" for c in mine):
            errors.append(f"check {r['name']!r} at {sha[:10]}: {sorted({str(c.get('conclusion')) for c in mine})} (only success passes)")
    return errors


def standing_approvers(reviews):
    """Logins whose latest decisive review is an approval, with the commit they approved. A comment does not
    withdraw an approval (GitHub's own rule); a dismissal or a change request does. A review whose author was
    deleted (`user` is null) is nobody's."""
    last = {}
    for r in sorted(reviews, key=lambda r: r.get("submitted_at") or ""):
        user = (r.get("user") or {}).get("login")
        if user and r.get("state") in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            last[user.lower()] = (r["state"], r.get("commit_id"))
    return {u: sha for u, (state, sha) in last.items() if state == "APPROVED"}


def evidence_pr_errors(where, pr, files, release, git_fn):
    """The pull request is the one that merged this release's release/evidence.toml, as the gate sees it."""
    errors = []
    if EVIDENCE not in files:
        errors.append(f"{where}: does not change {EVIDENCE}; a sign-off rests on the pull request that merged the evidence")
    merged = pr.get("merge_commit_sha")
    if not isinstance(merged, str) or not re.fullmatch(r"[0-9a-f]{40}", merged):
        return errors + [f"{where}: GitHub gives no merge commit"]
    if merged == release or git_fn("merge-base", "--is-ancestor", release, merged)[0] != 0:
        errors.append(f"{where}: merged as {merged[:10]}, which does not follow the release commit {release[:10]} "
                      "(a pull request from before the release cannot sign it off)")
    if git_fn("merge-base", "--is-ancestor", merged, "HEAD")[0] != 0:
        errors.append(f"{where}: merged as {merged[:10]}, which is not in the history of the commit checked out")
    theirs, ours = git_fn("rev-parse", f"{merged}:{EVIDENCE}"), git_fn("rev-parse", f"HEAD:{EVIDENCE}")
    if theirs[0] != 0 or ours[0] != 0 or theirs[1] != ours[1]:
        errors.append(f"{where}: {EVIDENCE} at {merged[:10]} is not the file being gated (changed since, or absent)")
    return errors


def online_signoffs(gate, ev, repo, api, roles, release, git_fn):
    """Each required role: `by` is a member of the role's reviewed list (`roles`, release/roles.toml: the
    workflow's token cannot read organisation teams), the pull request is the one that merged this release's
    evidence, it is merged, `by` approved its final commit, and no two roles are one person."""
    errors, people = [], {}
    cls, class_errors = release_class(ev)
    if cls is None:
        return ["signoff: the release has no valid class, so no sign-off can be checked (fail closed)"]
    required = class_roles(gate, cls, errors)
    if errors:
        return errors
    for role in required:
        where = f"signoff {role}"
        signed = (ev or {}).get("signoff")
        s = signed.get(role) if isinstance(signed, dict) else None
        if not isinstance(s, dict):
            errors.append(f"{where}: no sign-off table in the evidence; not checked online, and a gate that skips a role fails")
            continue
        n, by = s.get("pr"), s.get("by")
        if not (is_int(n) and n > 0 and is_name(by)):
            # Never a silent skip: with no pull request or no login there is no approval, roles-list, merge or
            # two-person check to run, and that is a failure of its own, not only of human().
            errors.append(f"{where}: `by` must be a login (a string) and `pr` a positive integer, found "
                          f"by={by!r}, pr={n!r}; the approval, roles-list, merge and two-person checks cannot run")
            continue
        entry = (roles.get("role") or {}).get(role) if isinstance(roles.get("role"), dict) else None
        members_raw = entry.get("members", []) if isinstance(entry, dict) else []
        if not isinstance(members_raw, list):
            errors.append(f"{where}: release/roles.toml `members` must be a list of logins, found {type(members_raw).__name__}")
            members_raw = []
        members = {m.lower() for m in members_raw if isinstance(m, str)}
        if not members:
            errors.append(f"{where}: release/roles.toml lists no member for the role (fail closed)")
        elif by.lower() not in members:
            errors.append(f"{where}: {by} is not in the role's list in release/roles.toml")
        people.setdefault(by.lower(), []).append(role)
        pr = api(f"repos/{repo}/pulls/{n}")
        if not pr.get("merged"):
            errors.append(f"{where}: PR #{n} is not merged")
        else:
            files = [f.get("filename") for f in paged(api, f"repos/{repo}/pulls/{n}/files")]
            errors += evidence_pr_errors(f"{where}: PR #{n}", pr, files, release, git_fn)
        approved = standing_approvers(paged(api, f"repos/{repo}/pulls/{n}/reviews"))
        head = (pr.get("head") or {}).get("sha")
        if by.lower() not in approved:
            errors.append(f"{where}: {by} has no standing approval on PR #{n}")
        elif not head or approved[by.lower()] != head:
            errors.append(f"{where}: {by}'s approval on PR #{n} is not of its final commit")
    for person, rs in people.items():
        if len(rs) > 1:
            errors.append(f"signoff: {person} signs {' and '.join(rs)}; each role needs a different person")
    return errors


def blockers(table):
    """What the map allows on a pull request and a value-moving release does not: an exception (no test yet) and a
    known-unmet entry (a test, yet the criterion is known not met). An ordinary release is not held to it."""
    errors = []
    excepted = [t["ac"] for t in table if t["status"] == "exception"]
    if excepted:
        # A criterion without a test at its layer is a release blocker; the map allows it while the work is in flight.
        errors.append(f"criteria: {len(excepted)} V0 criteria have no test yet and are excepted: {', '.join(excepted)}")
    unmet = [t for t in table if t["status"] == "known unmet"]
    if unmet:
        errors.append(f"criteria: {len(unmet)} V0 criteria have a test and are known not met "
                      "(e2e/criteria/known_unmet.toml): "
                      + "; ".join(f"{t['ac']} lacks {t['known_unmet']['missing']} ({t['known_unmet']['closed_by']})" for t in unmet))
    return errors


def mechanical_map(cls=VALUE_MOVING):
    """The criteria map. An exception or a known-unmet entry blocks a value-moving release (and an unclassed one);
    an ordinary release needs the map itself to hold, which is what CI already asks of every pull request."""
    crit, _, stories = criteria_map.spec_criteria()
    layers = tomllib.loads((ROOT / "e2e/criteria/layers.toml").read_text()).get("layer", {})
    try:
        cfg = criteria_map.load_config()
    except criteria_map.ConfigError as e:
        return [f"criteria: {e}"]  # a gate error, not a traceback
    errors, table = criteria_map.check(crit, stories, criteria_map.v0_stories(), criteria_map.invariant_layers(),
                                       layers, cfg, criteria_map.scan())
    return errors + (blockers(table) if cls != ORDINARY else [])


def gh_api(path):
    r = subprocess.run(["gh", "api", path], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"gh api {path.split('?')[0]} failed: {r.stderr.strip()[:300]}")
    return json.loads(r.stdout)


def git(*args):
    r = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    return r.returncode, r.stdout.strip()


def evidence_follows_release(release, git_fn=git):
    """The checkout is the release commit or a descendant of it that differs only by release/evidence.toml."""
    errors = []
    code, _ = git_fn("merge-base", "--is-ancestor", release, "HEAD")
    if code != 0:
        return [f"release: {release[:10]} is not an ancestor of the commit checked out (is the checkout shallow?)"]
    code, out = git_fn("diff", "--name-only", release, "HEAD")
    if code != 0:
        return [f"release: cannot diff {release[:10]} against the checkout"]
    other = [f for f in out.splitlines() if f and f != "release/evidence.toml"]
    if other:
        errors.append(f"release: the checkout differs from {release[:10]} beyond release/evidence.toml "
                      f"({', '.join(other[:5])}{', ...' if len(other) > 5 else ''}); the evidence speaks for that code only")
    return errors


def run(release, evidence_path, online, repo, api=gh_api, git_fn=git):
    errors, notes = [], []
    gate_path = GATE_PATH
    try:
        gate = tomllib.loads(gate_path.read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        return [f"release/gate.toml: {e}"], notes
    try:
        ev = tomllib.loads(Path(evidence_path).read_text())
    except (OSError, tomllib.TOMLDecodeError) as e:
        ev = {}
        errors.append(f"evidence: {evidence_path}: {e}")
    release = release or ev.get("release_commit")
    if not isinstance(release, str) or not re.fullmatch(r"[0-9a-f]{40}", release):
        return errors + ["release: no release commit (--release, or `release_commit` in the evidence)"], notes
    errors += evidence_follows_release(release, git_fn)
    errors += mechanical_map(release_class(ev)[0])
    if online:
        try:
            roles = tomllib.loads(ROLES_PATH.read_text())
        except (OSError, tomllib.TOMLDecodeError) as e:
            roles = {}
            errors.append(f"release/roles.toml: {e}")
        try:
            errors += online_checks(gate, release, repo, api)
            errors += online_signoffs(gate, ev, repo, api, roles, release, git_fn)
        except (subprocess.CalledProcessError, RuntimeError, KeyError, TypeError, ValueError, AttributeError) as e:
            # An unreadable answer is a failure, never a pass and never a traceback nobody reads.
            errors.append(f"github: could not read what the gate needs ({type(e).__name__}: {e})")
    else:
        errors.append("checks and sign-off reviews were not verified (run with --online): a gate that skips them fails")
    h_errors, notes = human(gate, ev, release, named_conditions(), incident_classes())
    return errors + h_errors, notes


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--release", default="", help="full sha of the code being released (default: the evidence's)")
    ap.add_argument("--evidence", default=str(ROOT / "release/evidence.toml"))
    ap.add_argument("--online", action="store_true")
    ap.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", "bloque-app/bezent"))
    a = ap.parse_args()
    errors, notes = run(a.release, a.evidence, a.online, a.repo)
    print(f"### Release gate for {a.release[:10] or 'the evidence'}: {'FAILED' if errors else 'passed'}\n")
    for n in notes:
        print(f"- ok: {n}")
    for e in errors:
        print(f"::error::{e}" if os.environ.get("GITHUB_ACTIONS") else f"- FAIL: {e}")
    sys.exit(1 if errors else 0)


if __name__ == "__main__":
    main()
