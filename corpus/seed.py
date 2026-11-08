#!/usr/bin/env python3
"""Make SigMF captures of the regression scenarios from the reference recordings.

captures/regress.sh gives each scenario on the command line. This writes the same
scenarios as capture folders in captures/scenarios/<id>/, with the decoding settings
in their metadata, then adds them to corpus/corpus.json:

    corpus/seed.py [--bin target/release/trunk-pro]

The data files are hard links to the recordings in captures/.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CAPTURES = ROOT / "captures"
RATE = 2_400_000
GUARD = 75_000.0

P25_SOURCES = [("p25-858300000.cu8", 858_300_000), ("p25-860400000.cu8", 860_400_000)]
DCFD_CC = [857_987_500.0, 855_462_500.0]


def system(kind, site):
    return {"shortName": "replay", "type": kind, "sites": [site]}


def channels(mode, freqs):
    return [{"freqHz": float(f), "mode": mode} for f in freqs]


SCENARIOS = {
    "p25": ("p25", "DCFD P25, CQPSK simulcast, two radios", P25_SOURCES, {"systems": [system("p25", {"controlChannelsHz": DCFD_CC})]}),
    "capplus": ("dmr", "DMR Capacity Plus", [("dmr-464200000.cu8", 464_200_000)], {"systems": [system("dmr", {"controlChannelsHz": [463_375_000.0, 463_750_000.0, 464_350_000.0]})]}),
    "capmax": ("dmr", "DMR Capacity Max", [("dmr-451800000.cu8", 451_800_000)], {"systems": [system("dmr", {"controlChannelsHz": [452_175_000.0], "dmrChannelsHz": [452_275_000.0]})]}),
    "smartnet": (
        "smartnet",
        "WMATA SmartNet, UHF OBT, two radios",
        [("sn-496337000.cu8", 496_337_000), ("sn-490600000.cu8", 490_600_000)],
        {"systems": [system("smartnet", {"controlChannelsHz": [496_437_500.0, 490_862_500.0, 496_537_500.0, 490_787_500.0], "bandplan": "400_custom", "bandplanBaseHz": 489_087_500.0, "bandplanSpacingHz": 25_000.0, "bandplanOffset": 380, "bandplanHighHz": 496_612_500.0})]},
    ),
    "conv-dmr": ("dmr", "Conventional DMR", [("dmr-464200000.cu8", 464_200_000)], {"conventional": [{"shortName": "conv", "channels": channels("dmr", [463_550_000, 464_462_500, 464_775_000, 464_825_000])}]}),
    "conv-p25-fm": ("p25", "Conventional P25 and FM", [("p25-858300000.cu8", 858_300_000)], {"conventional": [{"shortName": "conv", "channels": channels("fm", [858_100_000, 858_700_000]) + channels("p25", [858_587_500, 859_037_500, 857_587_500])}]}),
    "phase2": ("p25", "DCFD P25 with Phase 2 TDMA channels, encrypted calls recorded", [("p2-858300000.cu8", 858_300_000), ("p2-770400000.cu8", 770_400_000)], {"systems": [system("p25", {"controlChannelsHz": DCFD_CC})], "recording": {"recordEncrypted": True}}),
}

_sha512 = {}


def sha512_of(path):
    if path not in _sha512:
        h = hashlib.sha512()
        with open(path, "rb") as f:
            while chunk := f.read(1 << 20):
                h.update(chunk)
        _sha512[path] = h.hexdigest()
    return _sha512[path]


def meta(description, source_file, center, config):
    return {
        "global": {
            "core:datatype": "cu8",
            "core:sample_rate": float(RATE),
            "core:version": "1.2.0",
            "core:recorder": "rtl_sdr",
            "core:hw": "rtlsdr",
            "core:sha512": sha512_of(CAPTURES / source_file),
            "core:collection": "capture",
            "core:description": description,
            "core:extensions": [{"name": "trp", "version": "0.1.0", "optional": True}],
            "trp:tier": "wideband",
            "trp:purpose": "healthy",
            "trp:source": {"type": "rtlsdr", "rateHz": float(RATE), "autoTune": False, "guardHz": GUARD},
            "trp:config": config,
            "trp:talkgroups": {},
            "trp:bandplans": [],
        },
        "captures": [{"core:sample_start": 0, "core:global_index": 0, "core:frequency": float(center)}],
        "annotations": [],
    }


def write_scenario(scenario, description, sources, settings):
    folder = CAPTURES / "scenarios" / scenario
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    # (replay's own default vocoder, which the reference outputs were made with.)
    config = dict(settings, recording={"vocoder": "enhanced", **settings.get("recording", {})})
    config["sources"] = [{"type": "file", "path": f"source-{i}.sigmf-data", "format": "cu8", "realtime": False, "centerHz": float(c), "rateHz": float(RATE), "autoTune": False, "guardHz": GUARD, "name": f"radio-{i}"} for i, (_, c) in enumerate(sources)]
    streams = []
    for i, (file, center) in enumerate(sources):
        text = json.dumps(meta(description, file, center, config), indent=2, sort_keys=True)
        (folder / f"source-{i}.sigmf-meta").write_text(text)
        os.link(CAPTURES / file, folder / f"source-{i}.sigmf-data")
        streams.append({"name": f"source-{i}", "hash": hashlib.sha512(text.encode()).hexdigest()})
    collection = {"collection": {"core:version": "1.2.0", "core:streams": streams}}
    (folder / "capture.sigmf-collection").write_text(json.dumps(collection, indent=2, sort_keys=True))
    return folder


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", default=str(ROOT / "target/release/trunk-pro"))
    args = parser.parse_args()
    for scenario, (protocol, description, sources, settings) in SCENARIOS.items():
        folder = write_scenario(scenario, description, sources, settings)
        subprocess.run([args.bin, "tool", "corpus", "add", str(folder), "--id", scenario, "--protocol", protocol], cwd=ROOT, check=True)
        print(f"{scenario}: {folder}")


if __name__ == "__main__":
    main()
