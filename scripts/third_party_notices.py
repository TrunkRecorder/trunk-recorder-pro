#!/usr/bin/env python3
"""THIRD-PARTY-NOTICES.txt for a release: every Rust crate linked into
trunk-lite (any target) and the npm packages bundled into the web interface,
each with its license text(s) from its own sources.

    python3 scripts/third_party_notices.py > THIRD-PARTY-NOTICES.txt

Needs the crates downloaded (any `cargo build` / `cargo fetch`) and
web/node_modules (`npm ci`)."""

import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LICENSE_NAMES = ("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE", "COPYRIGHT")
# The web interface's runtime dependencies (what Vite bundles into dist/).
NPM = ["react", "react-dom", "scheduler"]


def license_files(d: pathlib.Path):
    return sorted(p for p in d.iterdir() if p.is_file() and p.name.upper().startswith(LICENSE_NAMES))


def main() -> None:
    tree = subprocess.run(
        ["cargo", "tree", "-p", "trunk-lite", "-e", "normal", "--prefix", "none", "-f", "{p}", "--target", "all"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    wanted = set()
    for line in tree.splitlines():
        parts = line.split()
        if len(parts) >= 2 and not parts[0].startswith("trunk-"):
            wanted.add((parts[0], parts[1].lstrip("v")))
    meta = json.loads(subprocess.run(["cargo", "metadata", "--format-version", "1"], cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    pkgs = {(p["name"], p["version"]): p for p in meta["packages"]}

    out = [
        "Trunk Recorder Lite — third-party notices",
        "",
        "Trunk Recorder Lite is licensed under the GNU General Public License v3.0 or",
        "later (LICENSE). It contains code derived from op25 (boatbod/op25, GPLv3),",
        "Trunk Recorder (GPLv3) and mbelib (ISC; notice in",
        "crates/trunk-core/src/mbe and archive/ts-engine/src/vendor/ff/mbe/tables.ts).",
        "IMBE and AMBE+2 are vocoder technologies of Digital Voice Systems, Inc.",
        "",
        "It also includes the following open-source components.",
        "",
    ]
    missing = []
    for name, version in sorted(wanted):
        p = pkgs.get((name, version))
        lic = (p or {}).get("license") or "see files"
        out += ["=" * 78, f"{name} {version} — {lic}", (p or {}).get("repository") or "", "=" * 78, ""]
        files = license_files(pathlib.Path(p["manifest_path"]).parent) if p else []
        if not files:
            missing.append(f"{name} {version}")
            out += [f"(No license file in the crate; its manifest declares: {lic}.)", ""]
        for f in files:
            out += [f"--- {f.name} ---", f.read_text(errors="replace").rstrip(), ""]
    for name in NPM:
        d = ROOT / "web" / "node_modules" / name
        pj = json.loads((d / "package.json").read_text())
        out += ["=" * 78, f"{name} {pj['version']} (npm) — {pj.get('license', '')}", "=" * 78, ""]
        for f in license_files(d):
            out += [f"--- {f.name} ---", f.read_text(errors="replace").rstrip(), ""]
    sys.stdout.write("\n".join(out) + "\n")
    if missing:
        print(f"note: no license file shipped in {len(missing)} crate(s): {', '.join(missing)}", file=sys.stderr)


main()
