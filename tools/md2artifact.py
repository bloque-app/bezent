#!/usr/bin/env python3
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Render a spec-repo Markdown file (SPEC.md, OVERVIEW.md, PLAN.md) as an Artifact page.

Usage: tools/md2artifact.py <input.md> <output.html> "<Page title>" [--standalone]

--standalone: write a full document (doctype, head) for pages published next to another artifact.

The page is a faithful render for reviewers: front matter as a header, a contents rail,
an anchor on every identifier where it is defined (REQ-*, INV-*, US-*, AC-*, ...), and a link
from every other mention to that anchor. Mermaid fences become <pre class="mermaid">, which the
Artifact viewer renders natively. Re-run after every change to the source and republish.
"""
import html, re, subprocess, sys
from pathlib import Path

src, out, title = Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3]
here = Path(__file__).resolve().parent
repo = here.parent
md = src.read_text()

# Front matter -> header fields
fm = {}
m = re.match(r"^---\n(.*?)\n---\n", md, re.S)
if m:
    for line in m.group(1).splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            fm[k.strip()] = v.strip()
    md = md[m.end():]

tpl = here / ".md2artifact.tpl"
tpl.write_text("$toc$\n<!--SPLIT-->\n$body$\n")
body = subprocess.run(
    ["pandoc", "-f", "gfm", "-t", "html5", "--toc", "--toc-depth=2", "--template", str(tpl),
     "--lua-filter", str(here / "mermaid.lua"), "--metadata", "title=x"],
    input=md, capture_output=True, text=True, check=True).stdout
tpl.unlink()
toc, body = body.split("<!--SPLIT-->", 1)

ID = r"(?:REQ-[A-Z]+-\d+|INV-\d+|ERR-[A-Za-z]+|CND-[A-Za-z]+|NFR-\d+|OQ-\d+|DEF-\d+|US-[A-Z]\d+|AC-[A-Z]\d+\.\d+|DEC-\d+|F-\d{3}|CTR-[A-Z]+|BR-[A-Z]+-\d+)"
defined = set()

def first(i):
    if i in defined:
        return False
    defined.add(i)
    return True

# Definitions: **`REQ-…`** (requirements, stories), the first cell of a table row (`INV-1` | …),
# and a list item that opens with an identifier (acceptance criteria).
body = re.sub(r'<strong><code>(' + ID + r')</code></strong>',
              lambda mo: f'<strong id="{mo.group(1)}" class="def"><code>{mo.group(1)}</code></strong>' if first(mo.group(1)) else mo.group(0), body)
body = re.sub(r'<tr>(\s*)<td><code>(' + ID + r')</code>',
              lambda mo: f'<tr>{mo.group(1)}<td id="{mo.group(2)}" class="def"><code>{mo.group(2)}</code>' if first(mo.group(2)) else mo.group(0), body)
body = re.sub(r'<li><code>(' + ID + r')</code>',
              lambda mo: f'<li id="{mo.group(1)}" class="def"><code>{mo.group(1)}</code>' if first(mo.group(1)) else mo.group(0), body)

# Every other mention links to its definition.
def link(mo):
    i = mo.group(1)
    return f'<a class="ref" href="#{i}"><code>{i}</code></a>' if i in defined else mo.group(0)
parts = re.split(r'(<strong id="[^"]+" class="def"><code>[^<]+</code></strong>|<td id="[^"]+" class="def"><code>[^<]+</code>|<li id="[^"]+" class="def"><code>[^<]+</code>)', body)
body = "".join(p if p.startswith(("<strong id=", "<td id=", "<li id=")) else re.sub(r'<code>(' + ID + r')</code>', link, p) for p in parts)

body = re.sub(r"<table>", '<div class="scroll"><table>', body).replace("</table>", "</table></div>")
body = re.sub(r"<pre>(?!<code)", '<pre class="block">', body)

try:
    commit = subprocess.run(["git", "-C", str(repo), "log", "-1", "--format=%h %cs", "--", src.name],
                            capture_output=True, text=True).stdout.strip() or "uncommitted"
    dirty = subprocess.run(["git", "-C", str(repo), "status", "--porcelain", "--", src.name],
                           capture_output=True, text=True).stdout.strip()
    if dirty: commit += " + local changes"
except Exception:
    commit = "unknown"

meta_keys = [("spec_version", "Version"), ("plan_version", "Version"), ("edition", "Edition"), ("status", "Status"),
             ("date", "Date"), ("digest_of", "Digest of")]
meta = "".join(f'<div><dt>{lab}</dt><dd>{html.escape(fm[k])}</dd></div>' for k, lab in meta_keys if k in fm)
meta += f'<div><dt>Source</dt><dd><code>bezent/{src.name}</code>' + ('' if commit == 'uncommitted' else f' · {html.escape(commit)}') + '</dd></div>'
summary = f'<p class="summary">{html.escape(fm["summary"])}</p>' if "summary" in fm else ""

def _svg(name, cls):
    try:
        svg = (repo / "brand" / name).read_text()
        return svg.replace("<svg ", f'<svg class="brand {cls}" role="img" aria-label="bezent by bloque" ', 1)
    except FileNotFoundError:
        return ""
brand_light, brand_dark = _svg("lockup.svg", "brand-light"), _svg("lockup-dark.svg", "brand-dark")

page = f"""<title>{html.escape(title)}</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Familjen+Grotesk:wght@500;600;700&family=Geist:wght@400;500;600&family=Geist+Mono:wght@400;500&display=swap">
<style>
/* bezent brand (Bloque open-source family): sky symbol, ink on paper, violet full stop; Familjen Grotesk,
   Geist, Geist Mono. Layout: a contents rail beside one reading column (≤ 74ch); the rail folds on phones. */
:root{{
  --paper:#fbfaff; --sheet:#ffffff; --ink:#0d0c17; --muted:#6d6982; --rule:#e4e1ef;
  --sky:#1f7bff; --accent:#0b5fd6; --accent-soft:#e8f0ff; --code:#f2f0fa; --stop:#6a3cff;
  --f-display:"Familjen Grotesk","Helvetica Neue",Helvetica,Arial,sans-serif;
  --f-body:"Geist",-apple-system,"Segoe UI",Helvetica,Arial,sans-serif;
  --f-mono:"Geist Mono",ui-monospace,SFMono-Regular,Menlo,monospace;
}}
@media (prefers-color-scheme:dark){{:root:not([data-theme="light"]){{
  --paper:#0d0c17; --sheet:#16141f; --ink:#f3f2f8; --muted:#9a96ad; --rule:#2a2738;
  --sky:#1f7bff; --accent:#7db3ff; --accent-soft:#17233b; --code:#1c1a28; --stop:#a78bfa; color-scheme:dark}}}}
:root[data-theme="dark"]{{
  --paper:#0d0c17; --sheet:#16141f; --ink:#f3f2f8; --muted:#9a96ad; --rule:#2a2738;
  --sky:#1f7bff; --accent:#7db3ff; --accent-soft:#17233b; --code:#1c1a28; --stop:#a78bfa; color-scheme:dark}}
.brand{{display:block;height:34px;width:auto;margin-bottom:14px}}.brand-dark{{display:none}}
@media (prefers-color-scheme:dark){{:root:not([data-theme="light"]) .brand-light{{display:none}}:root:not([data-theme="light"]) .brand-dark{{display:block}}}}
:root[data-theme="dark"] .brand-light{{display:none}}:root[data-theme="dark"] .brand-dark{{display:block}}
*{{box-sizing:border-box}}
html{{scroll-padding-top:calc(env(safe-area-inset-top,0px) + 12px)}}
body{{background:var(--paper);color:var(--ink);font:16px/1.6 var(--f-body)}}
.page{{max-width:1180px;margin:0 auto;padding-inline:16px;padding-block:28px 80px;display:grid;grid-template-columns:250px minmax(0,1fr);gap:40px}}
@media (max-width:900px){{.page{{grid-template-columns:minmax(0,1fr);gap:16px}}}}
.rail{{position:sticky;top:calc(env(safe-area-inset-top,0px) + 16px);align-self:start;max-height:calc(100vh - 32px);overflow:auto;font-size:13.5px;line-height:1.45}}
@media (max-width:900px){{.rail{{position:static;max-height:none}}}}
.rail summary{{font:600 12px/1 var(--f-body);text-transform:uppercase;letter-spacing:.08em;color:var(--muted);cursor:pointer;padding:6px 0}}
.rail ul{{list-style:none;margin:0;padding-left:0}}.rail ul ul{{padding-left:12px}}
.rail li{{margin:3px 0}}.rail a{{color:var(--muted);text-decoration:none}}.rail a:hover{{color:var(--accent)}}
main{{min-width:0;max-width:74ch}}
header.doc{{border-bottom:1px solid var(--rule);padding-bottom:18px;margin-bottom:8px}}
header.doc .kicker{{font:600 12px/1 var(--f-body);text-transform:uppercase;letter-spacing:.1em;color:var(--accent)}}
h1,h2,h3,h4{{font-family:var(--f-display);font-weight:600;text-wrap:balance;line-height:1.2}}
header.doc h1{{font-size:38px;margin:10px 0 8px}}
.summary{{font-size:17px;color:var(--muted);margin:0 0 14px}}
dl.meta{{display:flex;flex-wrap:wrap;gap:6px 22px;margin:0;font-size:13px}}
dl.meta div{{display:flex;gap:6px}}dl.meta dt{{color:var(--muted)}}dl.meta dd{{margin:0;font-weight:500}}
main h1{{font-size:30px;margin:40px 0 12px}}
main h2{{font-size:25px;margin:44px 0 12px;padding-top:14px;border-top:1px solid var(--rule)}}
main h3{{font-size:20px;margin:30px 0 8px}}main h4{{font-size:17px;margin:22px 0 6px}}
p,li{{overflow-wrap:anywhere}}
a{{color:var(--accent)}}
code{{font-family:var(--f-mono);font-size:.86em;background:var(--code);padding:1px 4px;border-radius:4px}}
a.ref{{text-decoration:none}}a.ref code{{background:var(--accent-soft);color:var(--accent)}}
.def{{scroll-margin-top:16px}}.def code{{font-weight:600}}
.def:target code, tr:has(td.def:target){{outline:2px solid var(--accent);outline-offset:2px}}
blockquote{{margin:16px 0;padding:12px 18px;border:1px solid var(--rule);border-radius:20px;background:var(--sheet)}}
blockquote p{{margin:6px 0}}
.scroll{{overflow-x:auto;margin:12px 0 18px}}
table{{border-collapse:collapse;width:100%;font-size:14px;background:var(--sheet)}}
th,td{{border:1px solid var(--rule);padding:7px 9px;text-align:left;vertical-align:top}}
th{{font-weight:600;background:var(--code)}}
td code{{white-space:nowrap}}
pre.block{{overflow-x:auto;background:var(--code);padding:14px 16px;border-radius:14px;font:13px/1.5 var(--f-mono)}}
pre.block code{{background:none;padding:0}}
pre.mermaid{{background:var(--sheet);border:1px solid var(--rule);border-radius:20px;padding:12px;overflow-x:auto;text-align:center}}
hr{{border:0;border-top:1px solid var(--rule);margin:28px 0}}
:focus-visible{{outline:2px solid var(--accent);outline-offset:2px}}
</style>
<div class="page">
  <nav class="rail" aria-label="Contents"><details open><summary>Contents</summary>{toc}</details></nav>
  <main>
    <header class="doc">
      {brand_light}{brand_dark}
      <div class="kicker">{html.escape(src.name)}</div>
      <h1>{html.escape(fm.get("name", title).replace("— design overview", "").strip() if src.name != "SPEC.md" else title)}</h1>
      {summary}
      <dl class="meta">{meta}</dl>
    </header>
    {body}
  </main>
</div>
<script>
// Fold the contents rail on phones so the text comes first.
try {{ if (matchMedia("(max-width: 900px)").matches) document.querySelector(".rail details").open = false; }} catch (e) {{}}
</script>
"""
if "--standalone" in sys.argv[4:]:
    # A page published next to another artifact (e.g. the decision form) is served as written, with no
    # skeleton, so it needs its own doctype, charset and viewport.
    head, body_part = page.split('<div class="page">', 1)
    page = ('<!doctype html>\n<html lang="en">\n<head>\n<meta charset="utf-8">\n'
            '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">\n'
            + head + '</head>\n<body>\n<div class="page">' + body_part + '</body>\n</html>\n')
out.parent.mkdir(parents=True, exist_ok=True)
out.write_text(page)
print(f"wrote {out} · {len(defined)} identifiers anchored · {len(page)//1024} KB")
