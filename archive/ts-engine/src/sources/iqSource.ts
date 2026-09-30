// Where wideband IQ comes from. Every source yields RTL-SDR native unsigned
// 8-bit interleaved IQ, the format the channelizer ingests directly.

export interface SourceSettings {
  centerHz: number;
  rateHz: number;
  /** null = tuner AGC. */
  gainDb: number | null;
  ppm: number;
  /** USB serial of the dongle to use; "" = first granted. */
  serial?: string;
}

export interface IqSource {
  readonly kind: "usb" | "file";
  open(settings: SourceSettings): Promise<void>;
  /** Next chunk of u8 IQ, or null at end of stream. */
  read(): Promise<Uint8Array | null>;
  close(): Promise<void>;
}
