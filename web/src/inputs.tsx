// The setup's form controls: a labelled field, a checkbox toggle, a hidden
// file picker, and text inputs that keep what is typed. Each text input is a
// DraftInput with a codec from fieldText.ts, so typing never fights a value
// being clamped, defaulted or tidied (a squelch of 12 typed as "1", then "12";
// "-" on the way to -5; a trailing comma in a list). Left, the field shows the
// value kept, tidied.
// The rules for using them: docs/developer/ui-style.md.

import { useState, type InputHTMLAttributes, type ReactNode, type RefObject } from "react";
import { decimal, gain, hex, talkgroupProblem, type TextCodec } from "./fieldText.ts";

/** A labelled setting. `needs`: what an import left to fill in here (highlighted); `anchor`: its place, for the to-do list's Show. */
export function Field(props: { label: string; hint?: ReactNode; children: ReactNode; wide?: boolean; needs?: string; anchor?: string; className?: string }) {
  return (
    <label className={`field${props.wide ? " wide" : ""}${props.needs ? " needs" : ""}${props.className ? ` ${props.className}` : ""}`} id={props.anchor && `need-${props.anchor}`}>
      <span className="field-label">{props.label}</span>
      {props.children}
      {props.needs && <span className="field-needs">{props.needs}</span>}
      {props.hint && <span className="field-hint">{props.hint}</span>}
    </label>
  );
}

/** A talkgroup list's hint: what's wrong with it (an entry that isn't a talkgroup), else `text`. */
export function TalkgroupHint(props: { list: readonly unknown[] | undefined; text?: ReactNode }) {
  const problem = talkgroupProblem(props.list ?? []);
  return problem ? <span className="bad-text">{problem}</span> : <>{props.text}</>;
}

/** A checkbox with its label and hint. */
export function Toggle(props: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: ReactNode; disabled?: boolean; id?: string }) {
  return (
    <label className={`toggle${props.disabled ? " disabled" : ""}`}>
      <input id={props.id} type="checkbox" checked={props.checked} disabled={props.disabled} onChange={(e) => props.onChange(e.target.checked)} />
      <span className="toggle-text">
        <span>{props.label}</span>
        {props.hint && <span className="field-hint">{props.hint}</span>}
      </span>
    </label>
  );
}

/**
 * A hidden file input, opened with `inputRef.current?.click()` from a button.
 * Cleared after each pick, so choosing the same file again reads it again.
 */
export function FileInput(props: { inputRef: RefObject<HTMLInputElement>; accept?: string; onFile: (f: File | undefined) => void }) {
  return (
    <input
      ref={props.inputRef}
      type="file"
      accept={props.accept}
      hidden
      onChange={(e) => {
        const f = e.target.files?.[0];
        e.target.value = "";
        props.onFile(f);
      }}
    />
  );
}

type InputProps = Omit<InputHTMLAttributes<HTMLInputElement>, "value" | "defaultValue" | "onChange">;

/**
 * A text input for a value: shows `codec.format(value)`; while focused, the
 * text as typed, each change read with `codec.parse` and passed on.
 */
export function DraftInput<T>(props: InputProps & { value: T; codec: TextCodec<T>; onChange: (v: T) => void }) {
  const { value, codec, onChange, onFocus, onBlur, ...rest } = props;
  const [text, setText] = useState<string | null>(null);
  return (
    <input
      {...rest}
      value={text ?? codec.format(value)}
      onFocus={(e) => {
        setText(codec.format(value));
        onFocus?.(e);
      }}
      onBlur={(e) => {
        setText(null);
        onBlur?.(e);
      }}
      onChange={(e) => {
        setText(e.target.value);
        onChange(codec.parse(e.target.value));
      }}
    />
  );
}

/**
 * A number (blank = undefined), in monospace. `hex` reads and shows it in
 * hex; `gain` shows it to a tenth. The caller clamps or defaults it: the text
 * typed stays until the field is left.
 */
export function NumberInput(props: Omit<InputProps, "className"> & { value: number | undefined; onChange: (v: number | undefined) => void; label: string; hex?: boolean; gain?: boolean; narrow?: boolean }) {
  const { label, hex: isHex, gain: isGain, narrow, ...rest } = props;
  return <DraftInput {...rest} className={`mono${narrow ? " narrow" : ""}`} aria-label={label} codec={isHex ? hex : isGain ? gain : decimal} />;
}
