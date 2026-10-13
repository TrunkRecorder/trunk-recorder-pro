# Compare dsd-fme's -Z per-frame IMBE hex (printed after decryption) against its decode of clear.wav.
import re, sys
def frames(p):
    return [(m.group(1), int(m.group(2)), int(m.group(3))) for m in re.finditer(r" IMBE ([0-9A-F]{22}) err = \[(\d+)\] \[(\d+)\]", open(p, errors="replace").read())]
clear = frames(sys.argv[1])
print(f"{sys.argv[1].split('/')[-1]:22s} frames={len(clear)} fec_err_frames={sum(1 for _,a,b in clear if a or b)}")
for p in sys.argv[2:]:
    f = frames(p); n = min(len(f), len(clear))
    eq = sum(a[0] == b[0] for a, b in zip(f, clear))
    be = sum(bin(int(a[0], 16) ^ int(b[0], 16)).count("1") for a, b in zip(f, clear))
    print(f"{p.split('/')[-1]:22s} frames={len(f)} exact_match_vs_clear={eq}/{n} bit_errors={be}/{n*88} fec_err_frames={sum(1 for _,a,b in f if a or b)}")
