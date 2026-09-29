// P25 TSBK CRC-16, ported verbatim from op25's `p25p1_fdma.cc::crc16`.
//
// It is a 16-bit CRC, generator poly (1<<12)+(1<<5)+(1<<0) = 0x1021 (CCITT),
// processed MSB-first over whole bytes, register initialised to 0 and finally
// XORed with 0xffff. A block is valid when `crc16(block, 12) === 0` — i.e. the
// two CRC bytes are appended so that re-running the CRC over all 12 bytes lands
// on zero.
//
// Verified in `web/test/p25.test.ts`: `appendCrc` produces a block that passes
// `crc16(block,12)===0`, and flipping any byte breaks it.

const POLY = (1 << 12) + (1 << 5) + (1 << 0); // 0x1021

/** op25 crc16 over `len` bytes of `buf`. Returns the 16-bit value; a valid TSBK
 *  block (12 bytes incl. its 2 CRC bytes) yields 0. */
export function crc16(buf: Uint8Array, len: number): number {
  let crc = 0;
  for (let i = 0; i < len; i++) {
    const bits = buf[i];
    for (let j = 0; j < 8; j++) {
      const bit = (bits >> (7 - j)) & 1;
      crc = ((crc << 1) | bit) & 0x1ffff;
      if (crc & 0x10000) crc = (crc & 0xffff) ^ POLY;
    }
  }
  return (crc ^ 0xffff) & 0xffff;
}

/** True when the 12-byte block's trailing CRC checks. */
export function tsbkCrcOk(block: Uint8Array): boolean {
  return crc16(block, 12) === 0;
}

/** Given a 12-byte block whose first 10 bytes are the opcode/mfid/args and whose
 *  last 2 bytes are zero, fill in the CRC so `tsbkCrcOk` passes. Mutates and
 *  returns the block. Used by the encoder/sim. */
export function appendCrc(block: Uint8Array): Uint8Array {
  block[10] = 0;
  block[11] = 0;
  const c = crc16(block, 12); // over 10 data bytes + 2 zero bytes (augmented)
  block[10] = (c >> 8) & 0xff;
  block[11] = c & 0xff;
  return block;
}
