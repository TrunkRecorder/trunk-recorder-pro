// The one type freq-finder's workbench/chains.ts contributed to the P25 code.

export interface DecodedMessage {
  protocol: string;
  address: string;
  label: string;
  text: string;
  fields?: { label: string; value: string }[];
}
