// The setup's form controls: a labelled field, a checkbox toggle, a hidden
// file picker, and text inputs that keep what is typed. Each text input is a
// DraftInput with a codec from fieldText.ts (TextInput, TextArea and
// NumberInput are ones), so typing never fights a value being clamped,
// defaulted or tidied (a squelch of 12 typed as "1", then "12"; "-" on the way
// to -5; a trailing comma in a list), and what's typed is kept once, when the
// field is left or Enter pressed (Escape: back as it was), not per letter.
// Left, the field shows the value kept, tidied.
// The rules for using them: docs/developer/ui-style.md.

import { useEffect, useRef, useState, type InputHTMLAttributes, type KeyboardEvent, type ReactNode, type RefObject, type TextareaHTMLAttributes } from "react";
import { Info } from "./charts.tsx";
import { hasBiasT, SOURCE_KINDS } from "./config.ts";
import { asTyped, decimal, finishDraft, gain, hex, talkgroupProblem, type TextCodec } from "./fieldText.ts";
import type { Source } from "./protocol.ts";

/** A labelled setting. `needs`: what an import left to fill in here (highlighted); `anchor`: its place, for the to-do list's Show; `info`: the longer why, as an ⓘ beside the label. */
export function Field(props: { label: string; hint?: ReactNode; info?: ReactNode; children: ReactNode; wide?: boolean; needs?: string; anchor?: string; className?: string }) {
  return (
    <label className={`field${props.wide ? " wide" : ""}${props.needs ? " needs" : ""}${props.className ? ` ${props.className}` : ""}`} id={props.anchor && `need-${props.anchor}`}>
      <span className="field-label">
        {props.label}
        {props.info && <LabelInfo label={props.label}>{props.info}</LabelInfo>}
      </span>
      {props.children}
      {props.needs && <span className="field-needs">{props.needs}</span>}
      {props.hint && <span className="field-hint">{props.hint}</span>}
    </label>
  );
}

/** An ⓘ inside a label: a click on it opens nothing else (not the label's input). */
function LabelInfo(props: { label: string; children: ReactNode }) {
  return (
    <span onClick={(e) => e.preventDefault()}>
      <Info label={props.label}>{props.children}</Info>
    </span>
  );
}

/** A talkgroup list's hint: what's wrong with it (an entry that isn't a talkgroup), else `text`. */
export function TalkgroupHint(props: { list: readonly unknown[] | undefined; text?: ReactNode }) {
  const problem = talkgroupProblem(props.list ?? []);
  return problem ? <span className="bad-text">{problem}</span> : <>{props.text}</>;
}

/** A checkbox with its label and hint; `info`: the longer why, as an ⓘ beside the label. */
export function Toggle(props: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: ReactNode; info?: ReactNode; disabled?: boolean; id?: string }) {
  return (
    <label className={`toggle${props.disabled ? " disabled" : ""}`}>
      <input id={props.id} type="checkbox" checked={props.checked} disabled={props.disabled} onChange={(e) => props.onChange(e.target.checked)} />
      <span className="toggle-text">
        <span>
          {props.label}
          {props.info && <LabelInfo label={props.label}>{props.info}</LabelInfo>}
        </span>
        {props.hint && <span className="field-hint">{props.hint}</span>}
      </span>
    </label>
  );
}

/** A radio's bias-T switch, wherever it's picked (Setup, the setup guide, a scan's spare radio); none for a radio without one. */
export function BiasT(props: { src: Source; checked: boolean; onChange: (v: boolean) => void }) {
  return hasBiasT(props.src) ? <Toggle label="Bias-T" hint={SOURCE_KINDS[props.src.type].biasT} checked={props.checked} onChange={props.onChange} /> : null;
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
 * A field's text while it's being typed in, and keeping it when it's done:
 * left, Enter (`enter`), or gone with its page. Escape: back as it was.
 */
function useDraft<T>(value: T, codec: TextCodec<T>, onChange: (v: T) => void) {
  const [text, setText] = useState<string | null>(null);
  // (The latest of each, for the handlers and the page going away.)
  const at = useRef({ text: null as string | null, value, codec, onChange });
  Object.assign(at.current, { value, codec, onChange });
  const edit = (t: string | null) => {
    at.current.text = t;
    setText(t);
  };
  const finish = () => {
    const { text, value, codec, onChange } = at.current;
    at.current.text = null;
    const kept = finishDraft(text, value, codec);
    if (kept) onChange(kept.value);
  };
  useEffect(() => finish, []);
  return {
    shown: text ?? codec.format(value),
    focus: () => edit(codec.format(value)),
    type: (t: string) => edit(t),
    blur: () => {
      finish();
      setText(null);
    },
    key: (e: KeyboardEvent<HTMLInputElement | HTMLTextAreaElement>, enter: boolean) => {
      if (e.key === "Escape") {
        edit(null);
        e.currentTarget.blur();
      } else if (e.key === "Enter" && enter) e.currentTarget.blur();
    },
  };
}

/**
 * A text input for a value: shows `codec.format(value)`; while focused, the
 * text as typed, read with `codec.parse` and passed on once the field is left
 * or Enter pressed. `sizeToText`: as wide as its text.
 */
export function DraftInput<T>(props: InputProps & { value: T; codec: TextCodec<T>; onChange: (v: T) => void; sizeToText?: boolean }) {
  const { value, codec, onChange, onFocus, onBlur, onKeyDown, sizeToText, ...rest } = props;
  const d = useDraft(value, codec, onChange);
  return (
    <input
      {...rest}
      size={sizeToText ? Math.max(1, d.shown.length) : rest.size}
      value={d.shown}
      onFocus={(e) => {
        d.focus();
        onFocus?.(e);
      }}
      onBlur={(e) => {
        d.blur();
        onBlur?.(e);
      }}
      onChange={(e) => d.type(e.target.value)}
      onKeyDown={(e) => {
        d.key(e, true);
        onKeyDown?.(e);
      }}
    />
  );
}

/** A setting's text (kept trimmed), as DraftInput keeps it. */
export function TextInput(props: InputProps & { value: string; onChange: (v: string) => void }) {
  return <DraftInput {...props} codec={asTyped} />;
}

/** A setting's lines of text (kept trimmed): kept once the box is left, or Ctrl/⌘+Enter. */
export function TextArea(props: Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, "value" | "defaultValue" | "onChange"> & { value: string; onChange: (v: string) => void }) {
  const { value, onChange, onFocus, onBlur, onKeyDown, ...rest } = props;
  const d = useDraft(value, asTyped, onChange);
  return (
    <textarea
      {...rest}
      value={d.shown}
      onFocus={(e) => {
        d.focus();
        onFocus?.(e);
      }}
      onBlur={(e) => {
        d.blur();
        onBlur?.(e);
      }}
      onChange={(e) => d.type(e.target.value)}
      onKeyDown={(e) => {
        d.key(e, false);
        if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) e.currentTarget.blur();
        onKeyDown?.(e);
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

/**
 * A slider: moves as it's dragged, saved once it's let go (the pointer up, a
 * key up, or leaving it) — one save per setting, not one per step dragged
 * through: a radio's gain is set while recording, and a plugin's settings are
 * sent to it (or it's restarted) at each save. `text`: its value as shown.
 */
export function ReleaseSlider(props: {
  min?: number;
  max: number;
  step?: number;
  value: number;
  disabled?: boolean;
  label: string;
  id?: string;
  className?: string;
  text?: (v: number) => string;
  onChange: (v: number) => void;
}) {
  const [draft, setDraft] = useState<number | null>(null);
  const shown = draft ?? props.value;
  const text = props.text ? props.text(shown) : String(shown);
  const done = () => {
    if (draft !== null && draft !== props.value) props.onChange(draft);
    setDraft(null);
  };
  return (
    <span className={props.className ?? "row"}>
      <input
        id={props.id}
        type="range"
        min={props.min ?? 0}
        max={props.max}
        step={props.step}
        value={shown}
        disabled={props.disabled}
        aria-label={props.label}
        aria-valuetext={text}
        onChange={(e) => setDraft(Number(e.target.value))}
        onPointerUp={done}
        onKeyUp={done}
        onBlur={done}
      />
      <span className="mono small">{text}</span>
    </span>
  );
}

/** A button that copies `text` to the clipboard, and says so for a moment. */
export function CopyButton(props: { text: string; className?: string }) {
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(props.text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <button type="button" className={props.className ?? "btn small"} onClick={copy}>
      {copied ? "Copied" : "Copy"}
    </button>
  );
}
