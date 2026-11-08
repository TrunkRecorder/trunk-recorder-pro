// Setup → Data's tools: each kind of data the store keeps, exported as CSV
// files (every system's or one's) and deleted; and the question asked when
// a system is removed, whether its data goes too.

import { useEffect, useState } from "react";
import { compactData, DATA_KINDS, dataSummary, deleteData, exportUrl, setNotice, web } from "./controller.ts";
import { bytes } from "./fmt.ts";
import { Modal } from "./Identify.tsx";
import { Group } from "./SetupLayout.tsx";
import { Field, Toggle } from "./inputs.tsx";
import type { DataCounts, DataKind, DataSystem, DataTable } from "./protocol.ts";

/** Each kind of data, as the Data tab lists it: what it's called, what it is, how many there are, and its tables. */
const KINDS: { kind: DataKind; label: string; hint: string; count: (c: DataCounts) => string; tables: { table: DataTable; label: string }[] }[] = [
  {
    kind: "calls",
    label: "Calls",
    hint: "The call index: each call, and each radio's transmissions in it. Deleting it leaves the recordings on disk, and they aren't indexed again.",
    count: (c) => `${c.calls.toLocaleString()} calls`,
    tables: [
      { table: "calls", label: "Calls" },
      { table: "transmissions", label: "Transmissions" },
    ],
  },
  {
    kind: "talkgroups",
    label: "Talkgroups",
    hint: "The talkgroups each system was given (loaded from a file or edited), each with where its names came from.",
    count: (c) => `${c.talkgroups.toLocaleString()} talkgroups`,
    tables: [{ table: "talkgroups", label: "Talkgroups" }],
  },
  {
    kind: "unitNames",
    label: "Unit names",
    hint: "The names each system was given for its radios (loaded from a file or edited), each with where it came from.",
    count: (c) => `${c.unitNames.toLocaleString()} names`,
    tables: [{ table: "unit-names", label: "Unit names" }],
  },
  {
    kind: "talkerAliases",
    label: "Talker aliases",
    hint: "Radios' names, as heard over the air.",
    count: (c) => `${c.talkerAliases.toLocaleString()} aliases`,
    tables: [{ table: "talker-aliases", label: "Talker aliases" }],
  },
  {
    kind: "registry",
    label: "Radio registry",
    hint: "Every radio and talkgroup heard: when first and last, how often, and the pairs and frequencies (the Radio page, first-heard alerts).",
    count: (c) => `${c.radios.toLocaleString()} radios, ${c.heardTalkgroups.toLocaleString()} talkgroups`,
    tables: [
      { table: "radios", label: "Radios" },
      { table: "heard-talkgroups", label: "Talkgroups heard" },
    ],
  },
  {
    kind: "dashboardHistory",
    label: "Dashboard history",
    hint: "The dashboard's charts, by the hour. All systems: the computer's and the radios' too.",
    count: (c) => `${c.historySeries.toLocaleString()} series`,
    tables: [{ table: "dashboard-history", label: "Dashboard history" }],
  },
];

const ZERO: DataCounts = { calls: 0, talkgroups: 0, unitNames: 0, talkerAliases: 0, radios: 0, heardTalkgroups: 0, historySeries: 0 };

/** Every system's counts added up. */
function total(systems: DataSystem[]): DataCounts {
  const out = { ...ZERO };
  for (const s of systems) for (const k of Object.keys(ZERO) as (keyof DataCounts)[]) out[k] += s[k];
  return out;
}

/** What went, in words ("1,204 calls, 210 talkgroups"); "nothing" when nothing did. */
export function deletedText(d: DataCounts): string {
  const parts = [
    [d.calls, "call", "calls"],
    [d.talkgroups, "talkgroup", "talkgroups"],
    [d.unitNames, "unit name", "unit names"],
    [d.talkerAliases, "talker alias", "talker aliases"],
    [d.radios, "radio", "radios"],
    [d.heardTalkgroups, "talkgroup heard", "talkgroups heard"],
    [d.historySeries, "dashboard series", "dashboard series"],
  ] as const;
  const said = parts.filter(([n]) => n > 0).map(([n, one, many]) => `${n.toLocaleString()} ${n === 1 ? one : many}`);
  return said.length ? said.join(", ") : "nothing";
}

/** The Data tab's card: pick every system or one, then export each kind's tables or delete kinds of it. */
export function ManageData() {
  const [systems, setSystems] = useState<DataSystem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [scope, setScope] = useState<number | "all">("all");
  const [marked, setMarked] = useState<DataKind[]>([]);
  const [busy, setBusy] = useState(false);
  const [said, setSaid] = useState<string | null>(null);
  const [compacting, setCompacting] = useState(false);
  const [compacted, setCompacted] = useState<string | null>(null);
  const compact = () => {
    if (!confirm("Compact the store? It's rewritten without the space deleted data left, which takes a while on a big one (minutes on a Pi). Calls are saved as ever; the store catches up once it's done.")) return;
    setCompacting(true);
    setCompacted(null);
    void compactData().then((m) => {
      setCompacting(false);
      setCompacted(m.type === "dataCompacted" ? (m.error ? `Not compacted: ${m.error}` : `Compacted: ${bytes(m.beforeBytes)} → ${bytes(m.afterBytes)}.`) : `Not compacted: ${m.message}`);
    });
  };
  const load = () =>
    void dataSummary().then((m) => {
      if (m.type === "dataSummary") {
        setSystems(m.systems);
        setError(m.error ?? null);
      } else setError(m.message);
    });
  useEffect(load, []);
  const one = scope === "all" ? undefined : systems?.find((s) => s.id === scope);
  // (A system deleted from the store meanwhile: back to all of them.)
  useEffect(() => {
    if (systems && scope !== "all" && !one) setScope("all");
  }, [systems, scope, one]);
  const counts = one ?? total(systems ?? []);
  const what = one ? one.shortName : "every system";
  const remove = () => {
    const kinds = DATA_KINDS.filter((k) => marked.includes(k));
    const names = KINDS.filter((k) => kinds.includes(k.kind)).map((k) => k.label.toLowerCase());
    if (!kinds.length || !confirm(`Delete the ${names.join(", ")} of ${what}? This can't be undone (export them first to keep a copy). Recordings stay on disk.`)) return;
    setBusy(true);
    setSaid(null);
    void deleteData(kinds, one?.id).then((m) => {
      setBusy(false);
      setMarked([]);
      setSaid(m.type === "dataDeleted" ? `Deleted ${deletedText(m.deleted)}${m.error ? `. ${m.error}` : "."}` : `Not deleted: ${m.message}`);
      load();
    });
  };
  return (
    <section className="set-card">
      <Group title="Export and delete" hint="What the store keeps, a CSV file a table (named for the system, the table and the time), or deleted. Every system's, or one's: a system removed from the config is listed while its data is kept." full>
        <Field label="Systems" hint={error ?? undefined}>
          <select value={scope} onChange={(e) => setScope(e.target.value === "all" ? "all" : Number(e.target.value))} disabled={!systems}>
            <option value="all">All systems</option>
            {(systems ?? []).map((s) => (
              <option key={s.id} value={s.id}>
                {s.shortName}
                {s.inConfig ? "" : " (removed)"}
              </option>
            ))}
          </select>
        </Field>
        <div className="data-kinds wide">
          {KINDS.map((k) => (
            <div className="data-kind" key={k.kind}>
              <Toggle
                label={k.label}
                hint={
                  <>
                    {k.hint} <span className="mono">{systems ? k.count(counts) : "…"}</span>
                  </>
                }
                checked={marked.includes(k.kind)}
                disabled={busy}
                onChange={(v) => setMarked((m) => (v ? [...m, k.kind] : m.filter((x) => x !== k.kind)))}
              />
              <div className="row data-exports">
                {k.tables.map((t) => (
                  <a key={t.table} className="btn ghost small" href={exportUrl(t.table, one?.id)} download title={`${t.label} of ${what}, as a CSV file`}>
                    Export {t.label.toLowerCase()}
                  </a>
                ))}
              </div>
            </div>
          ))}
        </div>
        <div className="row wide">
          {said && <span className="muted small">{said}</span>}
          <span className="spacer" />
          <button className="btn danger" disabled={busy || !marked.length} onClick={remove}>
            {busy ? "Deleting…" : `Delete what's ticked${one ? ` of ${one.shortName}` : ""}…`}
          </button>
        </div>
        <Field label="Compact the store" hint="Deleting data (or calls aging out) leaves free space inside the store that it reuses but doesn't give back to the disk. Compacting rewrites the store without it. Every system's: the whole store." wide>
          <div className="row">
            <button className="btn ghost" disabled={compacting || busy} onClick={compact}>
              {compacting ? "Compacting…" : "Compact DB…"}
            </button>
            {compacted && <span className="muted small">{compacted}</span>}
          </div>
        </Field>
      </Group>
    </section>
  );
}

/**
 * Asks before a system is removed, and whether its data in the store goes
 * with it (yes unless unticked; the browser build keeps none). `id`: the
 * system's (none: never saved, so nothing is stored of it).
 */
export function RemoveSystem(props: { name: string; question: string; id: number | undefined; onRemove: () => void; onClose: () => void }) {
  const [withData, setWithData] = useState(true);
  const stored = !web && props.id !== undefined;
  const go = () => {
    props.onClose();
    props.onRemove();
    const id = props.id;
    if (!stored || !withData || id === undefined) return;
    void deleteData(DATA_KINDS, id).then((m) =>
      setNotice(m.type === "dataDeleted" ? `Removed ${props.name} and deleted ${deletedText(m.deleted)}${m.error ? `. ${m.error}` : "."}` : `Removed ${props.name}; its data wasn't deleted: ${m.message}`),
    );
  };
  return (
    <Modal title={`Remove ${props.name}?`} onClose={props.onClose}>
      <p>{props.question}</p>
      {stored && (
        <Toggle
          label="Also delete its data"
          hint="Its calls in the call index, talkgroups, talker aliases, radio registry and dashboard history. Recordings stay on disk. Unticked, they stay in the store: Setup → Data exports or deletes them later."
          checked={withData}
          onChange={setWithData}
        />
      )}
      <div className="row">
        <span className="spacer" />
        <button className="btn ghost" onClick={props.onClose}>
          Cancel
        </button>
        <button className="btn danger" onClick={go} autoFocus>
          Remove
        </button>
      </div>
    </Modal>
  );
}
