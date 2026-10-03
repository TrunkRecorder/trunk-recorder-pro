// The trunking view (desktop app): the voice channels in use now, and the
// control channels' messages as they arrive, like an Omnitrunker display.

import { useMemo, useState } from "react";
import { formatMhz } from "./config.ts";
import { clearTrunk, TRUNK_KEPT, useApp, type AppState } from "./controller.ts";
import { conventionalIndex, type CallView, type TrunkMessage } from "./protocol.ts";
import { parseTalkgroupCsv } from "./talkgroups.ts";

/** Message kinds as the view names them, in the type menu's order. */
const KINDS: [string, string][] = [
  ["grant", "Grant"],
  ["affiliation", "Affiliation"],
  ["registration", "Registration"],
  ["deregistration", "Deregistration"],
  ["location", "Location"],
  ["acknowledge", "Acknowledge"],
  ["data_grant", "Data grant"],
  ["uu_v_grant", "Unit-to-unit grant"],
  ["uu_ans_req", "Answer request"],
  ["call_alert", "Call alert"],
  ["patch_add", "Patch"],
  ["patch_delete", "Unpatch"],
  ["status", "Status"],
  ["sysid", "System ID"],
  ["control_channel", "Control channel"],
  ["adjacent", "Neighbour"],
];
const KIND_NAME = new Map(KINDS);
/** What the site broadcasts over and over; the rest is what radios do. */
const SITE = new Set(["status", "sysid", "control_channel", "adjacent"]);

/** Whether message kind `k` is shown by the type menu's choice `show`. */
function shows(show: string, k: string): boolean {
  return show === "all" || (show === "units" ? !SITE.has(k) : show === "site" ? SITE.has(k) : show === k);
}
const BUFFERS = [100, 200, 500, 1000, TRUNK_KEPT];

function clock(s: number): string {
  const m = Math.floor(s / 60);
  return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
}

/** Talkgroup names per system, from its talkgroup file. */
function useTalkgroupNames(s: AppState): (system: string, tg: number) => string {
  const files = useMemo(() => new Map((s.config?.systems ?? []).map((x) => [x.shortName, parseTalkgroupCsv(x.talkgroupsCsv)])), [s.config]);
  return (system, tg) => files.get(system)?.get(tg)?.alphaTag ?? "";
}

export function OmnitrunkerPage() {
  const s = useApp();
  const tgName = useTalkgroupNames(s);
  const unitName = (system: string, unit: number | undefined) => (unit === undefined ? "" : (s.units[system]?.[unit] ?? ""));
  const names = useMemo(() => {
    const c = s.config;
    if (!c) return [];
    return [...c.systems.map((x) => x.shortName), ...c.conventional.filter((x) => x.channels.length > 0 || x.channelFile).map((x) => x.shortName)];
  }, [s.config]);
  const [system, setSystem] = useState("");
  const [kind, setKind] = useState("units");
  const [buffer, setBuffer] = useState(200);
  // Paused: the list stays as it was (messages still arrive underneath).
  const [frozen, setFrozen] = useState<TrunkMessage[] | null>(null);

  const messages = useMemo(() => {
    const src = frozen ?? s.trunk;
    const out: TrunkMessage[] = [];
    for (let i = src.length - 1; i >= 0 && out.length < buffer; i--) {
      const m = src[i];
      if ((!system || m.system === system) && shows(kind, m.kind)) out.push(m);
    }
    return out;
  }, [frozen, s.trunk, system, kind, buffer]);

  const running = s.phase === "running";
  return (
    <div className="omni stack">
      <div className="stats-bar">
        <label className="field">
          <span className="field-label">System</span>
          <select value={system} onChange={(e) => setSystem(e.target.value)}>
            <option value="">All systems</option>
            {names.map((n) => (
              <option key={n}>{n}</option>
            ))}
          </select>
        </label>
      </div>
      {!running && <div className="banner subtle">The recorder is stopped: nothing is being heard.</div>}
      <VoiceChannels s={s} system={system} tgName={tgName} unitName={unitName} />
      <section className="panel">
        <header className="panel-head">
          <h2>Control channel</h2>
          <div className="row omni-controls">
            <select value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Message type">
              <option value="units">Unit activity</option>
              <option value="site">Site broadcasts</option>
              <option value="all">All messages</option>
              {KINDS.map(([k, label]) => (
                <option key={k} value={k}>
                  {label}
                </option>
              ))}
            </select>
            <select value={buffer} onChange={(e) => setBuffer(Number(e.target.value))} aria-label="Messages shown">
              {BUFFERS.map((b) => (
                <option key={b} value={b}>
                  Last {b}
                </option>
              ))}
            </select>
            <button className="btn ghost small" onClick={() => setFrozen(frozen ? null : s.trunk)}>
              {frozen ? "Resume" : "Pause"}
            </button>
            <button
              className="btn ghost small"
              onClick={() => {
                clearTrunk();
                setFrozen(null);
              }}
            >
              Clear
            </button>
          </div>
        </header>
        {messages.length === 0 ? (
          <p className="muted">{s.trunk.length ? "Nothing matches." : "Messages appear here as the control channels are decoded."}</p>
        ) : (
          <div className="table-scroll omni-messages">
            <table className="stat-table omni-table">
              <thead>
                <tr>
                  <th>Time</th>
                  <th>System</th>
                  <th>Site</th>
                  <th>Message</th>
                  <th className="num">Radio</th>
                  <th>Radio name</th>
                  <th className="num">Talkgroup</th>
                  <th>Talkgroup name</th>
                  <th>Channel</th>
                </tr>
              </thead>
              <tbody>
                {messages.map((m, i) => (
                  <tr key={`${m.time}-${i}`} className={m.emergency ? "omni-emergency" : ""}>
                    <td className="mono">{new Date(m.time * 1000).toLocaleTimeString()}</td>
                    <td>{m.system}</td>
                    <td className="mono">{m.site ?? ""}</td>
                    <td>
                      <span className={`omni-kind omni-kind-${m.kind}`}>{KIND_NAME.get(m.kind) ?? m.kind}</span>
                      {m.encrypted && <span className="omni-badge enc">Encrypted</span>}
                      {m.emergency && <span className="omni-badge emer">Emergency</span>}
                    </td>
                    <td className="num mono">{m.unit ?? ""}</td>
                    <td>{unitName(m.system, m.unit)}</td>
                    <td className="num mono">{m.talkgroup ?? ""}</td>
                    <td>{m.talkgroup ? tgName(m.system, m.talkgroup) : (m.text ?? "")}</td>
                    <td className="mono">
                      {m.freqHz ? formatMhz(m.freqHz) : ""}
                      {m.slot !== undefined ? <span className="muted"> · slot {m.slot}</span> : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}

function VoiceChannels(props: {
  s: AppState;
  system: string;
  tgName: (system: string, tg: number) => string;
  unitName: (system: string, unit: number | undefined) => string;
}) {
  const { s } = props;
  const now = s.status?.nowS ?? 0;
  const calls = s.calls.filter((c) => !props.system || c.systemName === props.system).sort((a, b) => a.freqHz - b.freqHz);
  const mode = (c: CallView) => (c.analog ? "FM" : c.slot !== null ? `TDMA ${c.slot}` : conventionalIndex(c.system) !== null ? "P25" : "FDMA");
  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Voice channels</h2>
        <span className="muted small">{calls.length ? `${calls.length} in use` : ""}</span>
      </header>
      {calls.length === 0 ? (
        <p className="muted">No voice channels in use.</p>
      ) : (
        <div className="table-scroll">
          <table className="stat-table omni-table">
            <thead>
              <tr>
                <th>System</th>
                <th>Channel</th>
                <th className="num">Talkgroup</th>
                <th>Talkgroup name</th>
                <th className="num">Radio</th>
                <th>Radio name</th>
                <th>Elapsed</th>
                <th>Mode</th>
                <th>State</th>
              </tr>
            </thead>
            <tbody>
              {calls.map((c) => {
                const unit = c.sources.length ? c.sources[c.sources.length - 1] : undefined;
                return (
                  <tr key={c.id} className={c.emergency ? "omni-emergency" : ""}>
                    <td>{c.systemName}</td>
                    <td className="mono">{formatMhz(c.freqHz)}</td>
                    <td className="num mono">{c.talkgroup}</td>
                    <td>{c.alphaTag || props.tgName(c.systemName, c.talkgroup)}</td>
                    <td className="num mono">{unit ?? ""}</td>
                    <td>{props.unitName(c.systemName, unit)}</td>
                    <td className="mono">{clock(Math.max(0, now - c.startS))}</td>
                    <td>{mode(c)}</td>
                    <td>
                      {c.state === "recording" ? (
                        <span className="omni-badge rec">Recording</span>
                      ) : c.reason === "encrypted" ? null : (
                        <span className="muted">{c.reason ? c.reason.replaceAll("_", " ") : "not recorded"}</span>
                      )}
                      {c.encrypted && <span className="omni-badge enc">Encrypted</span>}
                      {c.emergency && <span className="omni-badge emer">Emergency</span>}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
