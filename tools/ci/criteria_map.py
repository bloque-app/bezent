#!/usr/bin/env python3
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""F-013 criteria map: every V0 acceptance criterion of SPEC.md has a test that names it at its layer.

Reads SPEC.md section 6 for the `AC-*` criteria in V0 scope (a criterion, or its whole story, marked
"Withdrawn" is out), `e2e/criteria/layers.toml` for the layer SPEC section 9 gives each one, and
`e2e/criteria/exceptions.toml` for the criteria that have no test yet, each with a reason, and
`e2e/criteria/known_unmet.toml` for the criteria that have a test yet are known not met, each with what is
missing and the issue or PR that closes it. It scans the
tests (Rust `#[test]` functions, Solidity `test*` functions, `check(...)` calls in e2e/*.mjs) and fails if:

  - a V0 criterion has no test at its layer and is not an exception;
  - an exception names a criterion that has a test (the entry is stale: remove it), or one not in V0 scope;
  - a known-unmet entry names a criterion not in V0 scope, one that is also an exception or has no test at its
    layers (that is an exception, not this), or lacks `missing` or `closed_by`;
  - a layer entry is missing, names a layer weaker than section 9 allows, or names no criterion of the spec.

It proves that a test NAMES a criterion at the right layer. That the test VERIFIES it is the reviewer's
(CONTRIBUTING: a test named for a criterion MUST verify it; never name one that does not).
Run from the repository root. `--table` prints the map as Markdown; `--json` prints it as JSON.
"""
import json
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
AC = re.compile(r"AC-[A-Z]\d+\.\d+")
AC_IN_NAME = re.compile(r"(?<![a-z0-9])ac_([a-z])(\d+)_(\d+)(?![0-9])")
INV = re.compile(r"INV-\d+")
LAYERS = ("chain", "store", "service", "custody")

# Where a test lives decides the layer it speaks for (SPEC section 9). First match wins.
LAYER_OF_PATH = [
    ("contracts/", "chain"),
    ("e2e/kreivo/", "chain"),
    ("crates/store/", "store"),
    ("services/signer/", "custody"),
    ("crates/vault/", "custody"),
    ("services/", "service"),
    ("crates/", "service"),
]
# The workflow that runs a test, by path (first match wins): the table says where each name is exercised, since
# the zombienet job runs only on the self-hosted pool and never on a pull request.
RUNS_IN = [
    ("contracts/bezent/", "contracts.yml"),
    ("contracts/evm/", "evm-contracts.yml"),
    ("e2e/kreivo/", "e2e-zombienet.yml (self-hosted pool, not on PRs)"),
    ("services/evm-endpoint/tests/fork.rs", "ci.yml evm-endpoint-fork"),
    ("", "ci.yml test"),
]
SKIP_DIRS = {"node_modules", "target", ".git", "lib", "out", "cache", "broadcast"}


class ConfigError(ValueError):
    """A criteria config file that says something the map would otherwise drop or let override another."""


# The tables each file may hold. known_unmet.toml is the spec owner's (CODEOWNERS), exceptions.toml the maintainers':
# a `[known_unmet]` table in exceptions.toml would be overwritten by the other file's (or, if that file had none, would
# stand in for it, without the spec owner), and the reverse is the same; neither is allowed.
CONFIG_TABLES = {
    "e2e/criteria/exceptions.toml": {"exception", "story_without_criteria"},
    "e2e/criteria/known_unmet.toml": {"known_unmet"},
}


def load_config(root=None):
    """exceptions.toml and known_unmet.toml as one config: [exception], [story_without_criteria], [known_unmet].
    A table in the wrong file, or one the map does not know, is a ConfigError, never silently dropped."""
    root = root or ROOT
    cfg = {}
    for name, allowed in CONFIG_TABLES.items():
        data = tomllib.loads((root / name).read_text())
        stray = sorted(set(data) - allowed)
        if stray:
            raise ConfigError(f"{name}: table(s) {', '.join(f'[{t}]' for t in stray)} not allowed here "
                              f"(this file holds {', '.join(f'[{t}]' for t in sorted(allowed))})")
        cfg.update(data)
    return cfg


def die(msgs):
    for m in msgs:
        print(f"::error::{m}" if GITHUB else f"error: {m}")
    sys.exit(1)


GITHUB = bool(__import__("os").environ.get("GITHUB_ACTIONS"))


def spec_criteria():
    """V0 criteria: the bullet lines of section 6 that are not withdrawn, with the invariants each cites."""
    text = (ROOT / "SPEC.md").read_text()
    start = text.index("\n## 6. User stories")
    end = text.index("\n## 7. ")
    stories = {}
    out, withdrawn, story, cur = {}, [], None, None
    for line in text[start:end].splitlines():
        m = re.match(r"\*\*`(US-[A-Z]\d+)`\*\*(.*)", line)
        if m:
            cur = None
            story = m.group(1)
            stories[story] = {"withdrawn": "Withdrawn" in m.group(2)[:40], "text": m.group(2)}
            if stories[story]["withdrawn"]:
                withdrawn.append(story)
                story = None
            continue
        m = re.match(r"- `(AC-[A-Z]\d+\.\d+)`\s*(.*)", line)
        if m and story:
            cur = None
            if m.group(2).lstrip().startswith("*Withdrawn"):
                withdrawn.append(m.group(1))
                continue
            cur = out[m.group(1)] = {"story": story, "text": m.group(2)}
        elif cur is not None and line.startswith("  ") and line.strip():
            cur["text"] += " " + line.strip()  # a criterion continues on its indented lines
        else:
            cur = None
    for entry in out.values():
        entry["invs"] = sorted(set(INV.findall(entry["text"])))
    return out, withdrawn, stories


def v0_stories():
    """The stories section 15's release-scope table puts in V0 (ranges such as `US-H1`-`US-H3` expanded)."""
    text = (ROOT / "SPEC.md").read_text()
    sec = text[text.index("### Release scope"):]
    out = set()
    for row in re.findall(r"^\| [A-I] \u2014[^|]*\| ([^|]*) \|", sec, re.M):
        for a, b in re.findall(r"`(US-[A-Z]\d+)`\u2013`(US-[A-Z]\d+)`", row):
            out |= {f"{a[:4]}{n}" for n in range(int(a[4:]), int(b[4:]) + 1)}
        out |= set(re.findall(r"`(US-[A-Z]\d+)`", row))
    return out


def invariant_layers():
    text = (ROOT / "SPEC.md").read_text()
    sec = text[text.index("\n## 9. Invariants"):text.index("\n## 10. ")]
    out = {}
    for m in re.finditer(r"^\| `(INV-\d+)` \|.*\| ([^|]+) \|$", sec, re.M):
        out[m.group(1)] = {w.strip().lower() for w in re.split(r"\+|\(", m.group(2).replace("(continuously checked)", ""))
                           if w.strip().lower() in LAYERS}
    return out


def layer_of(rel):
    for prefix, layer in LAYER_OF_PATH:
        if rel.startswith(prefix):
            return layer
    return None


def files(patterns):
    for pat in patterns:
        for p in sorted(ROOT.glob(pat)):
            if p.is_file() and not (set(p.relative_to(ROOT).parts) & SKIP_DIRS):
                yield p


RUST_TEST_ATTR = re.compile(r"^\s*#\[(?:[\w:]*::)?(?:test|rstest)\b")
FN = re.compile(r"^(\s*)(?:pub\s+)?(?:async\s+)?fn\s+(\w+)")


def rust_tests(path):
    """Yield (name, attached) for each runnable (not #[ignore]) test fn: its name and the comment block above it."""
    lines = path.read_text().splitlines()
    i = 0
    while i < len(lines):
        if RUST_TEST_ATTR.match(lines[i]):
            # Attributes and docs may sit above and below the test attribute; gather the contiguous block.
            top = i
            while top > 0 and re.match(r"\s*(///|#\[|//)", lines[top - 1]):
                top -= 1
            j, ignored = i, False
            while j < len(lines) and not FN.match(lines[j]):
                ignored |= bool(re.match(r"\s*#\[ignore", lines[j]))
                j += 1
            if j >= len(lines):
                break
            m = FN.match(lines[j])
            indent, name = m.group(1), m.group(2)
            k = j
            if not lines[j].rstrip().endswith(";"):
                k = j + 1
                while k < len(lines) and not (lines[k].startswith(indent + "}") and not lines[k].startswith(indent + " ")):
                    k += 1
            if not ignored:
                # Only the block attached above the fn names a criterion, never a mention in the body or a
                # string: "see also" and error text must not count as coverage.
                yield name, "\n".join(lines[top:j])
            i = k + 1
        else:
            i += 1


SOL_FN = re.compile(r"^(\s*)function\s+(test\w*)\(")


def sol_tests(path):
    lines = path.read_text().splitlines()
    for i, line in enumerate(lines):
        m = SOL_FN.match(line)
        if not m:
            continue
        top = i
        while top > 0 and re.match(r"\s*(///|//|\*|/\*)", lines[top - 1]):
            top -= 1
        k = i + 1
        while k < len(lines) and not lines[k].startswith(m.group(1) + "}"):
            k += 1
        yield m.group(2), "\n".join(lines[top:i])


MJS_CHECK = re.compile(r"await check\(\s*(['\"`])((?:(?!\1).)*)\1\s*,\s*(['\"`])((?:(?!\3).)*)\3")


def mjs_tests(path):
    for m in MJS_CHECK.finditer(path.read_text()):
        yield m.group(4), m.group(2) + " " + m.group(4)


def scan():
    """criterion -> list of {layer, file, test}"""
    found = {}
    jobs = [
        (["crates/**/*.rs", "services/**/*.rs", "contracts/bezent/**/*.rs", "e2e/network/**/*.rs"], rust_tests),
        (["contracts/evm/test/**/*.sol"], sol_tests),
        (["e2e/**/*.mjs"], mjs_tests),
    ]
    for patterns, fn in jobs:
        for p in files(patterns):
            rel = str(p.relative_to(ROOT))
            layer = layer_of(rel)
            if layer is None:
                continue
            for name, text in fn(p):
                # `name` heads the text for mjs; for Rust the fn name is searched as well.
                named = set(AC.findall(text)) | {f"AC-{a.upper()}{b}.{c}" for a, b, c in AC_IN_NAME.findall(name)}
                for ac in named:
                    runs = next(w for prefix, w in RUNS_IN if rel.startswith(prefix))
                    found.setdefault(ac, []).append({"layer": layer, "file": rel, "test": name, "runs": runs})
    return found


def check(crit, stories, v0, inv_layers, layers, cfg, found):
    """Returns (errors, table). Pure: the tests feed it altered inputs to show each way it fails."""
    exc, no_ac = cfg.get("exception", {}), cfg.get("story_without_criteria", {})
    unmet = cfg.get("known_unmet", {})
    errors = []

    # Fail closed on an empty parse: a reworded SPEC must not turn the check green by hiding every criterion.
    if len(crit) < 10 or len(inv_layers) < 10 or not found:
        errors.append(f"parsed {len(crit)} criteria, {len(inv_layers)} invariants and {len(found)} named criteria "
                      "in tests: the SPEC or the test layout changed shape; fix the parser, do not skip the check")
    for st in sorted(v0):
        if st not in stories or stories[st]["withdrawn"]:
            errors.append(f"SPEC section 15 lists {st} for V0, but section 6 has no live story of that name")
        elif not any(e["story"] == st for e in crit.values()) and st not in no_ac:
            errors.append(f"{st} is a V0 story with no acceptance criterion and is not in "
                          "exceptions.toml [story_without_criteria]")
    for st, e in no_ac.items():
        if st not in v0 or any(c["story"] == st for c in crit.values()):
            errors.append(f"exceptions.toml: [story_without_criteria.{st}] is not a V0 story without criteria: remove it")
        if not e.get("reason"):
            errors.append(f"exceptions.toml: story {st} needs a `reason`")
    for st, e in sorted(stories.items()):
        if not e["withdrawn"] and st not in v0:
            errors.append(f"SPEC section 6 has {st}, live, but section 15 does not list it for V0 (deferred scope?)")

    for ac, entry in sorted(layers.items()):
        if ac not in crit:
            errors.append(f"layers.toml: {ac} is not a V0 criterion of SPEC.md (withdrawn or unknown): remove it")
            continue
        want = entry.get("layers", [])
        if not want or any(l not in LAYERS for l in want):
            errors.append(f"layers.toml: {ac} must name layers among {LAYERS}")
            continue
        if not entry.get("why"):
            errors.append(f"layers.toml: {ac} needs a `why`")
        invs = crit[ac]["invs"]
        if invs:
            # REQ-AR-5: an invariant is not satisfied at a weaker layer. A criterion citing one needs a test at
            # every layer section 9 gives it, unless the entry says, in `narrowed`, why one is not the criterion's.
            need = set().union(*(inv_layers.get(i, set()) for i in invs))
            if set(want) - need:
                errors.append(f"layers.toml: {ac} names {sorted(set(want) - need)} beyond SPEC section 9's {sorted(need)} for {invs}")
            if set(want) != need and not entry.get("narrowed"):
                errors.append(f"layers.toml: {ac} cites {invs}, which section 9 gives {sorted(need)}; "
                              f"list all of them, or say in `narrowed` why {sorted(need - set(want))} is not this criterion's")
    for ac in sorted(crit):
        if ac not in layers:
            errors.append(f"layers.toml: {ac} has no layer entry")
    for ac, entry in sorted(exc.items()):
        if ac not in crit:
            errors.append(f"exceptions.toml: {ac} is not a V0 criterion of SPEC.md: remove the entry")
        if not entry.get("reason") or not entry.get("blocked_by"):
            errors.append(f"exceptions.toml: {ac} needs a `reason` and a `blocked_by`")

    for ac, entry in sorted(unmet.items()):
        if ac not in crit:
            errors.append(f"known_unmet.toml: {ac} is not a V0 criterion of SPEC.md: remove the entry")
        if not entry.get("missing") or not entry.get("closed_by"):
            errors.append(f"known_unmet.toml: {ac} needs `missing` (what is not met) and `closed_by` (the issue or PR that closes it)")
        if ac in exc:
            errors.append(f"{ac} is both an exception and known unmet: an exception means no test, known unmet means a test; keep one")

    table = []
    for ac in sorted(crit):
        want = layers.get(ac, {}).get("layers", [])
        hits = [h for h in found.get(ac, []) if h["layer"] in want]
        wrong = [h for h in found.get(ac, []) if h["layer"] not in want]
        have = sorted({h["layer"] for h in hits})
        missing = [l for l in want if l not in have]
        if ac in exc:
            status = "exception"
            if not missing:
                errors.append(f"exceptions.toml: {ac} has a test at every required layer ({have}): remove the entry")
        elif not hits or missing:
            status = "NO TEST"
            if ac in unmet:
                errors.append(f"known_unmet.toml: {ac} has no test at layer {missing or want}: it belongs in exceptions.toml, not here")
            where = f" (named only at {sorted({h['layer'] for h in wrong})})" if wrong else ""
            errors.append(f"{ac} ({crit[ac]['story']}) has no test at layer {missing or want}{where} and is not an exception")
        else:
            status = "known unmet" if ac in unmet else "covered"
            # A layer can hold tests of different components with different stakes (the EVM vault, the Kreivo
            # contract): `also_under` lists the path prefixes that must each have a test naming the criterion.
            for prefix in layers.get(ac, {}).get("also_under", []):
                if not any(h["file"].startswith(prefix) for h in hits):
                    status = "NO TEST"
                    errors.append(f"{ac} ({crit[ac]['story']}) has no test under {prefix} (layers.toml `also_under`)")
        table.append({"ac": ac, "story": crit[ac]["story"], "layers": want, "status": status,
                      "tests": [f"{h['file']}::{h['test']}" for h in hits],
                      "runs_in": sorted({h["runs"] for h in hits}),
                      "narrowed": layers.get(ac, {}).get("narrowed"),
                      "exception": exc.get(ac), "known_unmet": unmet.get(ac)})

    return errors, table


def main():
    crit, withdrawn, stories = spec_criteria()
    layers = tomllib.loads((ROOT / "e2e/criteria/layers.toml").read_text()).get("layer", {})
    try:
        cfg = load_config()
    except ConfigError as e:
        die([str(e)])
    errors, table = check(crit, stories, v0_stories(), invariant_layers(), layers, cfg, scan())
    if "--json" in sys.argv:
        print(json.dumps(table, indent=1))
    else:
        n = len(table)
        c = sum(t["status"] == "covered" for t in table)
        x = sum(t["status"] == "exception" for t in table)
        u = sum(t["status"] == "known unmet" for t in table)
        lines = [f"### Criteria map: {n} V0 criteria, {c} with a test at their layer, {u} with a test but known "
                 f"not met, {x} excepted, {n - c - x - u} with neither\n", f"Withdrawn, not counted: {', '.join(sorted(set(withdrawn)))}\n",
                 "| Criterion | Layers required | Status | Runs in | Tests (first 3) |", "|---|---|---|---|---|"]
        for t in table:
            tests = "<br>".join(f"`{x}`" for x in t["tests"][:3]) or (
                f"exception: {t['exception']['reason']} (`{t['exception']['blocked_by']}`)" if t["exception"] else "")
            if t["known_unmet"]:
                tests += f"<br>not met: {t['known_unmet']['missing']} (`{t['known_unmet']['closed_by']}`)"
            narrowed = " (narrowed)" if t["narrowed"] else ""
            lines.append(f"| `{t['ac']}` | {'+'.join(t['layers'])}{narrowed} | {t['status']} | "
                         f"{', '.join(t['runs_in'])} | {tests} |")
        print("\n".join(lines))
    if errors:
        die(errors)


if __name__ == "__main__":
    main()
