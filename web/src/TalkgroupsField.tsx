// A system's talkgroup names (Setup → Systems): a Trunk Recorder talkgroup
// CSV loaded from a file, or the talkgroup table copied from RadioReference's
// web page and pasted in. Each system keeps its own.

import { useMemo, useRef, useState } from "react";
import { downloadText, setNotice, updateConfig } from "./controller.ts";
import type { System } from "./protocol.ts";
import { isEncryptedMode, normalizeTalkgroupCsv, parseRadioReferencePaste, readTalkgroupCsv, talkgroupsToCsv, type Talkgroup } from "./talkgroups.ts";

const PASTED = "RadioReference (pasted)";

export function TalkgroupsField(props: { sys: System; index: number; needs?: string; anchor?: string }) {
  const { sys, index } = props;
  const fileRef = useRef<HTMLInputElement>(null);
  const [pasting, setPasting] = useState(false);
  const report = useMemo(() => readTalkgroupCsv(sys.talkgroupsCsv ?? ""), [sys.talkgroupsCsv]);
  const tgs = [...report.talkgroups.values()];
  const ignored = tgs.filter((t) => t.ignore).length;
  const encrypted = tgs.filter((t) => isEncryptedMode(t.mode)).length;
  const set = (csv: string, name: string) =>
    updateConfig((x) => {
      x.systems[index].talkgroupsCsv = csv;
      x.systems[index].talkgroupsName = name;
    });

  const onFile = async (f: File | undefined) => {
    if (!f) return;
    const csv = normalizeTalkgroupCsv(await f.text());
    const r = readTalkgroupCsv(csv);
    if (!r.talkgroups.size) return setNotice(`${f.name}: no talkgroups in it — each row needs a talkgroup number in its first (Decimal) column.`);
    set(csv, f.name);
    setNotice(`Loaded ${r.talkgroups.size} talkgroups from ${f.name} into ${sys.shortName}.${r.skipped.length ? ` ${r.skipped.length} row(s) without a number were passed over.` : ""}`);
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
        <span className="mono">{tgs.length ? `${tgs.length} from ${sys.talkgroupsName || "a file"}` : "none"}</span>
        {tgs.length > 0 && (
          <>
            <button className="btn ghost" title="As a Trunk Recorder talkgroup CSV" onClick={() => downloadText(csvName(sys), sys.talkgroupsCsv)}>
              Download
            </button>
            <button className="btn ghost" onClick={() => set("", "")}>
              Clear
            </button>
          </>
        )}
      </div>
      <input
        ref={fileRef}
        type="file"
        accept=".csv,.tsv,.txt,text/csv,text/plain"
        hidden
        onChange={(e) => {
          const f = e.target.files?.[0];
          e.target.value = "";
          void onFile(f);
        }}
      />
      {tgs.length > 0 && (
        <div className="row tg-facts">
          {ignored > 0 && <span className="chip">{ignored} ignored</span>}
          {encrypted > 0 && <span className="chip" title="Mode E, DE or TE: Trunk Recorder doesn't record them">{encrypted} encrypted</span>}
          {report.legacy && <span className="chip" title="Trunk Recorder now wants a header row; Download gives the file with one">no header row</span>}
          {report.missing.length > 0 && (
            <span className="chip warn" title="Trunk Recorder stops at a talkgroup file without them; this recorder doesn't mind">
              no {report.missing.join(" or ")} column
            </span>
          )}
          {report.unknownColumns.length > 0 && (
            <span className="chip warn" title="Trunk Recorder stops at a column it doesn't know; this recorder leaves it be">
              unknown column{report.unknownColumns.length === 1 ? "" : "s"}: {report.unknownColumns.join(", ")}
            </span>
          )}
          {report.skipped.length > 0 && (
            <span className="chip warn" title={`Rows with no talkgroup number:\n${report.skipped.slice(0, 5).join("\n")}`}>
              {report.skipped.length} row{report.skipped.length === 1 ? "" : "s"} without a number
            </span>
          )}
        </div>
      )}
      {props.needs && <span className="field-needs">{props.needs}</span>}
      <span className="field-hint">
        Trunk Recorder&apos;s talkgroup CSV (Decimal, Mode, Alpha Tag, Description, Tag, Category, Priority…; commas, semicolons or tabs). An Ignore column (true / yes / x)
        marks talkgroups never to record, as does Priority −1.
      </span>
      {pasting && (
        <RadioReferencePaste
          sys={sys}
          replacing={tgs.length}
          onUse={(list) => {
            set(talkgroupsToCsv(list), PASTED);
            setPasting(false);
            setNotice(`Loaded ${list.length} talkgroups from RadioReference into ${sys.shortName}.`);
          }}
          onCancel={() => setPasting(false)}
        />
      )}
    </div>
  );
}

const csvName = (sys: System) => (sys.talkgroupsName && sys.talkgroupsName !== PASTED && /\.csv$/i.test(sys.talkgroupsName) ? sys.talkgroupsName : `${sys.shortName || "talkgroups"}-talkgroups.csv`);
const hex = (n: number) => n.toString(16).toUpperCase();

/** Where to copy from, the box to paste into, and what was read from it. */
function RadioReferencePaste(props: { sys: System; replacing: number; onUse: (list: Talkgroup[]) => void; onCancel: () => void }) {
  const [text, setText] = useState("");
  const parsed = useMemo(() => parseRadioReferencePaste(text), [text]);
  const id = props.sys.expect;
  const shown = 8;
  return (
    <div className="tg-paste">
      <ol className="small">
        <li>
          Open{" "}
          <a href="https://www.radioreference.com/db/browse/" target="_blank" rel="noreferrer">
            RadioReference&apos;s database
          </a>{" "}
          and find this system (state, county, then the trunked system)
          {id.sysId != null && (
            <>
              {" "}
              — it&apos;s <b className="mono">System ID {hex(id.sysId)}</b>
              {id.wacn != null && (
                <>
                  , <b className="mono">WACN {hex(id.wacn)}</b>
                </>
              )}
            </>
          )}
          .
        </li>
        <li>Select the talkgroup tables, from the first category heading to the last talkgroup, and copy. Headings and column names can come along; the whole page works too.</li>
        <li>Paste below. Each category heading becomes the talkgroups&apos; Category.</li>
      </ol>
      <textarea
        className="mono tg-paste-box"
        value={text}
        aria-label="RadioReference talkgroup table"
        placeholder={"Fire Dispatch\nDEC\tHEX\tMode\tAlpha Tag\tDescription\tTag\n1201\t4b1\tD\tFD Disp\tFire Dispatch\tFire Dispatch"}
        onChange={(e) => setText(e.target.value)}
      />
      {text.trim() && !parsed.length && <span className="field-needs">That doesn&apos;t look like a talkgroup table — each row should start with a talkgroup number.</span>}
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
                    <td className="muted">{t.group}</td>
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
          {props.replacing && parsed.length ? `Replace the ${props.replacing} with these ${parsed.length}` : `Use these ${parsed.length || ""}`}
        </button>
      </div>
    </div>
  );
}

function summary(list: Talkgroup[]): string {
  const cats = new Set(list.map((t) => t.group).filter(Boolean)).size;
  const enc = list.filter((t) => isEncryptedMode(t.mode)).length;
  return [`${list.length} talkgroup${list.length === 1 ? "" : "s"}`, cats ? `in ${cats} categor${cats === 1 ? "y" : "ies"}` : "", enc ? `${enc} encrypted (not recorded)` : ""].filter(Boolean).join(", ") + ".";
}
