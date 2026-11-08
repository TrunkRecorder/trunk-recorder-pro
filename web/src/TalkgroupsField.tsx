// A system's names (Setup → Systems): its talkgroups — a Trunk Recorder
// talkgroup CSV loaded from a file, or the talkgroup table copied from
// RadioReference's web page and pasted in — and its own names for its radios
// (Trunk Recorder's unitTagsFile). Each system keeps its own, saved as soon
// as they change, in the recorder's store; a file is read and written by the
// recorder's own code (talkgroups.ts, units.ts). Each row says where its
// names came from; loading over names edited by hand asks first.

import { useMemo, useRef, useState } from "react";
import { downloadText, mayReplaceNames, setNotice, setTalkgroups, setUnitNames, useApp } from "./controller.ts";
import { Field, FileInput } from "./inputs.tsx";
import { ORIGIN_SHORT, originCounts } from "./origins.ts";
import type { Origin, OriginFrom, System, UnitNames } from "./protocol.ts";
import type { TalkgroupFileReport, UnitNamesFileReport } from "./rules.ts";
import { isEncryptedMode, parseRadioReferencePaste, readTalkgroupFile, talkgroupFileText, type Talkgroup } from "./talkgroups.ts";
import { isPattern, readUnitNamesFile, unitNamesFileText } from "./units.ts";

/** A system with a talkgroup file: trunked (its first site's identity helps find it on RadioReference), or conventional. */
type TalkgroupsOf = Pick<System, "shortName"> & Partial<Pick<System, "sites">>;

/** Where a list's names came from, a chip each way ("212 RadioReference", "3 edited"): said only when they came more than one way, or were edited. */
function OriginChips(props: { rows: { origin?: Origin }[] }) {
  const counts = originCounts(props.rows);
  if (counts.length < 2 && !counts.some(([k]) => k === "edited")) return null;
  return (
    <>
      {counts.map(([k, n]) => (
        <span key={k} className="chip" title="Where these names came from (each one's last change)">
          {n} {ORIGIN_SHORT[k]}
        </span>
      ))}
    </>
  );
}

export function TalkgroupsField(props: { sys: TalkgroupsOf; needs?: string; anchor?: string }) {
  const { sys } = props;
  const tgs = useApp().talkgroups[sys.shortName] ?? [];
  const fileRef = useRef<HTMLInputElement>(null);
  const [pasting, setPasting] = useState(false);
  // What the file loaded last held that its rows don't say (until the talkgroups are replaced).
  const [loaded, setLoaded] = useState<TalkgroupFileReport | null>(null);
  const ignored = tgs.filter((t) => t.ignore).length;
  const encrypted = tgs.filter((t) => isEncryptedMode(t.mode)).length;
  // Columns kept from the file that Trunk Recorder doesn't know.
  const unknownColumns = useMemo(() => [...new Set(tgs.flatMap((t) => (t.extra ?? []).map(([k]) => k)))], [tgs]);
  const report = loaded && tgs.length ? loaded : null;
  /** The talkgroups are `next`, from `from` (none: cleared), unless that loses edits and the user says no. */
  const set = (next: Talkgroup[], from: OriginFrom | undefined, what: string, file: TalkgroupFileReport | null = null): boolean => {
    if (next.length && !mayReplaceNames("talkgroups", sys.shortName, next, what)) return false;
    setLoaded(file);
    setTalkgroups(sys.shortName, next, from);
    return true;
  };

  const onFile = async (f: File | undefined) => {
    if (!f) return;
    const r = readTalkgroupFile(await f.text());
    if (!r.talkgroups.length) return setNotice(`${f.name}: no talkgroups found.`);
    if (set(r.talkgroups, { kind: "file", name: f.name }, f.name, r))
      setNotice(`Loaded ${r.talkgroups.length} talkgroups from ${f.name} into ${sys.shortName}.${r.skipped.length ? ` ${r.skipped.length} row(s) without a number skipped.` : ""}`);
  };

  return (
    <div className={`field wide tg-field${props.needs ? " needs" : ""}`} id={props.anchor && `need-${props.anchor}`}>
      <span className="field-label">Talkgroups</span>
      <div className="row">
        <button className="btn" onClick={() => fileRef.current?.click()}>
          Load CSV…
        </button>
        <button className="btn" aria-expanded={pasting} onClick={() => setPasting(!pasting)}>
          Paste from RadioReference…
        </button>
        <span className="mono">{tgs.length ? `${tgs.length}` : "none"}</span>
        {tgs.length > 0 && (
          <>
            <button className="btn ghost" title="Trunk Recorder CSV, with a header row" onClick={() => downloadText(`${sys.shortName || "system"}.talkgroups.csv`, talkgroupFileText(tgs))}>
              Download
            </button>
            <button className="btn ghost" onClick={() => set([], undefined, "")}>
              Clear
            </button>
          </>
        )}
      </div>
      <FileInput inputRef={fileRef} accept=".csv,.tsv,.txt,text/csv,text/plain" onFile={(f) => void onFile(f)} />
      {tgs.length > 0 && (
        <div className="row tg-facts">
          <OriginChips rows={tgs} />
          {ignored > 0 && <span className="chip">{ignored} ignored</span>}
          {encrypted > 0 && <span className="chip" title="Not recorded">{encrypted} encrypted</span>}
          {report?.legacy && <span className="chip" title="Trunk Recorder's old fixed columns; Download adds a header row">no header row</span>}
          {report && report.missing.length > 0 && (
            <span className="chip warn" title="Trunk Recorder requires it; Download adds it">
              no {report.missing.join(" or ")} column
            </span>
          )}
          {unknownColumns.length > 0 && (
            <span className="chip warn" title="Trunk Recorder rejects these; kept for Download">
              unknown column{unknownColumns.length === 1 ? "" : "s"}: {unknownColumns.join(", ")}
            </span>
          )}
          {report && report.skipped.length > 0 && (
            <span className="chip warn" title={`Rows with no talkgroup number:\n${report.skipped.slice(0, 5).join("\n")}`}>
              {report.skipped.length} row{report.skipped.length === 1 ? "" : "s"} without a number
            </span>
          )}
        </div>
      )}
      {props.needs && <span className="field-needs">{props.needs}</span>}
      {pasting && (
        <RadioReferencePaste
          sys={sys}
          replacing={tgs.length}
          onUse={(list) => {
            if (!set(list, { kind: "radioReference" }, "The pasted table")) return;
            setPasting(false);
            setNotice(`Loaded ${list.length} talkgroups from RadioReference into ${sys.shortName}.`);
          }}
          onCancel={() => setPasting(false)}
        />
      )}
    </div>
  );
}

/**
 * A system's own names for its radios (Trunk Recorder's unitTagsFile, kept
 * as rows) and which names come first (its unitTagsMode, in the config:
 * `value`). Radios are named one at a time on their Radio page.
 */
export function UnitNamesField(props: { system: string; value: UnitNames | undefined; onChange: (u: UnitNames | undefined) => void; anchor: string; needs?: string }) {
  const rows = useApp().unitNames[props.system] ?? [];
  const ref = useRef<HTMLInputElement>(null);
  // The patterns the file loaded last held that aren't regular expressions (until the names are replaced).
  const [loaded, setLoaded] = useState<UnitNamesFileReport | null>(null);
  const patterns = rows.filter(isPattern).length;
  const bad = loaded && rows.length ? loaded.bad : [];
  const mode = props.value?.mode ?? "";
  return (
    <Field label="Unit names" needs={props.needs} anchor={props.anchor}>
      <div className="row">
        <button className="btn" onClick={() => ref.current?.click()}>
          Load CSV…
        </button>
        <span className="mono">{rows.length ? `${rows.length - patterns} radio${rows.length - patterns === 1 ? "" : "s"}${patterns ? `, ${patterns} pattern${patterns === 1 ? "" : "s"}` : ""}` : "none"}</span>
        {rows.length > 0 && (
          <>
            <button className="btn ghost" title="Trunk Recorder's unitTagsFile: unit,name lines" onClick={() => downloadText(`${props.system || "system"}.units.csv`, unitNamesFileText(rows))}>
              Download
            </button>
            <button
              className="btn ghost"
              onClick={() => {
                setLoaded(null);
                setUnitNames(props.system, []);
              }}
            >
              Clear
            </button>
          </>
        )}
        <select value={mode} aria-label="Which names come first" onChange={(e) => props.onChange(e.target.value ? { mode: e.target.value as UnitNames["mode"] } : undefined)}>
          <option value="">These first, then aliases heard</option>
          <option value="ota">Aliases heard first</option>
          <option value="user_only">Only these</option>
          <option value="none">No names</option>
        </select>
      </div>
      {rows.length > 0 && (
        <div className="row tg-facts">
          <OriginChips rows={rows} />
          {bad.length > 0 && (
            <span className="chip warn" title={bad.slice(0, 5).join("\n")}>
              {bad.length} pattern{bad.length === 1 ? "" : "s"} that name nothing (not a regular expression)
            </span>
          )}
        </div>
      )}
      <FileInput
        inputRef={ref}
        accept=".csv,text/csv,text/plain"
        onFile={async (f) => {
          if (!f) return;
          const r = readUnitNamesFile(await f.text());
          if (!r.unitNames.length) return setNotice(`${f.name}: no unit names found.`);
          if (!mayReplaceNames("unitNames", props.system, r.unitNames, f.name)) return;
          setLoaded(r);
          setUnitNames(props.system, r.unitNames, { kind: "file", name: f.name });
          setNotice(`Loaded ${r.unitNames.length} unit names from ${f.name} into ${props.system}.`);
        }}
      />
    </Field>
  );
}

/** Where to copy from, the box to paste into, and what was read from it. */
function RadioReferencePaste(props: { sys: TalkgroupsOf; replacing: number; onUse: (list: Talkgroup[]) => void; onCancel: () => void }) {
  const [text, setText] = useState("");
  const { sysId, wacn } = props.sys.sites?.[0]?.expect ?? {};
  const hex = (n: number) => n.toString(16).toUpperCase();
  const parsed = useMemo(() => parseRadioReferencePaste(text), [text]);
  const shown = 8;
  return (
    <div className="tg-paste">
      <p className="small">
        Copy the talkgroup tables from{" "}
        <a href="https://www.radioreference.com/db/browse/" target="_blank" rel="noreferrer">
          RadioReference
        </a>{" "}
        and paste them here.
        {sysId != null && (
          <span className="muted">
            {" "}
            This system: <span className="mono">System ID {hex(sysId)}</span>
            {wacn != null && <span className="mono">, WACN {hex(wacn)}</span>}.
          </span>
        )}
      </p>
      <textarea
        className="mono tg-paste-box"
        value={text}
        aria-label="RadioReference talkgroup table"
        placeholder={"Fire Dispatch\nDEC\tHEX\tMode\tAlpha Tag\tDescription\tTag\n1201\t4b1\tD\tFD Disp\tFire Dispatch\tFire Dispatch"}
        onChange={(e) => setText(e.target.value)}
      />
      {text.trim() && !parsed.length && <span className="field-needs">No talkgroups found.</span>}
      {parsed.length > 0 && (
        <>
          <div className="table-wrap">
            <table className="tg-preview">
              <thead>
                <tr>
                  <th>DEC</th>
                  <th>Mode</th>
                  <th>Alpha Tag</th>
                  <th>Description</th>
                  <th>Tag</th>
                  <th>Category</th>
                </tr>
              </thead>
              <tbody>
                {parsed.slice(0, shown).map((t) => (
                  <tr key={t.number}>
                    <td className="mono">{t.number}</td>
                    <td className="mono">{t.mode}</td>
                    <td>
                      <b>{t.alphaTag}</b>
                    </td>
                    <td>{t.description}</td>
                    <td className="muted">{t.tag}</td>
                    <td className="muted">{t.category}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <span className="muted small">
            {parsed.length > shown ? `…and ${parsed.length - shown} more. ` : ""}
            {summary(parsed)}
          </span>
        </>
      )}
      <div className="row">
        <span className="spacer" />
        <button className="btn ghost" onClick={props.onCancel}>
          Cancel
        </button>
        <button className="btn primary" disabled={!parsed.length} onClick={() => props.onUse(parsed)}>
          {props.replacing && parsed.length ? `Replace ${props.replacing} with ${parsed.length}` : `Use these ${parsed.length || ""}`}
        </button>
      </div>
    </div>
  );
}

function summary(list: Talkgroup[]): string {
  const cats = new Set(list.map((t) => t.category).filter(Boolean)).size;
  const enc = list.filter((t) => isEncryptedMode(t.mode)).length;
  return [`${list.length} talkgroup${list.length === 1 ? "" : "s"}`, cats ? `in ${cats} categor${cats === 1 ? "y" : "ies"}` : "", enc ? `${enc} encrypted (not recorded)` : ""].filter(Boolean).join(", ") + ".";
}
