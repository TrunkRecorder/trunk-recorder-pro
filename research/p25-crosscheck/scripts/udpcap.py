# Capture OP25 UDP audio (8 kHz s16le mono) to a WAV until idle timeout.
import socket, sys, wave, time
port, out, idle = int(sys.argv[1]), sys.argv[2], float(sys.argv[3]) if len(sys.argv) > 3 else 25
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind(("127.0.0.1", port)); s.settimeout(idle)
buf = bytearray(); n = 0
try:
    while True:
        d, _ = s.recvfrom(65536)
        if len(d) == 2: continue          # audio flag packet
        buf += d; n += 1
except socket.timeout:
    pass
w = wave.open(out, "wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(8000); w.writeframes(bytes(buf)); w.close()
print(f"{out}: {n} packets, {len(buf)//2} samples")
