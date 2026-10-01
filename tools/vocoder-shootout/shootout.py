#!/usr/bin/env python3
"""Vocoder Shootout: decode the same P25 IMBE frames with several vocoders,
score them, and build a blind listening page.

    ./shootout.py setup [--tr-source DIR]     build the TR harness, install Python deps, fetch DNSMOS
    ./shootout.py decoders                    list decoders and whether each is available
    ./shootout.py run INPUT... [options]      render, score, write a report and a listening page
    ./shootout.py stats INPUT...              voicing statistics of the encoded speech

INPUT is a file or a directory (searched recursively) of IMBE frames:
  *.frames.jsonl  trunk-pro's vocoder frame capture (Recording → "Save vocoder frames"),
                  with the call's .json record beside it for talkgroup names
  *.hex           one frame per line: 22 hex digits (u0..u7), optionally followed by E0 and ET
  *.imbe          raw 11-byte frames (u0..u7 packed MSB first)

run options:
  -o, --out DIR          output directory (default: shootout-<date-time>)
  --decoders a,b,...     which decoders (default: every available one marked default)
  --min-seconds S        skip calls shorter than this (default 3)
  --max-calls N          score at most N calls, the longest first (default 24)
  --page-clips N         calls on the listening page (default 4)
  --clip-seconds S       length of each page clip (default 8)
  --title TEXT           page title (default "Vocoder Shootout")
  --no-score             skip DNSMOS
  --no-zip               don't bundle DIR.zip

Outputs in DIR: audio/<call>.<decoder>.wav (8 kHz), scores.json, report.md, shootout.html,
and DIR.zip beside it with all of those (not frames/) for viewing on another computer
(--no-zip to skip).
"""

import argparse
import base64
import datetime
import glob
import json
import os
import platform
import shutil
import subprocess
import sys
import urllib.request
import zipfile

HERE = os.path.dirname(os.path.abspath(__file__))
VENV = os.path.join(HERE, ".venv")
VENV_PY = os.path.join(VENV, "bin", "python")
CACHE = os.path.join(HERE, ".cache")
SETTINGS = os.path.join(CACHE, "settings.json")
HARNESS = os.path.join(HERE, "build", "imbe_tr")
DNSMOS = os.path.join(CACHE, "dnsmos", "sig_bak_ovr.onnx")
DNSMOS_URL = "https://github.com/microsoft/DNS-Challenge/raw/master/DNSMOS/DNSMOS/sig_bak_ovr.onnx"
DEFAULT_TR = os.path.expanduser("~/Projects/Trunk Recorder/source")
PY_DEPS = ["numpy", "scipy", "soundfile", "onnxruntime"]

# Speech level every clip is brought to before scoring and listening, dBFS.
LEVEL_DB = -18.0


def die(msg):
    sys.exit(f"shootout: {msg}")


def settings():
    try:
        with open(SETTINGS) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def save_settings(s):
    os.makedirs(CACHE, exist_ok=True)
    with open(SETTINGS, "w") as f:
        json.dump(s, f, indent=1)


def in_venv():
    return os.path.realpath(sys.prefix) == os.path.realpath(VENV)


def reexec_in_venv():
    if not in_venv() and os.path.exists(VENV_PY):
        os.execv(VENV_PY, [VENV_PY, os.path.abspath(__file__)] + sys.argv[1:])


# ---------------------------------------------------------------- setup


def setup(a):
    s = settings()
    tr = os.path.abspath(os.path.expanduser(a.tr_source or s.get("tr_source") or DEFAULT_TR))
    lib = os.path.join(tr, "lib", "op25_repeater", "lib")
    if not os.path.exists(os.path.join(lib, "software_imbe_decoder.cc")):
        die(f"no Trunk Recorder source at {tr} (want {lib}/software_imbe_decoder.cc); pass --tr-source")
    s["tr_source"] = tr
    save_settings(s)

    print(f"Building the Trunk Recorder harness against {tr}")
    os.makedirs(os.path.dirname(HARNESS), exist_ok=True)
    cxx = shutil.which("clang++") or shutil.which("g++") or die("no C++ compiler (clang++ or g++)")
    inc = [f"-I{lib}", f"-I{lib}/imbe_vocoder"]
    for p in ["/opt/homebrew/include", "/usr/local/include"]:
        if os.path.isdir(os.path.join(p, "boost")):
            inc.append(f"-I{p}")
    srcs = [os.path.join(HERE, "harness", "imbe_tr.cc"), f"{lib}/software_imbe_decoder.cc", f"{lib}/imbe_decoder.cc"]
    srcs += sorted(glob.glob(f"{lib}/imbe_vocoder/*.cc"))
    r = subprocess.run([cxx, "-std=c++17", "-O2", "-w", *inc, *srcs, "-o", HARNESS, "-lm"], capture_output=True, text=True)
    if r.returncode:
        print(r.stderr[-3000:])
        die("harness build failed (Boost headers are needed: brew install boost)")

    print("Creating the Python environment")
    if not os.path.exists(VENV_PY):
        subprocess.run([sys.executable, "-m", "venv", VENV], check=True)
    subprocess.run([VENV_PY, "-m", "pip", "install", "-q", "--disable-pip-version-check", *PY_DEPS], check=True)
    install_blip25()

    if not os.path.exists(DNSMOS):
        print("Fetching the DNSMOS model")
        os.makedirs(os.path.dirname(DNSMOS), exist_ok=True)
        urllib.request.urlretrieve(DNSMOS_URL, DNSMOS)
    if not shutil.which("ffmpeg"):
        print("Note: ffmpeg not found; the listening page will carry WAV instead of AAC.")
    print("Ready.")
    list_decoders(None)


def install_blip25():
    """blip25-mbe from PyPI; when pip won't pick a wheel, fetch this platform's wheel directly."""
    q = [VENV_PY, "-m", "pip", "install", "-q", "--disable-pip-version-check"]
    if subprocess.run(q + ["blip25-mbe"], capture_output=True).returncode == 0:
        return
    try:
        with urllib.request.urlopen("https://pypi.org/pypi/blip25-mbe/json") as r:
            files = json.load(r)["urls"]
        mach = platform.machine().lower()
        want = {"Darwin": "macosx", "Linux": "manylinux", "Windows": "win"}.get(platform.system(), "?")
        arch = {"arm64": ("arm64",), "aarch64": ("aarch64", "arm64"), "x86_64": ("x86_64",), "amd64": ("amd64", "x86_64")}.get(mach, (mach,))
        wheel = next(f for f in files if f["filename"].endswith(".whl") and want in f["filename"] and any(x in f["filename"] for x in arch))
        path = os.path.join(CACHE, wheel["filename"])
        urllib.request.urlretrieve(wheel["url"], path)
        subprocess.run(q + [path], check=True)
    except Exception as e:  # optional decoder
        print(f"Note: blip25-mbe not installed ({e}); its decoder will be skipped.")


# ---------------------------------------------------------------- decoders


def trunk_pro_path():
    for p in [os.environ.get("TRUNK_PRO"), os.path.join(HERE, "..", "..", "target", "release", "trunk-pro"), shutil.which("trunk-pro")]:
        if p and os.path.exists(p):
            return os.path.abspath(p)
    return None


def available():
    py = VENV_PY if os.path.exists(VENV_PY) else sys.executable
    blip = subprocess.run([py, "-c", "import blip25_mbe"], capture_output=True).returncode == 0
    return {"trunk_pro": trunk_pro_path(), "harness": HARNESS if os.path.exists(HARNESS) else None, "blip25": py if blip else None, "python": py}


def load_decoders():
    with open(os.path.join(HERE, "decoders.json")) as f:
        return json.load(f)["decoders"]


def list_decoders(_):
    have = available()
    for k, d in load_decoders().items():
        missing = [n for n in d.get("needs", []) if not have.get(n)]
        state = "ready" if not missing else "missing " + ", ".join(missing)
        print(f"  {k:12} {d['label']:28} {state}{'' if d.get('default', True) else '  (not default)'}")
    if not have["trunk_pro"]:
        print("  trunk-pro: build it (cargo build --release -p trunk-pro) or set TRUNK_PRO")
    if not have["harness"]:
        print("  harness: run ./shootout.py setup")


def pick_decoders(names):
    have, all_ = available(), load_decoders()
    chosen = names.split(",") if names else [k for k, d in all_.items() if d.get("default", True)]
    out = {}
    for k in chosen:
        if k not in all_:
            die(f"unknown decoder {k} (see ./shootout.py decoders)")
        missing = [n for n in all_[k].get("needs", []) if not have.get(n)]
        if missing:
            print(f"skipping {k}: needs {', '.join(missing)}")
            continue
        out[k] = all_[k]
    if not out:
        die("no decoders available; run ./shootout.py setup")
    return out, have


def render(dec, frames, out_wav, have):
    import numpy as np
    import soundfile as sf

    tmp = out_wav + (".s16" if dec["output"] == "s16" else ".tmp.wav")
    vals = {"in": frames, "out": tmp, "here": HERE, **{k: v or "" for k, v in have.items()}}
    cmd = [c.format(**vals) for c in dec["cmd"]]
    r = subprocess.run(cmd, capture_output=True, text=True)
    if r.returncode or not os.path.exists(tmp):
        raise RuntimeError(f"{' '.join(cmd)}: {r.stderr.strip()[-400:]}")
    if dec["output"] == "s16":
        x = np.fromfile(tmp, dtype=np.int16).astype(np.float32) / 32768
    else:
        x, rate = sf.read(tmp, dtype="float32")
        if rate != 8000:
            raise RuntimeError(f"{tmp}: {rate} Hz, want 8000")
    os.remove(tmp)
    sf.write(out_wav, x, 8000, subtype="PCM_16")
    return x


# ---------------------------------------------------------------- inputs


def find_inputs(paths):
    files = []
    for p in paths:
        if os.path.isdir(p):
            for ext in ("*.frames.jsonl", "*.hex", "*.imbe"):
                files += glob.glob(os.path.join(p, "**", ext), recursive=True)
        elif os.path.exists(p):
            files.append(p)
        else:
            die(f"{p}: no such file or directory")
    return sorted(set(files))


def call_name(path):
    b = os.path.basename(path)
    for ext in (".frames.jsonl", ".hex", ".imbe"):
        if b.endswith(ext):
            return b[: -len(ext)]
    return os.path.splitext(b)[0]


def normalise(path, dst):
    """Any input format → trunk-pro frames.jsonl at dst; returns (frames, metadata)."""
    recs = []
    if path.endswith(".imbe"):
        data = open(path, "rb").read()
        recs = [{"bits": data[i : i + 11].hex(), "e0": 0, "errs": 0} for i in range(0, len(data) - 10, 11)]
    elif path.endswith(".hex"):
        for line in open(path):
            t = line.split()
            if t and len(t[0]) == 22:
                recs.append({"bits": t[0].lower(), "e0": int(t[1]) if len(t) > 1 else 0, "errs": int(t[2]) if len(t) > 2 else 0})
    else:
        for line in open(path):
            line = line.strip()
            if line:
                r = json.loads(line)
                if r.get("codec") == "imbe":
                    recs.append(r)
    with open(dst, "w") as f:
        for i, r in enumerate(recs):
            r = {"pos": round(i * 0.02, 2), "codec": "imbe", "bits": r["bits"], "e0": r.get("e0", 0), "errs": r.get("errs", 0),
                 "erased": r.get("erased", False), "out": r.get("out", "voice")}
            f.write(json.dumps(r, separators=(",", ":")) + "\n")
    meta = {"name": call_name(path), "desc": "", "tag": ""}
    rec = path[: -len(".frames.jsonl")] + ".json" if path.endswith(".frames.jsonl") else None
    if rec and os.path.exists(rec):
        try:
            c = json.load(open(rec))
            meta["name"] = c.get("talkgroup_tag") or str(c.get("talkgroup", meta["name"]))
            meta["desc"] = c.get("talkgroup_description", "")
            bits = [f"TG {c['talkgroup']}"] if "talkgroup" in c else []
            if c.get("freq"):
                bits.append(f"{c['freq'] / 1e6:.4f} MHz")
            meta["tag"] = " · ".join(bits)
        except ValueError:
            pass
    errs = sum(r.get("errs", 0) for r in recs)
    meta["nframes"], meta["errs_per_frame"] = len(recs), (errs / len(recs) if recs else 0)
    return len(recs), meta


# ---------------------------------------------------------------- scoring


def speech_level(x):
    import numpy as np

    n = len(x) // 160 * 160
    if n == 0:
        return None
    p = (x[:n].astype(np.float64).reshape(-1, 160) ** 2).mean(1)
    db = 10 * np.log10(np.maximum(p, 1e-20))
    loud = p[db > -50]
    if len(loud) < 10:
        return None
    k = loud[10 * np.log10(loud) > 10 * np.log10(loud.mean()) - 10]
    return 10 * np.log10(k.mean()) if len(k) >= 10 else None


def level_match(x):
    import numpy as np

    l = speech_level(x)
    return np.clip(x * 10 ** ((LEVEL_DB - l) / 20), -0.98, 0.98) if l is not None else x


class Dnsmos:
    def __init__(self):
        import onnxruntime as ort

        self.sess = ort.InferenceSession(DNSMOS)
        import numpy as np

        self.p = [np.poly1d([-0.08397278, 1.22083953, 0.0052439]), np.poly1d([-0.13166888, 1.60915514, -0.39604546]),
                  np.poly1d([-0.06766283, 1.11546468, 0.04602535])]

    def __call__(self, x8k):
        """(SIG, BAK, OVRL) of 8 kHz audio, levelled, upsampled to 16 kHz, mean over 9 s windows hopped by 1 s."""
        import numpy as np
        from scipy.signal import resample_poly

        x = resample_poly(level_match(x8k), 2, 1).astype(np.float32)
        x *= 10 ** (-26 / 20) / (np.sqrt(np.mean(x**2)) + 1e-9)
        L = int(9.01 * 16000)
        while len(x) < L:
            x = np.concatenate([x, x])
        out = []
        for s in range(0, len(x) - L + 1, 16000):
            raw = self.sess.run(None, {"input_1": x[s : s + L][None, :]})[0][0]
            out.append([float(self.p[i](raw[i])) for i in range(3)])
        return [float(v) for v in np.mean(out, 0)]


# ---------------------------------------------------------------- run


def run(a):
    import numpy as np

    decs, have = pick_decoders(a.decoders)
    out = os.path.abspath(a.out or datetime.datetime.now().strftime("shootout-%Y%m%d-%H%M%S"))
    for d in ("frames", "audio"):
        os.makedirs(os.path.join(out, d), exist_ok=True)

    calls, seen = [], set()
    for p in find_inputs(a.inputs):
        name = call_name(p)
        if name in seen:  # the same call in two formats or folders
            name += "-" + os.path.basename(p)[len(name) + 1 :].replace(".", "-")
        seen.add(name)
        dst = os.path.join(out, "frames", name + ".frames.jsonl")
        n, meta = normalise(p, dst)
        if n * 0.02 >= a.min_seconds:
            calls.append({"id": name, "path": dst, **meta})
        else:
            os.remove(dst)
    if not calls:
        die(f"no IMBE calls of {a.min_seconds} s or more in the inputs")
    calls = sorted(calls, key=lambda c: -c["nframes"])[: a.max_calls]
    print(f"{len(calls)} calls, {len(decs)} decoders: {', '.join(decs)}")

    score = None
    if not a.no_score:
        if os.path.exists(DNSMOS):
            score = Dnsmos()
        else:
            print("DNSMOS model missing (./shootout.py setup); skipping scores")

    audio, results = {}, {}
    for c in calls:
        audio[c["id"]] = {}
        for k, d in decs.items():
            wav = os.path.join(out, "audio", f"{c['id']}.{k}.wav")
            try:
                x = render(d, c["path"], wav, have)
            except RuntimeError as e:
                print(f"  {k} failed on {c['id']}: {e}")
                continue
            audio[c["id"]][k] = x
            if score is not None and speech_level(x) is not None:
                results.setdefault(c["id"], {})[k] = score(x)
        got = results.get(c["id"], {})
        print(f"  {c['id']}: " + "  ".join(f"{k} {v[2]:.2f}" for k, v in got.items()) if got else f"  {c['id']}: rendered")

    summary = {}
    for k in decs:
        rows = [results[c][k] for c in results if k in results[c]]
        if rows:
            m = np.mean(rows, 0)
            wins = sum(1 for c in results if results[c] and max(results[c], key=lambda j: results[c][j][2]) == k)
            summary[k] = {"sig": float(m[0]), "bak": float(m[1]), "ovrl": float(m[2]), "calls": len(rows), "wins": wins}

    stats = voicing_stats([c["path"] for c in calls], have)
    with open(os.path.join(out, "scores.json"), "w") as f:
        json.dump({"decoders": {k: {"label": d["label"], "note": d["note"]} for k, d in decs.items()}, "summary": summary,
                   "per_call": results, "calls": [{k: v for k, v in c.items() if k != "path"} for c in calls], "voicing": stats}, f, indent=1)
    write_report(out, decs, summary, calls, stats)
    if not a.no_page:
        write_page(out, a, decs, calls, audio, summary, stats)
    print(f"\nWrote {out}/report.md" + ("" if a.no_page else f" and {out}/shootout.html"))
    if not a.no_zip:
        z = bundle(out)
        print(f"Bundled {z} ({os.path.getsize(z) / 1e6:.1f} MB)")


def bundle(out):
    """DIR.zip: the page, report, scores and every WAV, under one folder named like DIR."""
    root = os.path.basename(out.rstrip(os.sep))
    path = out.rstrip(os.sep) + ".zip"
    names = ["shootout.html", "report.md", "scores.json"] + [os.path.join("audio", f) for f in sorted(os.listdir(os.path.join(out, "audio"))) if f.endswith(".wav")]
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        for n in names:
            if os.path.exists(os.path.join(out, n)):
                z.write(os.path.join(out, n), os.path.join(root, n))
    return path


def voicing_stats(frame_files, have):
    if not have.get("harness"):
        return None
    cat = os.path.join(os.path.dirname(frame_files[0]), "..", "all.frames.jsonl")
    with open(cat, "w") as w:
        for p in frame_files:
            w.write(open(p).read())
    r = subprocess.run([have["harness"], "stats", cat], capture_output=True, text=True)
    os.remove(cat)
    return json.loads(r.stdout) if r.returncode == 0 and r.stdout.strip() else None


def write_report(out, decs, summary, calls, stats):
    lines = ["# Vocoder Shootout", "", f"{len(calls)} calls, {sum(c['nframes'] for c in calls) * 0.02:.0f} s of audio.", ""]
    if summary:
        lines += ["DNSMOS (1–5) on each whole call, level-matched; wins = calls where the decoder scored highest overall.", "",
                  "| Decoder | Signal | Background | Overall | Wins |", "|---|---|---|---|---|"]
        for k, s in sorted(summary.items(), key=lambda kv: -kv[1]["ovrl"]):
            lines.append(f"| {decs[k]['label']} | {s['sig']:.2f} | {s['bak']:.2f} | {s['ovrl']:.2f} | {s['wins']} |")
        lines.append("")
    if stats:
        lines += ["Encoded speech (from the radios' parameters, the same for every decoder):", "",
                  f"- speech frames: {stats['speech_frames']}",
                  f"- fully voiced frames: {100 * stats['fully_voiced']:.0f} % (DC Fire's modern P25 fleet: 11 %, WMATA: 22 %)",
                  f"- voiced harmonics above 2 kHz: {100 * stats['above_2k_voiced']:.0f} %",
                  f"- mean pitch: {stats['mean_f0_hz']:.0f} Hz", ""]
    lines += ["Calls:", ""] + [f"- {c['id']}: {c['name']} {c['desc']} {c['nframes'] * 0.02:.1f} s, {c['errs_per_frame']:.2f} FEC errors/frame" for c in calls]
    with open(os.path.join(out, "report.md"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print("\n" + "\n".join(lines[:4 + (len(summary) + 4 if summary else 0)]))


def write_page(out, a, decs, calls, audio, summary, stats):
    import numpy as np
    import soundfile as sf

    # The page carries the calls with the most speech; each clip is the densest stretch of it.
    ref = next(iter(decs))
    ranked = sorted([c for c in calls if ref in audio[c["id"]]], key=lambda c: -active(audio[c["id"]][ref]))[: a.page_clips]
    W = int(a.clip_seconds / 0.02)
    aac = shutil.which("ffmpeg") is not None
    clips = []
    for c in ranked:
        x = audio[c["id"]][ref]
        n = len(x) // 160
        act = (10 * np.log10((x[: n * 160].reshape(-1, 160) ** 2).mean(1) + 1e-12) > -45).astype(float)
        s = int(np.argmax(np.convolve(act, np.ones(W), "valid"))) if n > W else 0
        enc = {}
        for k in decs:
            if k not in audio[c["id"]]:
                continue
            y = level_match(audio[c["id"]][k][s * 160 : (s + W) * 160])
            wav = os.path.join(out, "audio", f".clip.{c['id']}.{k}.wav")
            sf.write(wav, y, 8000, subtype="PCM_16")
            if aac:
                m4a = wav[:-4] + ".m4a"
                subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", wav, "-c:a", "aac", "-ar", "16000", "-ac", "1", "-b:a", "32k",
                                "-movflags", "+faststart", "-f", "mp4", m4a], check=True)
                enc[k] = base64.b64encode(open(m4a, "rb").read()).decode()
                os.remove(m4a)
            else:
                enc[k] = base64.b64encode(open(wav, "rb").read()).decode()
            os.remove(wav)
        clips.append({"id": c["id"], "name": c["name"], "desc": c["desc"], "tag": c["tag"], "seconds": round(min(W, n) * 0.02, 1), "audio": enc})
    data = {"title": a.title, "mime": "audio/mp4" if aac else "audio/wav", "calls": len(calls),
            "decoders": {k: {"name": d["label"], "note": d["note"], "dns": [summary[k][j] for j in ("sig", "bak", "ovrl")] if k in summary else None}
                         for k, d in decs.items()},
            "clips": clips, "voicing": stats}
    with open(os.path.join(HERE, "page", "template.html")) as f:
        html = f.read()
    html = html.replace("__TITLE__", a.title).replace("__DATA__", json.dumps(data, separators=(",", ":")))
    with open(os.path.join(out, "shootout.html"), "w") as f:
        f.write(html)


def active(x):
    import numpy as np

    n = len(x) // 160
    return int(((10 * np.log10((x[: n * 160].reshape(-1, 160) ** 2).mean(1) + 1e-12)) > -45).sum()) if n else 0


def stats_cmd(a):
    have = available()
    if not have["harness"]:
        die("the harness isn't built; run ./shootout.py setup")
    import tempfile

    with tempfile.TemporaryDirectory() as t:
        files = []
        for p in find_inputs(a.inputs):
            dst = os.path.join(t, "frames", call_name(p) + ".frames.jsonl")
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            normalise(p, dst)
            files.append(dst)
        if not files:
            die("no IMBE inputs")
        s = voicing_stats(files, have)
    print(json.dumps(s, indent=1))


def main():
    reexec_in_venv()
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("setup")
    p.add_argument("--tr-source")
    sub.add_parser("decoders")
    p = sub.add_parser("run")
    p.add_argument("inputs", nargs="+")
    p.add_argument("-o", "--out")
    p.add_argument("--decoders")
    p.add_argument("--min-seconds", type=float, default=3.0)
    p.add_argument("--max-calls", type=int, default=24)
    p.add_argument("--page-clips", type=int, default=4)
    p.add_argument("--clip-seconds", type=float, default=8.0)
    p.add_argument("--title", default="Vocoder Shootout")
    p.add_argument("--no-score", action="store_true")
    p.add_argument("--no-page", action="store_true")
    p.add_argument("--no-zip", action="store_true")
    p = sub.add_parser("stats")
    p.add_argument("inputs", nargs="+")
    a = ap.parse_args()
    {"setup": setup, "decoders": list_decoders, "run": run, "stats": stats_cmd}[a.cmd](a)


if __name__ == "__main__":
    main()
