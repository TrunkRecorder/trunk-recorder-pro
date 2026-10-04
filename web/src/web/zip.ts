// A minimal zip writer for exporting calls where there's no folder picker
// (Android Chrome, Firefox). Entries are stored, not deflated: WAV audio
// barely compresses and the JSON is small. The archive is a Blob made of the
// files' own Blobs, so it isn't copied into memory; only each file's bytes
// are read once, for its CRC. No zip64: up to 65535 entries and 4 GiB.

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(b: Uint8Array): number {
  let c = 0xffffffff;
  for (let i = 0; i < b.length; i++) c = CRC_TABLE[(c ^ b[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** MS-DOS time and date words. */
function dosTime(ms: number): [number, number] {
  const d = new Date(ms);
  const time = (d.getHours() << 11) | (d.getMinutes() << 5) | (d.getSeconds() >> 1);
  const date = (Math.max(0, d.getFullYear() - 1980) << 9) | ((d.getMonth() + 1) << 5) | d.getDate();
  return [time, date];
}

export class ZipWriter {
  private parts: BlobPart[] = [];
  private central: Uint8Array[] = [];
  private offset = 0;
  private count = 0;

  /** Add one file at `name` ("a/b/c.wav"). */
  async add(name: string, data: Blob, modifiedMs = Date.now()): Promise<void> {
    if (this.count >= 0xffff) throw new Error("too many files for one zip (65535)");
    if (this.offset + data.size + 1024 > 0xffffffff) throw new Error("the export is over 4 GB; delete some calls and export the rest separately");
    const bytes = new Uint8Array(await data.arrayBuffer());
    const crc = crc32(bytes);
    const fname = new TextEncoder().encode(name);
    const [time, date] = dosTime(modifiedMs);

    const local = new DataView(new ArrayBuffer(30));
    local.setUint32(0, 0x04034b50, true);
    local.setUint16(4, 20, true); // version needed
    local.setUint16(6, 0x0800, true); // UTF-8 names
    local.setUint16(8, 0, true); // stored
    local.setUint16(10, time, true);
    local.setUint16(12, date, true);
    local.setUint32(14, crc, true);
    local.setUint32(18, bytes.length, true);
    local.setUint32(22, bytes.length, true);
    local.setUint16(26, fname.length, true);
    local.setUint16(28, 0, true);

    const cen = new DataView(new ArrayBuffer(46 + fname.length));
    cen.setUint32(0, 0x02014b50, true);
    cen.setUint16(4, 20, true); // version made by
    cen.setUint16(6, 20, true);
    cen.setUint16(8, 0x0800, true);
    cen.setUint16(10, 0, true);
    cen.setUint16(12, time, true);
    cen.setUint16(14, date, true);
    cen.setUint32(16, crc, true);
    cen.setUint32(20, bytes.length, true);
    cen.setUint32(24, bytes.length, true);
    cen.setUint16(28, fname.length, true);
    cen.setUint32(42, this.offset, true);
    new Uint8Array(cen.buffer).set(fname, 46);

    // Keep the original Blob (not the bytes just read) so memory stays flat.
    this.parts.push(local.buffer, fname, data);
    this.central.push(new Uint8Array(cen.buffer));
    this.offset += 30 + fname.length + bytes.length;
    this.count++;
  }

  finish(): Blob {
    const size = this.central.reduce((n, c) => n + c.length, 0);
    const end = new DataView(new ArrayBuffer(22));
    end.setUint32(0, 0x06054b50, true);
    end.setUint16(8, this.count, true);
    end.setUint16(10, this.count, true);
    end.setUint32(12, size, true);
    end.setUint32(16, this.offset, true);
    return new Blob([...this.parts, ...this.central.map((c) => c as Uint8Array<ArrayBuffer>), end.buffer], { type: "application/zip" });
  }
}
