# Compare dsd-fme -Z IMBE hex against OP25's error-free clear decode (same 88-bit u0..u7 packing).
import re, sys
truth = [l.split("IMBE (CLEARTEXT)")[1].split(" errs")[0].replace(" ", "").upper() for l in open(sys.argv[1]) if "IMBE (CLEARTEXT)" in l]
for p in sys.argv[2:]:
    f = re.findall(r" IMBE ([0-9A-F]{22}) err = \[(\d+)\] \[(\d+)\]", open(p, errors="replace").read())
    n = min(len(f), len(truth))
    bad = [i for i in range(n) if f[i][0] != truth[i]]
    be = sum(bin(int(f[i][0], 16) ^ int(truth[i], 16)).count("1") for i in range(n))
    bad_fec = sum(1 for i in bad if f[i][1] != "0" or f[i][2] != "0")
    print(f"{p.split('/')[-1]:20s} exact={n-len(bad)}/{n} bit_errors={be}/{n*88} mismatched_frames_with_fec_errs={bad_fec}/{len(bad)} idx={bad[:12]}")
