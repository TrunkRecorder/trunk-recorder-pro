#!/usr/bin/env python3
"""CPU cost gate: instructions each stage of trunk-pro takes on synthetic IQ.

    perf/gate.py [--bin target/release/trunk-pro]      against perf/baseline.json
    perf/gate.py --against OLD_BIN                      against another build
    perf/gate.py --update                               write perf/baseline.json
    perf/gate.py count --to FILE --name NAME -- CMD...  (regress.sh: one command)

The captures are made by the binary's own synthesizers (tool p25synth,
dmrsynth, nxdnsynth), so nothing outside the repository is needed. Each
stage is a whole process — a radio's read path (`tool perf`) or a replay
through the engine — and what's measured is the instructions it retires:
unlike time, that doesn't move with the clock speed, turbo, heat or what
else the computer is doing.

Counters (--counter, default auto):
  cachegrind  valgrind's count: exact and the same every run. CI uses it
              (hosted runners have no hardware counters); perf/baseline.json
              holds its numbers, for x86-64.
  perf        Linux hardware counters (`perf stat -e instructions:u`).
  time        macOS: `/usr/bin/time -l`'s "instructions retired".
Hardware counts vary ~0.2% run to run: each is the median of --runs (3).
A baseline compares only with the same counter on the same architecture;
locally, compare two builds with --against.

A stage more than --max percent (2) above its baseline fails the gate. A
change that adds CPU on purpose updates perf/baseline.json (--update, or
the numbers from CI's run), so the increase shows in the diff.
"""

import argparse
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
BASELINE = os.path.join(HERE, "baseline.json")

# The synthetic captures: (file, the tool that writes it).
SYNTH = [
    ("p25.cu8", ["tool", "p25synth", "{f}", "--center", "851000000", "--freq", "851100000", "--superframes", "20"]),
    ("dmr.cu8", ["tool", "dmrsynth", "{f}", "--center", "464000000", "--freq", "464100000"]),
    ("nxdn.cu8", ["tool", "nxdnsynth", "{f}", "--center", "451000000", "--kind", "typeC"]),
]

# (name, capture, command, per pass). A per-pass stage is counted at
# --passes 3 and 1: half the difference is one pass, without start-up and loading.
STAGES = [
    ("radio-rtl", "p25.cu8", ["tool", "perf", "rtl", "{f}"], True),
    ("radio-hackrf", "p25.cu8", ["tool", "perf", "hackrf", "{f}"], True),
    ("radio-airspy", "p25.cu8", ["tool", "perf", "airspy", "{f}"], True),
    ("p25-conventional", "p25.cu8", ["replay", "{f}", "--center", "851000000", "--rate", "2400000", "--p25", "851100000", "--out", "{out}"], False),
    ("dmr-conventional", "dmr.cu8", ["replay", "{f}", "--center", "464000000", "--rate", "2400000", "--dmr", "464100000", "--out", "{out}"], False),
    ("nxdn-trunked", "nxdn.cu8", ["replay", "{f}", "--center", "451000000", "--rate", "2400000", "--cc", "451018750", "--nxdn-trunk", "typeC", "--lcn", "12=451068750", "--out", "{out}"], False),
]


def counted(cmd, counter, **popen):
    """Run cmd; (exit code, instructions or None)."""
    if counter == "none":
        return subprocess.call(cmd, **popen), None
    fd, rep = tempfile.mkstemp(prefix="perf-count-")
    os.close(fd)
    try:
        if counter == "time":
            full, pat = ["/usr/bin/time", "-l", "-o", rep] + cmd, r"(\d+)\s+instructions retired"
        elif counter == "perf":
            full, pat = ["perf", "stat", "-x,", "-e", "instructions:u", "-o", rep, "--"] + cmd, r"^(\d+),[^,]*,instructions"
        elif counter == "cachegrind":
            full, pat = ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null", "--log-file=" + rep] + cmd, r"I\s+refs:\s+([\d,]+)"
        else:
            sys.exit(f"--counter {counter}: cachegrind, perf, time or none")
        rc = subprocess.call(full, **popen)
        with open(rep) as f:
            m = re.search(pat, f.read(), re.M)
        return rc, int(m.group(1).replace(",", "")) if m else None
    finally:
        os.unlink(rep)


def native_counter():
    """This computer's hardware counter, or "none"."""
    for c in (["time"] if sys.platform == "darwin" else ["perf"] if sys.platform.startswith("linux") else []):
        try:
            rc, n = counted(["true"], c, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if rc == 0 and n:
                return c
        except OSError:
            pass
    return "none"


def label(counter):
    return f"{counter}-{platform.machine()}"


class Runner:
    def __init__(self, counter, runs, work):
        self.counter, self.runs, self.work = counter, runs, work

    def once(self, bin_, args, log):
        out = os.path.join(self.work, "out")
        shutil.rmtree(out, ignore_errors=True)
        cmd = [bin_] + [a.replace("{out}", out) for a in args]
        with open(log, "w") as lf:
            rc, n = counted(cmd, self.counter, stdout=lf, stderr=subprocess.STDOUT)
        if rc != 0 or n is None:
            with open(log) as lf:
                tail = lf.read()[-1500:]
            raise RuntimeError(f"{' '.join(cmd)}: exit {rc}\n{tail}")
        return n

    def count(self, bin_, args, log):
        runs = 1 if self.counter == "cachegrind" else self.runs
        return int(statistics.median(self.once(bin_, args, log) for _ in range(runs)))

    def stage(self, bin_, args, per_pass, log):
        if not per_pass:
            return self.count(bin_, args, log)
        return (self.count(bin_, args + ["--passes", "3"], log) - self.count(bin_, args + ["--passes", "1"], log)) // 2


def measure(bin_, runner, captures, only):
    stages = {}
    for name, cap, args, per_pass in STAGES:
        if only and name not in only:
            continue
        f = captures[cap]
        try:
            n = runner.stage(bin_, [a.replace("{f}", f) for a in args], per_pass, os.path.join(runner.work, name + ".log"))
        except RuntimeError as e:
            print(f"{name}: failed\n{e}", file=sys.stderr)
            stages[name] = None
            continue
        air = os.path.getsize(f) / 2 / 2_400_000
        stages[name] = {"instructions": n, "air_s": round(air, 3)}
        print(f"  {name}: {n / 1e6:,.1f} M instructions ({n / air / 1e6:,.1f} M per second of air)", file=sys.stderr)
    return stages


def compare(base, now, max_pct, base_name):
    """A markdown table and whether every stage is within max_pct."""
    rows = [f"| Stage | {base_name} (M instr.) | This build (M instr.) | Change |", "|---|---:|---:|---:|"]
    ok, notes = True, []
    for name in list(dict.fromkeys(list(base) + list(now))):
        b, n = base.get(name), now.get(name)
        bn = b["instructions"] if b else None
        nn = n["instructions"] if n else None
        if nn is None:
            ok = False
            rows.append(f"| {name} | {fmt(bn)} | failed | ❌ |")
            continue
        if bn is None:
            rows.append(f"| {name} | – | {fmt(nn)} | new |")
            continue
        pct = (nn - bn) / bn * 100
        mark = ""
        if pct > max_pct:
            ok, mark = False, " ❌"
        elif pct < -max_pct:
            mark = " ✅"
            notes.append(name)
        rows.append(f"| {name} | {fmt(bn)} | {fmt(nn)} | {pct:+.2f}%{mark} |")
    table = "\n".join(rows)
    if notes:
        table += f"\n\nLower than the {base_name.lower()} by more than {max_pct}%: {', '.join(notes)}." + (" Update perf/baseline.json to keep the gain." if base_name == "Baseline" else "")
    return table, ok


def fmt(n):
    return "–" if n is None else f"{n / 1e6:,.1f}"


def cmd_count(a, cmd):
    """regress.sh: run one command (its output where the caller sent it), its count appended to --to."""
    counter = native_counter() if a.counter == "auto" else a.counter
    rc, n = counted(cmd, counter)
    if n is not None and a.to:
        with open(a.to, "a") as f:
            f.write(f"{a.name}\t{n}\n")
    return rc


def main():
    argv = sys.argv[1:]
    if argv[:1] == ["count"]:
        p = argparse.ArgumentParser(prog="gate.py count")
        p.add_argument("--to")
        p.add_argument("--name", default="")
        p.add_argument("--counter", default="auto")
        rest = argv[1:]
        if "--" not in rest:
            sys.exit("gate.py count [--to FILE --name NAME] -- CMD...")
        i = rest.index("--")
        return cmd_count(p.parse_args(rest[:i]), rest[i + 1 :])

    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--bin", default=os.path.join(ROOT, "target/release/trunk-pro"))
    p.add_argument("--against", help="compare with this build instead of perf/baseline.json")
    p.add_argument("--counter", default="auto", help="cachegrind, perf, time (auto: this computer's hardware counter, else cachegrind)")
    p.add_argument("--runs", type=int, default=3, help="hardware counts: the median of this many")
    p.add_argument("--max", type=float, default=2.0, help="the most a stage may rise, percent")
    p.add_argument("--update", action="store_true", help="write the counts to perf/baseline.json")
    p.add_argument("--summary", help="also append the table here (CI: $GITHUB_STEP_SUMMARY)")
    p.add_argument("--json", help="also write the counts here")
    p.add_argument("--only", help="these stages (comma-separated)")
    a = p.parse_args(argv)

    counter = a.counter
    if counter == "auto":
        counter = native_counter()
        if counter == "none":
            counter = "cachegrind" if shutil.which("valgrind") else sys.exit("no instruction counter here: install valgrind, or perf (Linux)")
    only = set(a.only.split(",")) if a.only else None
    work = tempfile.mkdtemp(prefix="trunk-perf-")
    try:
        captures = {}
        for f, args in SYNTH:
            captures[f] = os.path.join(work, f)
            subprocess.run([a.bin] + [x.replace("{f}", captures[f]) for x in args], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        runner = Runner(counter, a.runs, work)
        print(f"{a.bin} ({label(counter)}):", file=sys.stderr)
        now = {"counter": label(counter), "stages": measure(a.bin, runner, captures, only)}
        if a.json:
            with open(a.json, "w") as f:
                json.dump(now, f, indent=2)
        if a.update:
            if any(v is None for v in now["stages"].values()):
                sys.exit("a stage failed: baseline not written")
            with open(BASELINE, "w") as f:
                json.dump(now, f, indent=2)
                f.write("\n")
            print(f"wrote {BASELINE}", file=sys.stderr)
            return 0
        if a.against:
            print(f"{a.against} ({label(counter)}):", file=sys.stderr)
            base, base_name = measure(a.against, runner, captures, only), "Other build"
        else:
            try:
                with open(BASELINE) as f:
                    b = json.load(f)
            except FileNotFoundError:
                b = {"counter": label(counter), "stages": {}}
            if b["counter"] != label(counter):
                print(f"perf/baseline.json is {b['counter']}'s; this is {label(counter)}: compare two builds with --against instead.", file=sys.stderr)
                return 0
            base, base_name = {k: v for k, v in b["stages"].items() if not only or k in only}, "Baseline"
        table, ok = compare(base, now["stages"], a.max, base_name)
        if ok:
            end = f"Every stage within {a.max}% of the {base_name.lower()}."
        else:
            end = f"A stage rose more than {a.max}%. If that's intended, update perf/baseline.json (perf/gate.py --update, or the counts from this run's `--json`)."
        text = f"### CPU cost ({label(counter)})\n\n{table}\n\n{end}"
        print(text)
        if a.summary:
            with open(a.summary, "a") as f:
                f.write(text + "\n")
        return 0 if ok else 1
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
