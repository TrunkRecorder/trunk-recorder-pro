// A CTCSS tone or DCS code as shown, with what FRS/GMRS radios call it on its
// frequency: the recorder's Tone::label and Tone::privacy_code (rules.ts toneLabel).

import { toneLabel, type ToneLabel } from "./rules.ts";

/** An FRS/GMRS privacy code: "#24", or a DCS code's Motorola number, "Moto #39". */
export function PrivacyCode({ p }: { p: NonNullable<ToneLabel["privacy"]> }) {
  const title = p.motorola
    ? `Motorola's FRS/GMRS privacy code ${p.code} (other brands number DCS codes differently)`
    : `FRS/GMRS privacy code ${p.code} (the same on every brand)`;
  return (
    <span className="privacy-code" title={title}>
      {p.motorola ? `Moto #${p.code}` : `#${p.code}`}
    </span>
  );
}

/** Tone or code `code` heard or set on `freqHz`, with its privacy code there if it has one. */
export function ToneName({ code, freqHz }: { code: string; freqHz: number }) {
  const t = toneLabel(code, freqHz);
  return (
    <>
      {t.text}
      {t.privacy && <PrivacyCode p={t.privacy} />}
    </>
  );
}
