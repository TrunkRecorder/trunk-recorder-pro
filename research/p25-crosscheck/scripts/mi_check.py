# Check each LDU2 MI the decoder logged equals OP25-style LFSR(previous MI).
import re, sys
def step(l):
    ov = (l >> 63) & 1
    fb = ((l >> 63) ^ (l >> 61) ^ (l >> 45) ^ (l >> 37) ^ (l >> 26) ^ (l >> 14)) & 1
    return ((l << 1) | fb) & (2**64 - 1), ov
def cycle(mi):
    l = int.from_bytes(bytes(mi[:8]), "big")
    for _ in range(64): l, _ = step(l)
    return list(l.to_bytes(8, "big")) + [0]
mis = [[int(x, 16) for x in m.split()] for m in re.findall(r"mi=([0-9a-f ]{26})", open(sys.argv[1]).read())]
ok = sum(cycle(a) == b for a, b in zip(mis, mis[1:]))
print(f"{len(mis)} MIs logged (HDU + LDU2s); {ok}/{len(mis)-1} transitions match the 64-step LFSR")
