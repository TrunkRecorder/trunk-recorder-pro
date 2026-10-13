# Compare OP25's per-frame IMBE hex: decrypted (PLAINTEXT) vs clear-file (CLEARTEXT).
import re, sys
def frames(path, tag):
    return [l.split(tag)[1].split(" errs")[0].strip() for l in open(path, errors="replace") if tag in l]
clear = frames(sys.argv[1], "IMBE (CLEARTEXT)")
for p in sys.argv[2:]:
    pt = frames(p, "IMBE (PLAINTEXT)"); ct = frames(p, "IMBE (CIPHERTXT)")
    n = min(len(pt), len(clear))
    eq = sum(a == b for a, b in zip(pt, clear))
    biterr = sum(bin(int(a.replace(" ", ""), 16) ^ int(b.replace(" ", ""), 16)).count("1") for a, b in zip(pt, clear))
    ceq = sum(a == b for a, b in zip(ct, pt))
    print(f"{p.split('/')[-1]:22s} frames={len(pt)} exact_match_vs_clear={eq}/{n} bit_errors={biterr}/{n*88} (plaintext==ciphertext in {ceq} frames)")
