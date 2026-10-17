#!/usr/bin/env python3
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""SPDX headers on every tracked source file (docs/oss/LICENSING.md).

    tools/oss/spdx.py --check    # CI: fail if a file that can carry a comment has no header,
                                 # or declares a licence other than the one its path requires
    tools/oss/spdx.py --fix      # add the missing headers, and correct wrong ones, in place

Files that cannot carry a comment (JSON, lock files, SVG, patches, checksums) are licensed by
`REUSE.toml` instead; `reuse lint` checks those, and this script skips them.

The licence a path requires is `policy()` below, and docs/oss/LICENSING.md says why:

- the services, the ink! contract and the store are AGPL-3.0-or-later (`AGPL_PREFIXES`);
- prose (Markdown, HTML) is CC-BY-4.0, wherever it lives;
- everything else (the reusable crates, the EVM contracts, the test bed, tooling) is Apache-2.0 OR MIT.

It also checks that every Cargo.toml declares the licence its path requires.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
from pathlib import Path

# REUSE-IgnoreStart
HOLDER = "Pagos Cube LLC"  # Bloque's operating company (docs/oss/LICENSING.md §6)
YEAR = "2026"
COPYRIGHT = f"Copyright (c) {YEAR} {HOLDER}"

AGPL = "AGPL-3.0-or-later"
PERMISSIVE = "Apache-2.0 OR MIT"
PROSE = "CC-BY-4.0"

# How a file type writes a comment: a line prefix, or a block (open, close).
LINE = {
    ".rs": "//", ".ts": "//", ".js": "//", ".mjs": "//",
    ".sh": "#", ".py": "#", ".toml": "#", ".yml": "#", ".yaml": "#",
    ".sql": "--", ".lua": "--", ".sol": "//",
}
BLOCK = {
    ".md": ("<!--", "-->"), ".html": ("<!--", "-->"),
    ".css": ("/*", "*/"), ".ld": ("/*", "*/"),
}
# Whole file names that take `#` comments.
NAMED_HASH = {"Dockerfile", ".gitignore", ".dockerignore", "env.example", "CODEOWNERS"}
# Paths a header must not be written into: the file's bytes are read by a tool that would break,
# or the file is a third party's. `REUSE.toml` covers them.
SKIP_PREFIXES = (
    "LICENSES/",
    # The store's migrations: sqlx records each file's checksum and refuses to start on a database
    # that applied a file whose bytes changed, so a header added here would break every deployment
    # that applied it. Covered by REUSE.toml.
    "crates/store/migrations/",
    # trybuild's expected output names lines of these files (`tests/ui/*.stderr`); a second header
    # line would move them. They keep their one-line SPDX tag; REUSE.toml adds the copyright.
    "crates/core/tests/ui/",
)
SKIP_FILES = {
    "LICENSE", "LICENSE-APACHE", "LICENSE-MIT", "REUSE.toml",
    # Their YAML front matter must open the file: tools/md2artifact.py reads it. Covered by REUSE.toml.
    "SPEC.md", "PLAN.md", "OVERVIEW.md",
}

HEAD_LINES = 12  # a header must start within the first lines of the file


# Services, the ink! contract and the store (its migrations carry an AGPL header and may not be edited:
# docs/oss/LICENSING.md §3): AGPL-3.0-or-later.
AGPL_PREFIXES = ("services/", "contracts/bezent/", "crates/store/")


def code_policy(path: str) -> str:
    return AGPL if path.startswith(AGPL_PREFIXES) else PERMISSIVE


def policy(path: str) -> str:
    if Path(path).suffix in (".md", ".html"):
        return PROSE
    return code_policy(path)


def manifest_licence(root: Path, path: str) -> str | None:
    """The licence a Cargo.toml declares, following `license.workspace = true` to its workspace."""
    import tomllib

    data = tomllib.loads((root / path).read_text(encoding="utf-8"))
    package = data.get("package")
    if package is None:
        return code_policy(path)  # a virtual workspace: it declares no licence of its own
    lic = package.get("license")
    if isinstance(lic, dict) and lic.get("workspace"):
        here = (root / path).parent.parent
        while here != root.parent:
            ws = here / "Cargo.toml"
            if ws.is_file():
                wdata = tomllib.loads(ws.read_text(encoding="utf-8"))
                if "workspace" in wdata:
                    return wdata["workspace"].get("package", {}).get("license")
            here = here.parent
        return None
    return lic


def style(path: str):
    name = Path(path).name
    suffix = Path(path).suffix
    if name in NAMED_HASH or (name.endswith(".dockerignore")):
        return ("line", "#")
    if name.endswith(".Dockerfile"):
        return ("line", "#")
    if suffix in LINE:
        return ("line", LINE[suffix])
    if suffix in BLOCK:
        return ("block", BLOCK[suffix])
    return None


def tracked(root: Path) -> list[str]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=root, check=True, capture_output=True)
    return [p for p in out.stdout.decode().split("\0") if p]


def header(kind, mark, licence: str) -> list[str]:
    if kind == "line":
        return [f"{mark} {COPYRIGHT}", f"{mark} SPDX-License-Identifier: {licence}"]
    open_, close = mark
    if open_ == "<!--":
        return [open_, COPYRIGHT, f"SPDX-License-Identifier: {licence}", close]
    return [f"{open_} {COPYRIGHT} */", f"{open_} SPDX-License-Identifier: {licence} {close}"]


def declared(text: str) -> str | None:
    for line in text.splitlines()[:HEAD_LINES]:
        if "SPDX-License-Identifier:" in line:
            value = line.split("SPDX-License-Identifier:", 1)[1]
            for end in ("-->", "*/"):
                value = value.replace(end, "")
            return value.strip()
    return None


def has_copyright(text: str) -> bool:
    for line in text.splitlines()[:HEAD_LINES]:
        if "Copyright" in line or "SPDX-FileCopyrightText:" in line:
            return True
    return False


def insert(text: str, lines: list[str], kind: str) -> str:
    body = text.splitlines(keepends=True)
    at = 0
    # Keep a shebang, and a Python encoding line, first.
    if body and body[0].startswith("#!"):
        at = 1
    if at < len(body) and body[at].startswith("# -*- coding"):
        at += 1
    block = "".join(line + "\n" for line in lines)
    # One blank line after a Markdown header keeps the first heading a heading.
    if kind == "block" and lines[0] == "<!--":
        block += "\n"
    return "".join(body[:at]) + block + "".join(body[at:])


def rewrite(text: str, licence: str) -> str:
    """Correct the licence of an existing header, and add the copyright line it lacks."""
    lines = text.splitlines(keepends=True)
    for i, line in enumerate(lines[:HEAD_LINES]):
        if "SPDX-License-Identifier:" not in line:
            continue
        head, value = line.split("SPDX-License-Identifier:", 1)
        close = ""
        for end in ("-->", "*/"):
            if end in value:
                close = " " + end
        if has_copyright(text):
            lines[i] = f"{head}SPDX-License-Identifier: {licence}{close}\n"
        elif head.strip() == "<!--":
            lines[i] = f"<!--\n{COPYRIGHT}\nSPDX-License-Identifier: {licence}\n-->\n"
        elif head.strip() == "" and close:
            lines[i] = f"{head}SPDX-License-Identifier: {licence}{close}\n"
        else:
            lines[i] = f"{head}{COPYRIGHT}{close}\n{head}SPDX-License-Identifier: {licence}{close}\n"
        break
    return "".join(lines)


# REUSE-IgnoreEnd


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--fix", action="store_true")
    args = ap.parse_args()

    root = Path(subprocess.run(["git", "rev-parse", "--show-toplevel"], check=True,
                               capture_output=True, text=True).stdout.strip())
    missing, wrong, fixed = [], [], 0
    for path in tracked(root):
        if path in SKIP_FILES or path.startswith(SKIP_PREFIXES):
            continue
        # Pages rendered from the Markdown by tools/md2artifact.py: covered by REUSE.toml.
        if path.startswith("docs/") and path.endswith(".html"):
            continue
        s = style(path)
        if s is None:
            continue
        file = root / path
        if not file.is_file() or file.is_symlink():
            continue
        text = file.read_text(encoding="utf-8")
        want = policy(path)
        have = declared(text)
        if have is None:
            if args.fix:
                lines = header(s[0], s[1], want)
                if has_copyright(text):
                    lines = [line for line in lines if COPYRIGHT not in line] or lines
                file.write_text(insert(text, lines, s[0]), encoding="utf-8")
                fixed += 1
            else:
                missing.append(path)
        elif have != want or not has_copyright(text):
            if args.fix:
                file.write_text(rewrite(text, want), encoding="utf-8")
                fixed += 1
            elif have != want:
                wrong.append(f"{path}: declares {have!r}, its path requires {want!r}")
            else:
                missing.append(f"{path} (no copyright line)")
        if Path(path).name == "Cargo.toml":
            lic = manifest_licence(root, path)
            if lic != code_policy(path):
                wrong.append(f"{path}: Cargo `license` is {lic!r}, its path requires {code_policy(path)!r}")

    if args.fix:
        print(f"spdx: headers added to {fixed} files")
    for p in missing:
        print(f"spdx: missing header: {p}", file=sys.stderr)
    for w in wrong:
        print(f"spdx: wrong licence: {w}", file=sys.stderr)
    if missing or wrong:
        print("spdx: run `tools/oss/spdx.py --fix`, and see docs/oss/LICENSING.md", file=sys.stderr)
        return 1
    if args.check:
        print("spdx: every tracked source file carries its header")
    return 0


if __name__ == "__main__":
    sys.exit(main())
