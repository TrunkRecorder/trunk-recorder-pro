// Folders and file names (filenameFormat), built by dragging tokens in: the
// call's fields, its start time, folder separators and plain text. The format
// itself (filenameFormat.ts) can still be typed or pasted, in either a
// macOS / Linux (`/`) or a Windows (`\`) style.

import { useMemo, useState } from "react";
import { useApp } from "./controller.ts";
import { formatToString, parseFormat, pieceProblem, pieceSample, samplePath, TEXT_NOT_ALLOWED, type Piece } from "./filenameFormat.ts";

/** Trunk Recorder's layout, as near as a format gets (its file name has `.<slot>` on TDMA calls). */
export const DEFAULT_FORMAT = "{short_name}/{time:%Y}/{time:%-m}/{time:%-d}/{talkgroup}-{epoch}_{freq}";

const TOKEN_LABEL: Record<string, string> = {
  talkgroup: "Talkgroup",
  talkgroup_alpha_tag: "Alpha tag",
  talkgroup_tag: "Tag",
  talkgroup_description: "Description",
  talkgroup_group: "Category",
  talkgroup_display: "Talkgroup",
  short_name: "System",
  freq: "Frequency, Hz",
  freq_mhz: "Frequency, MHz",
  epoch: "Start, Unix time",
  call_num: "Call number",
  tdma_slot: "TDMA slot",
  audio_type: "Audio type",
  emergency: "Emergency",
  encrypted: "Encrypted",
  priority: "Priority",
  sys_num: "System number",
  source_num: "Source number",
  recorder_num: "Recorder number",
  signal: "Signal, dB",
  noise: "Noise, dB",
  color_code: "Colour code",
};

const TIME_LABEL: Record<string, string> = {
  "%Y": "Year",
  "%m": "Month",
  "%-m": "Month",
  "%d": "Day",
  "%-d": "Day",
  "%H": "Hour",
  "%M": "Minute",
  "%S": "Second",
  "%f": "Millisecond",
  "%j": "Day of year",
  "%a": "Weekday",
  "%Y-%m-%d": "Date",
  "%H%M%S": "Time",
  "%H-%M-%S": "Time",
  iso: "ISO time",
  iso_ms: "ISO time, ms",
};

const GROUPS: { title: string; tokens: string[] }[] = [
  { title: "Talkgroup", tokens: ["talkgroup", "talkgroup_alpha_tag", "talkgroup_tag", "talkgroup_description", "talkgroup_group"] },
  { title: "Call", tokens: ["short_name", "freq", "freq_mhz", "epoch", "call_num", "tdma_slot", "audio_type", "emergency", "encrypted", "priority"] },
  { title: "Radio", tokens: ["sys_num", "source_num", "recorder_num", "signal", "noise", "color_code"] },
];
const TIMES = ["%Y", "%m", "%-m", "%d", "%-d", "%H", "%M", "%S", "%f", "%Y-%m-%d", "%H%M%S", "iso", "%j", "%a"];
const JOINERS = ["-", "_", ".", " "];

const PRESETS: { label: string; format: string }[] = [
  { label: "Trunk Recorder's layout", format: DEFAULT_FORMAT },
  { label: "By talkgroup, then day", format: "{short_name}/{talkgroup}-{talkgroup_alpha_tag}/{time:%Y-%m-%d}/{time:%H%M%S}_{freq}" },
  { label: "By day, named by talkgroup", format: "{short_name}/{time:%Y-%m-%d}/{talkgroup_alpha_tag}-{time:%H%M%S}" },
  { label: "By category (UTC)", format: "{short_name}/{ztime:%Y-%m-%d}/{talkgroup_group}/{talkgroup}-{ztime:iso}_{freq_mhz}" },
];

function chipLabel(p: Piece): string {
  if (p.kind === "sep") return "/";
  if (p.kind === "text") return p.text;
  if (p.kind === "time") {
    const name = TIME_LABEL[p.fmt] ?? p.fmt;
    const bare = p.fmt.startsWith("%-") ? " (no 0)" : "";
    return `${name}${bare}${p.utc ? " UTC" : ""}`;
  }
  return TOKEN_LABEL[p.name] ?? `{${p.name}}`;
}

function chipTitle(p: Piece, when: Date): string {
  if (p.kind === "sep") return "Folder";
  if (p.kind === "text") return "Text";
  const code = p.kind === "time" ? `{${p.utc ? "ztime" : "time"}:${p.fmt}}` : `{${p.name}}`;
  return `${code} — e.g. ${pieceSample(p, when) || "(empty)"}`;
}

const MIME = "application/x-trunk-filename";
type Drag = { piece: Piece } | { move: number };

/** Remove what a text box mustn't hold; `allowFolders`: let / and \ through (they become folders). */
function cleanText(s: string, allowFolders: boolean): { text: string; dropped: boolean } {
  const re = allowFolders ? /[:*?"<>|\u0000-\u001f]/g : TEXT_NOT_ALLOWED;
  const text = s.replace(re, "");
  return { text, dropped: text !== s };
}

/**
 * The filename format box. `value` blank: `fallback` is used (Trunk
 * Recorder's layout, or the Recording tab's for a system), shown as
 * Default; changing anything starts this one's own from it.
 */
export function FilenameEditor(props: { label: string; hint?: string; value: string; fallback: string; fallbackName: string; root?: string; onChange: (v: string) => void }) {
  const { value, fallback, onChange } = props;
  const [open, setOpen] = useState(false);
  const own = value.trim() !== "";
  const shown = own ? value : fallback;
  const { pieces, problems } = useMemo(() => parseFormat(shown), [shown]);
  const platform = useApp().host?.platform.os ?? "";
  const winGuess = /windows/i.test(platform) || /^[A-Za-z]:|\\/.test(props.root ?? "") || (!platform && /Win/.test(navigator.platform));
  const [style, setStyle] = useState<"posix" | "windows" | null>(null);
  const windows = (style ?? (winGuess ? "windows" : "posix")) === "windows";
  // A fixed moment, so the example doesn't tick.
  const when = useMemo(() => new Date(), []);
  const example = samplePath(pieces, when);
  const sep = windows ? "\\" : "/";
  const root = props.root?.replace(/[/\\]+$/, "");

  const write = (next: Piece[]) => onChange(formatToString(next));

  return (
    <div className="field wide fname">
      <span className="field-label">{props.label}</span>
      <div className={`fname-line${problems.length ? " invalid" : ""}`}>
        <div className="fname-chips readonly" aria-label="The format">
          {pieces.length ? pieces.map((p, k) => <Chip key={k} piece={p} when={when} />) : <span className="muted">—</span>}
          {!own && <span className="chip">Default ({props.fallbackName})</span>}
        </div>
        <button className="btn small" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "Done" : "Edit…"}
        </button>
      </div>
      <div className="fname-example">
        <span className="muted small">Example:</span>{" "}
        <code className="mono small">
          {root ? `${root}${sep}` : ""}
          {example.join(sep)}.wav
        </code>
        <span className="seg small" role="radiogroup" aria-label="Path style">
          <button className={windows ? "" : "on"} role="radio" aria-checked={!windows} onClick={() => setStyle("posix")}>
            macOS / Linux
          </button>
          <button className={windows ? "on" : ""} role="radio" aria-checked={windows} onClick={() => setStyle("windows")}>
            Windows
          </button>
        </span>
      </div>
      {problems.length > 0 && (
        <ul className="field-needs fname-problems">
          {problems.map((p) => (
            <li key={p}>{p}</li>
          ))}
        </ul>
      )}
      {props.hint && <span className="field-hint">{props.hint}</span>}
      {open && <Builder pieces={pieces} own={own} when={when} write={write} onChange={onChange} value={value} />}
    </div>
  );
}

function Chip(props: { piece: Piece; when: Date }) {
  const p = props.piece;
  const bad = pieceProblem(p);
  return (
    <span className={`fname-chip k-${p.kind}${bad ? " bad" : ""}`} title={bad ?? chipTitle(p, props.when)}>
      {p.kind === "text" ? <span className="fname-text">{p.text.replace(/ /g, "␣")}</span> : chipLabel(p)}
    </span>
  );
}

/** The editor: the format's chips, which can be dragged about, and the palette to drag new ones from. */
function Builder(props: { pieces: Piece[]; own: boolean; when: Date; value: string; write: (p: Piece[]) => void; onChange: (v: string) => void }) {
  const { pieces, write, when } = props;
  const [dropAt, setDropAt] = useState<number | null>(null);
  const [utc, setUtc] = useState(false);
  const [text, setText] = useState("");
  const [note, setNote] = useState("");
  const [raw, setRaw] = useState(false);

  const insert = (p: Piece, at = pieces.length) => write([...pieces.slice(0, at), p, ...pieces.slice(at)]);
  const remove = (k: number) => write(pieces.filter((_, j) => j !== k));
  const move = (from: number, to: number) => {
    const next = [...pieces];
    const [p] = next.splice(from, 1);
    next.splice(to > from ? to - 1 : to, 0, p);
    write(next);
  };
  const dragData = (e: React.DragEvent): Drag | null => {
    try {
      return JSON.parse(e.dataTransfer.getData(MIME)) as Drag;
    } catch {
      return null;
    }
  };
  const startDrag = (e: React.DragEvent, d: Drag) => {
    e.dataTransfer.setData(MIME, JSON.stringify(d));
    e.dataTransfer.effectAllowed = "move";
  };
  /** Where a drop over chip `k` goes: before it or after it, by which half the pointer is over. */
  const overChip = (e: React.DragEvent, k: number) => {
    e.preventDefault();
    e.stopPropagation();
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    setDropAt(e.clientX < r.left + r.width / 2 ? k : k + 1);
  };
  const drop = (e: React.DragEvent) => {
    e.preventDefault();
    const d = dragData(e);
    const at = dropAt ?? pieces.length;
    setDropAt(null);
    if (!d) return;
    if ("move" in d) move(d.move, at);
    else insert(d.piece, at);
  };
  const addText = () => {
    if (!text) return;
    insert({ kind: "text", text });
    setText("");
  };
  const paletteChip = (p: Piece, label = chipLabel(p), key?: string) => (
    <button
      key={key ?? label}
      className={`fname-chip k-${p.kind} palette`}
      draggable
      title={chipTitle(p, when)}
      onDragStart={(e) => startDrag(e, { piece: p })}
      onClick={() => insert(p)}
    >
      {label}
    </button>
  );

  return (
    <div className="fname-editor">
      <div
        className={`fname-chips build${dropAt !== null ? " dragging" : ""}`}
        onDragOver={(e) => {
          e.preventDefault();
          if (e.target === e.currentTarget) setDropAt(pieces.length);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropAt(null);
        }}
        onDrop={drop}
        aria-label="The format: drag to reorder, Delete to remove"
      >
        {pieces.map((p, k) => {
          const bad = pieceProblem(p);
          return (
            <span key={k} className="fname-slot" onDragOver={(e) => overChip(e, k)}>
              {dropAt === k && <span className="fname-caret" />}
              <span
                className={`fname-chip k-${p.kind}${bad ? " bad" : ""}`}
                draggable
                tabIndex={0}
                title={bad ?? chipTitle(p, when)}
                onDragStart={(e) => startDrag(e, { move: k })}
                onDragEnd={() => setDropAt(null)}
                onKeyDown={(e) => {
                  if (e.target !== e.currentTarget) return;
                  if (e.key === "Delete" || e.key === "Backspace") {
                    e.preventDefault();
                    remove(k);
                  } else if (e.altKey && e.key === "ArrowLeft" && k > 0) move(k, k - 1);
                  else if (e.altKey && e.key === "ArrowRight" && k < pieces.length - 1) move(k, k + 2);
                }}
              >
                {p.kind === "text" ? (
                  <input
                    className="fname-text-input"
                    value={p.text}
                    size={Math.max(1, p.text.length)}
                    aria-label="Text"
                    onChange={(e) => {
                      const c = cleanText(e.target.value, true);
                      if (c.dropped) setNote('Not allowed: : * ? " < > |');
                      // A / or \ typed here makes a folder.
                      const next = [...pieces];
                      next[k] = { kind: "text", text: c.text };
                      props.onChange(formatToString(next.filter((x) => x.kind !== "text" || x.text)));
                    }}
                  />
                ) : (
                  chipLabel(p)
                )}
                <button className="fname-x" aria-label="Remove" tabIndex={-1} onClick={() => remove(k)}>
                  ×
                </button>
              </span>
            </span>
          );
        })}
        {dropAt === pieces.length && <span className="fname-caret" />}
        {!pieces.length && <span className="muted small">Empty</span>}
      </div>

      <div
        className="fname-palette"
        onDragOver={(e) => e.preventDefault()}
        onDrop={(e) => {
          // A chip dragged back out of the format: removed.
          e.preventDefault();
          const d = dragData(e);
          setDropAt(null);
          if (d && "move" in d) remove(d.move);
        }}
      >
        <div className="fname-group">
          <span className="fname-group-title">Folders and joiners</span>
          {paletteChip({ kind: "sep" }, "/ folder")}
          {JOINERS.map((j) => paletteChip({ kind: "text", text: j }, j === " " ? "␣ space" : j))}
          <span className="fname-add">
            <input
              value={text}
              placeholder="your text"
              aria-label="Text to add"
              onChange={(e) => {
                const c = cleanText(e.target.value, false);
                setNote(c.dropped ? 'Not allowed: / \\ : * ? " < > | { }' : "");
                setText(c.text);
              }}
              onKeyDown={(e) => e.key === "Enter" && (e.preventDefault(), addText())}
            />
            <button className="btn small" disabled={!text} draggable={!!text} onDragStart={(e) => startDrag(e, { piece: { kind: "text", text } })} onClick={addText}>
              Add text
            </button>
          </span>
        </div>
        {GROUPS.map((g) => (
          <div key={g.title} className="fname-group">
            <span className="fname-group-title">{g.title}</span>
            {g.tokens.map((t) => paletteChip({ kind: "token", name: t }, undefined, t))}
          </div>
        ))}
        <div className="fname-group">
          <span className="fname-group-title">
            Start time
            <span className="seg small" role="radiogroup" aria-label="Time zone">
              <button className={utc ? "" : "on"} role="radio" aria-checked={!utc} onClick={() => setUtc(false)}>
                Local
              </button>
              <button className={utc ? "on" : ""} role="radio" aria-checked={utc} onClick={() => setUtc(true)}>
                UTC
              </button>
            </span>
          </span>
          {TIMES.map((f) => paletteChip({ kind: "time", utc, fmt: f }, `${TIME_LABEL[f]}${f.startsWith("%-") ? " (no 0)" : ""}`, f))}
        </div>
        {note && <p className="small warn-text">{note}</p>}
      </div>

      <div className="row fname-actions">
        <select
          value=""
          aria-label="Start from a layout"
          onChange={(e) => {
            if (e.target.value) props.onChange(e.target.value);
          }}
        >
          <option value="">Start from…</option>
          {PRESETS.map((p) => (
            <option key={p.label} value={p.format}>
              {p.label}
            </option>
          ))}
        </select>
        <button className="btn ghost small" onClick={() => setRaw(!raw)}>
          {raw ? "Hide text" : "Edit as text"}
        </button>
        <span className="spacer" />
        {props.own && (
          <button className="btn ghost small" onClick={() => props.onChange("")}>
            Reset to default
          </button>
        )}
      </div>
      {raw && (
        <input
          className="mono"
          aria-label="The format as text"
          value={props.value}
          placeholder="{short_name}/{time:%Y}/…"
          onChange={(e) => props.onChange(e.target.value)}
        />
      )}
    </div>
  );
}
