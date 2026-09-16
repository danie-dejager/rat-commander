#!/usr/bin/env python3
"""Vendor the 010 Editor Binary Template repository into this directory.

The hex editor's binary templates come from SweetScape's public repository:

    https://www.sweetscape.com/010editor/repository/templates/

whose terms (https://www.sweetscape.com/companyinfo/terms.html) state: "By
submitting a script or template to the repository, you agree to release your
file into the public domain. Other people may download your file and use it
for any purpose, commercial or otherwise."

Not every file in the repository is vendored:

    Syntax, Inspector   categories that are not binary parsers (text syntax
                        highlighters and 010 Editor Inspector customisations)
    MDS.bt              carries a GPLv3 header, incompatible with this
                        GPL-2.0-only program
    #include misses     templates that include files the repository lacks

Every file is decoded (UTF-8, else the encoding named in ENCODINGS, else
Windows-1252) and written back as UTF-8 with LF line endings, so the internal
editor can open it and the bundle is identical on every OS. README.md is
regenerated with the provenance and a per-file table.

build.rs packs every *.bt here into the binary; the program deploys them into
~/.config/rat-commander/templates/ on start.

Run from anywhere; downloads are cached in BT_CACHE (default
/tmp/bt-repository), so a rerun only rewrites this directory.

    python3 fetch_templates.py
"""

import datetime
import html
import os
import re
import sys
import time
import urllib.request

BASE = "https://www.sweetscape.com/010editor/repository/"
LISTING = BASE + "templates/"
CACHE = os.environ.get("BT_CACHE", "/tmp/bt-repository")
HERE = os.path.dirname(os.path.abspath(__file__))

SKIP_CATEGORIES = {"Syntax", "Inspector"}
SKIP_FILES = {
    "MDS.bt": "GPLv3 header (incompatible with GPL-2.0-only)",
}
# Files that are not UTF-8 and whose non-ASCII text is not Windows-1252.
ENCODINGS = {}

LICENSE_WORDS = re.compile(
    r"\b(GPL|General Public License|MIT|BSD|Apache License|Copyright\s*(?:\(c\)|\d{4})"
    r"|COPYRIGHT NOTE|License\s*:|public domain|No Copyright)",
    re.I,
)


def fetch(url, dest):
    if os.path.exists(dest) and os.path.getsize(dest) > 0:
        with open(dest, "rb") as f:
            return f.read()
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    req = urllib.request.Request(url, headers={"User-Agent": "rat-commander fetch_templates.py"})
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                data = r.read()
            break
        except Exception as e:  # noqa: BLE001 - retry any transient failure
            if attempt == 3:
                raise
            print(f"  retry {url}: {e}", file=sys.stderr)
            time.sleep(2 + attempt * 3)
    with open(dest, "wb") as f:
        f.write(data)
    time.sleep(0.2)
    return data


def parse_listing(page):
    """(category, file name, description) for every template row."""
    rows = []
    category = None
    for m in re.finditer(
        r'class="rep-heading"[^>]*><i>([^<]+)</i>'
        r'|<a href="file_info\.php\?file=([^&"]+)&amp;type=0[^"]*">[^<]*</a></td>\s*'
        r'<td valign="top">(.*?)</td>'
        r'|<a href="file_info\.php\?file=([^&"]+)&type=0[^"]*">[^<]*</a></td>\s*'
        r'<td valign="top">(.*?)</td>',
        page,
        re.S,
    ):
        if m.group(1):
            category = html.unescape(m.group(1)).strip()
            continue
        name = m.group(2) or m.group(4)
        desc = m.group(3) if m.group(2) else m.group(5)
        desc = re.sub(r"<[^>]+>", " ", desc or "")
        desc = " ".join(html.unescape(desc).split())
        if name and name.lower().endswith(".bt"):
            rows.append((category, html.unescape(name), desc))
    return rows


def decode(name, raw):
    if raw.startswith(b"\xef\xbb\xbf"):
        raw = raw[3:]
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = raw.decode(ENCODINGS.get(name, "cp1252"), errors="replace")
    return text.replace("\r\n", "\n").replace("\r", "\n")


def header_field(text, key):
    m = re.search(r"^//\s*" + key + r"\s*:[ \t]*(.*)$", text, re.M)
    return m.group(1).strip() if m else ""


def includes(text):
    return re.findall(r'^\s*#\s*include\s*[<"]([^>"]+)[>"]', text, re.M)


def main():
    page = fetch(LISTING, os.path.join(CACHE, "listing.html")).decode("utf-8", "replace")
    rows = parse_listing(page)
    if len(rows) < 200:
        sys.exit(f"only {len(rows)} templates found on the listing page; has its layout changed?")

    texts = {}
    skipped = []
    for category, name, desc in rows:
        if category in SKIP_CATEGORIES:
            skipped.append((name, f"category {category}"))
            continue
        if name in SKIP_FILES:
            skipped.append((name, SKIP_FILES[name]))
            continue
        print(f"{category:>18}  {name}")
        raw = fetch(BASE + "files/" + name, os.path.join(CACHE, "files", name))
        texts[name] = (category, desc, decode(name, raw))

    # Drop templates whose includes the vendored set can't satisfy (repeat, in
    # case a dropped file was itself an include).
    lower = {}
    changed = True
    while changed:
        changed = False
        lower = {n.lower(): n for n in texts}
        for name, (_, _, text) in list(texts.items()):
            missing = [i for i in includes(text) if os.path.basename(i).lower() not in lower]
            if missing:
                skipped.append((name, "includes missing " + ", ".join(missing)))
                del texts[name]
                changed = True

    for old in os.listdir(HERE):
        if old.lower().endswith(".bt"):
            os.remove(os.path.join(HERE, old))
    for name, (_, _, text) in sorted(texts.items()):
        with open(os.path.join(HERE, name), "w", encoding="utf-8", newline="\n") as f:
            f.write(text if text.endswith("\n") else text + "\n")

    write_readme(texts, skipped)
    print(f"\n{len(texts)} templates written, {len(skipped)} skipped")


def write_readme(texts, skipped):
    today = datetime.date.today().isoformat()
    out = []
    out.append("# Binary templates\n")
    out.append(
        "These 010 Editor Binary Templates are vendored from SweetScape's public\n"
        f"[template repository]({LISTING}) (fetched {today} by `fetch_templates.py`).\n"
        "`build.rs` packs every `*.bt` here into the program, which deploys them to\n"
        "`~/.config/rat-commander/templates/`.\n"
    )
    out.append("## License\n")
    out.append(
        "The repository's [terms](https://www.sweetscape.com/companyinfo/terms.html) state:\n\n"
        "> By submitting a script or template to the repository, you agree to release your\n"
        "> file into the public domain. Other people may download your file and use it for\n"
        "> any purpose, commercial or otherwise.\n\n"
        "A few templates carry their own license or attribution notes in their headers,\n"
        "which are kept intact; those files are marked in the table below. Not vendored:\n"
    )
    for name, why in sorted(skipped):
        out.append(f"- `{name}` — {why}")
    out.append("\n## Templates\n")
    out.append("| File | Category | Version | Authors | Purpose | Notes |")
    out.append("|---|---|---|---|---|---|")
    for name, (category, desc, text) in sorted(texts.items(), key=lambda kv: (kv[1][0], kv[0].lower())):
        head = "\n".join(text.split("\n")[:60])
        comments = "\n".join(l for l in head.split("\n") if l.lstrip().startswith("//"))
        authors = header_field(head, "Authors?")
        version = header_field(head, "Version")
        purpose = header_field(head, "Purpose") or desc
        notes = ""
        lic = sorted({" ".join(m.group(1).split()) for m in LICENSE_WORDS.finditer(comments)}, key=str.lower)
        if lic:
            notes = "header: " + ", ".join(lic)
        cell = lambda s: s.replace("|", "\\|")  # noqa: E731
        out.append(
            f"| {cell(name)} | {cell(category)} | {cell(version)} | {cell(authors)} | {cell(purpose)} | {notes} |"
        )
    with open(os.path.join(HERE, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
