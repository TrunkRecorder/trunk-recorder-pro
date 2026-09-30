import { useEffect, useId, useRef, useState } from "react";
import {
  AIRSPY_RATES,
  autoCenterFor,
  channelsToCsv,
  defaultTalkgroup,
  formatFromPath,
  formatMhz,
  importTrunkRecorderConfig,
  mhzCell,
  newAirspy,
  newDongle,
  newFile,
  newUsrp,
  parseChannelCsv,
  parseFreqList,
  resolvedCenters,
  SAMPLE_RATES,
  USRP_RATES,
  usableHalfWidth,
} from "./config.ts";
import { downloadText, findRadios, refreshDevices, setChannelFile, setNotice, updateConfig, useApp, web } from "./controller.ts";
import type { Channel, Config, Source } from "./protocol.ts";
import { SurveyPanel } from "./Survey.tsx";
import { parseTalkgroupCsv } from "./talkgroups.ts";

function Field(props: { label: string; hint?: string; children: React.ReactNode; wide?: boolean }) {
  return (
    <label className={`field${props.wide ? " wide" : ""}`}>
      <span className="field-label">{props.label}</span>
      {props.children}
      {props.hint && <span className="field-hint">{props.hint}</span>}
    </label>
  );
}

function Toggle(props: { label: string; checked: boolean; onChange: (v: boolean) => void; hint?: string }) {
  return (
    <label className="toggle">
      <input type="checkbox" checked={props.checked} onChange={(e) => props.onChange(e.target.checked)} />
      <span>
        {props.label}
        {props.hint && <span className="field-hint"> — {props.hint}</span>}
      </span>
    </label>
  );
}

/** A frequency input that keeps the user's text while typing. */
function MhzInput(props: { hz: number; placeholder?: string; onChange: (hz: number) => void }) {
  const [text, setText] = useState(props.hz ? formatMhz(props.hz) : "");
  return (
    <input
      className="mono"
      value={text}
      placeholder={props.placeholder ?? "MHz"}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(parseFreqList(e.target.value)[0] ?? 0);
      }}
    />
  );
}

async function connectDongle(): Promise<void> {
  try {
    await web?.requestUsb();
  } catch (e) {
    setNotice(e instanceof Error ? e.message : String(e));
  }
}

/** A sample rate in MSPS: pick a common one or type another. */
function RateInput(props: { hz: number; options: number[]; onChange: (hz: number) => void; free?: boolean }) {
  const id = useId();
  if (!props.free) {
    return (
      <select value={props.hz} onChange={(e) => props.onChange(Number(e.target.value))}>
        {(props.options.includes(props.hz) ? props.options : [props.hz, ...props.options]).map((r) => (
          <option key={r} value={r}>
            {(r / 1e6).toFixed(3)} MSPS
          </option>
        ))}
      </select>
    );
  }
  return (
    <>
      <input
        className="mono"
        list={id}
        defaultValue={props.hz / 1e6}
        onChange={(e) => {
          const v = Number(e.target.value);
          if (v > 0) props.onChange(Math.round(v * 1e6));
        }}
      />
      <datalist id={id}>
        {props.options.map((r) => (
          <option key={r} value={r / 1e6} />
        ))}
      </datalist>
    </>
  );
}

/** Why an optional driver can't be used, and how to install it. */
function DriverMissing(props: { kind: "usrp" | "airspy"; detail: string }) {
  return (
    <div className="banner bad small wide">
      {props.kind === "usrp" ? (
        <span>
          USRP support needs <b>UHD</b>, which wasn&apos;t found ({props.detail}). Install it — macOS: <code>brew install uhd</code>; Debian/Ubuntu:{" "}
          <code>sudo apt install libuhd-dev uhd-host</code>; Windows: Ettus&apos;s UHD installer — then run <code>uhd_images_downloader</code> and restart
          Trunk Recorder Lite.
        </span>
      ) : (
        <span>
          Airspy support needs <b>libairspy</b>, which wasn&apos;t found ({props.detail}). Install it — macOS: <code>brew install airspy</code>; Debian/Ubuntu:{" "}
          <code>sudo apt install libairspy0</code>; Windows: put <code>airspy.dll</code> (from airspy-tools) next to <code>trunk-lite.exe</code> — then
          restart Trunk Recorder Lite.
        </span>
      )}
    </div>
  );
}

const KINDS: { kind: Source["kind"]; label: string; desktop?: boolean }[] = [
  { kind: "rtlsdr", label: "RTL-SDR" },
  { kind: "usrp", label: "USRP", desktop: true },
  { kind: "airspy", label: "Airspy", desktop: true },
  { kind: "file", label: "Capture file" },
];

function SourceCard(props: { c: Config; i: number }) {
  const s = useApp();
  const { c, i } = props;
  const src = c.sources[i];
  const center = resolvedCenters(c)[i];
  const auto = i === 0 ? autoCenterFor(c, src.rateHz) : null;
  const half = usableHalfWidth(src.rateHz);
  const edit = (fn: (x: Source) => void) => updateConfig((x) => fn(x.sources[i]));
  const setKind = (kind: Source["kind"]) =>
    updateConfig((x) => {
      const old = x.sources[i];
      const fresh = kind === "rtlsdr" ? newDongle() : kind === "usrp" ? newUsrp() : kind === "airspy" ? newAirspy() : newFile();
      x.sources[i] = { ...fresh, centerHz: old.centerHz } as Source;
    });
  const fileRef = useRef<HTMLInputElement>(null);
  const others = c.sources.filter((_, k) => k !== i);
  const used = new Set(others.map((x) => (x.kind === "rtlsdr" ? `r:${x.serial}` : x.kind === "airspy" ? `a:${x.serial}` : x.kind === "usrp" ? `u:${x.args}` : "")));
  const radios = s.radios;

  return (
    <div className="source-card">
      <div className="row source-head">
        <strong>Source {i + 1}</strong>
        <div className="seg small" role="radiogroup" aria-label={`Source ${i + 1} kind`}>
          {KINDS.filter((k) => !k.desktop || !web).map((k) => (
            <button key={k.kind} role="radio" aria-checked={src.kind === k.kind} className={src.kind === k.kind ? "on" : ""} onClick={() => setKind(k.kind)}>
              {k.label}
            </button>
          ))}
        </div>
        <span className="spacer" />
        {c.sources.length > 1 && (
          <button
            className="btn ghost small"
            onClick={() => {
              web?.removeFile(i);
              updateConfig((x) => void x.sources.splice(i, 1));
            }}
          >
            Remove
          </button>
        )}
      </div>
      <div className="grid2">
        {src.kind === "rtlsdr" && (
          <Field
            label="Dongle"
            hint={s.devices.length ? undefined : web ? "No dongle connected — press Connect and pick it in the browser's list." : "No dongle found — plug one in and press Refresh."}
          >
            <div className="row">
              <select value={src.serial} onChange={(e) => edit((x) => x.kind === "rtlsdr" && void (x.serial = e.target.value))}>
                <option value="">First available</option>
                {s.devices.map((d) => (
                  <option key={d.serial || d.index} value={d.serial} disabled={used.has(`r:${d.serial}`)}>
                    {d.product}
                    {d.serial ? ` · SN ${d.serial}` : ""}
                    {used.has(`r:${d.serial}`) ? " (in use)" : ""}
                  </option>
                ))}
              </select>
              {web ? (
                <button className="btn ghost small" onClick={() => void connectDongle()}>
                  Connect…
                </button>
              ) : (
                <button className="btn ghost small" onClick={refreshDevices}>
                  Refresh
                </button>
              )}
            </div>
          </Field>
        )}
        {src.kind === "usrp" &&
          (radios && !radios.usrp.available ? (
            <DriverMissing kind="usrp" detail={radios.usrp.detail} />
          ) : (
            <Field
              label="USRP"
              hint={
                radios?.usrp.devices === null
                  ? `${radios?.usrp.detail ?? "UHD"} · press Find to search, or type UHD device arguments (blank = first found)`
                  : radios?.usrp.devices?.length
                    ? radios.usrp.detail
                    : "None found — check the cable / network, or type device arguments (e.g. addr=192.168.10.2)"
              }
            >
              <div className="row">
                <input
                  className="mono"
                  list={`usrp-${i}`}
                  value={src.args}
                  placeholder="first found"
                  onChange={(e) => edit((x) => x.kind === "usrp" && void (x.args = e.target.value))}
                />
                <datalist id={`usrp-${i}`}>
                  {(radios?.usrp.devices ?? []).map((d) => (
                    <option key={d.args} value={d.args}>
                      {d.label}
                    </option>
                  ))}
                </datalist>
                <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                  {s.findingRadios ? "Searching…" : "Find"}
                </button>
              </div>
            </Field>
          ))}
        {src.kind === "airspy" &&
          (radios && !radios.airspy.available ? (
            <DriverMissing kind="airspy" detail={radios.airspy.detail} />
          ) : (
            <Field label="Airspy" hint={radios?.airspy.devices?.length ? radios.airspy.detail : `${radios?.airspy.detail ?? "libairspy"} · none found — plug one in and press Refresh`}>
              <div className="row">
                <select value={src.serial} onChange={(e) => edit((x) => x.kind === "airspy" && void (x.serial = e.target.value))}>
                  <option value="">First available</option>
                  {src.serial && !(radios?.airspy.devices ?? []).some((d) => d.serial === src.serial) && <option value={src.serial}>SN {src.serial} (not connected)</option>}
                  {(radios?.airspy.devices ?? []).map((d) => (
                    <option key={d.serial} value={d.serial} disabled={used.has(`a:${d.serial}`)}>
                      {d.label}
                      {used.has(`a:${d.serial}`) ? " (in use)" : ""}
                    </option>
                  ))}
                </select>
                <button className="btn ghost small" disabled={s.findingRadios} onClick={findRadios}>
                  Refresh
                </button>
              </div>
            </Field>
          ))}
        {src.kind === "file" &&
          (web ? (
            <Field label="Capture file" hint="rtl_sdr output (unsigned 8-bit IQ). The browser forgets the choice on reload." wide>
              <div className="row">
                <button className="btn ghost small" onClick={() => fileRef.current?.click()}>
                  Choose file…
                </button>
                <span className="mono">{web.file(i)?.name ?? (src.path ? `${src.path} (choose again)` : "none")}</span>
              </div>
              <input
                ref={fileRef}
                type="file"
                hidden
                onChange={(e) => {
                  const f = e.target.files?.[0] ?? null;
                  web?.setFile(i, f);
                  edit((x) => x.kind === "file" && void (x.path = f?.name ?? ""));
                  e.target.value = "";
                }}
              />
            </Field>
          ) : (
            <Field label="Capture file" hint="A path on the recorder's computer." wide>
              <input
                className="mono"
                value={src.path}
                placeholder="/path/to/capture.cu8"
                onChange={(e) =>
                  edit((x) => {
                    if (x.kind !== "file") return;
                    x.path = e.target.value;
                    x.format = formatFromPath(x.path);
                  })
                }
              />
            </Field>
          ))}
        <Field label="Center frequency, MHz" hint={src.centerHz ? "Manual" : auto ? `Auto: ${formatMhz(auto, 4)} MHz` : i === 0 ? "Auto — needs channels that fit one source" : "Required"}>
          <MhzInput hz={src.centerHz} placeholder={auto ? formatMhz(auto, 4) : "MHz"} onChange={(hz) => edit((x) => void (x.centerHz = hz))} />
        </Field>
        <Field
          label="Sample rate"
          hint={src.kind === "usrp" ? "MSPS; wider covers more channels, costs more CPU" : src.kind === "airspy" ? "R2: 10 or 2.5; Mini: 6 or 3 (10 on newer firmware)" : undefined}
        >
          <RateInput
            key={src.kind}
            hz={src.rateHz}
            free={src.kind === "usrp" || (src.kind === "file" && !web)}
            options={src.kind === "usrp" ? USRP_RATES : src.kind === "airspy" ? AIRSPY_RATES : src.kind === "file" ? [...SAMPLE_RATES, 8_000_000, 10_000_000] : SAMPLE_RATES}
            onChange={(hz) => edit((x) => void (x.rateHz = hz))}
          />
        </Field>
        {src.kind === "rtlsdr" && (
          <>
            <Field label="Gain, dB" hint="Blank = tuner AGC">
              <input
                className="mono"
                value={src.gainDb ?? ""}
                placeholder="AGC"
                onChange={(e) => {
                  const v = e.target.value.trim();
                  edit((x) => x.kind === "rtlsdr" && void (x.gainDb = v === "" ? null : Number(v)));
                }}
              />
            </Field>
            <Field label="Frequency correction, ppm">
              <input className="mono" value={src.ppm} onChange={(e) => edit((x) => x.kind === "rtlsdr" && void (x.ppm = Number(e.target.value) || 0))} />
            </Field>
          </>
        )}
        {src.kind === "usrp" && (
          <>
            <Field label="Gain, dB" hint="B200/B210: 0–76">
              <input className="mono" value={src.gainDb} onChange={(e) => edit((x) => x.kind === "usrp" && void (x.gainDb = Number(e.target.value) || 0))} />
            </Field>
            <Field label="Antenna" hint="Blank = the device's default (e.g. RX2, TX/RX)">
              <input className="mono" value={src.antenna} placeholder="default" onChange={(e) => edit((x) => x.kind === "usrp" && void (x.antenna = e.target.value.trim()))} />
            </Field>
            <Field label="Frequency correction, ppm">
              <input className="mono" value={src.ppm} onChange={(e) => edit((x) => x.kind === "usrp" && void (x.ppm = Number(e.target.value) || 0))} />
            </Field>
          </>
        )}
        {src.kind === "airspy" && (
          <>
            <Field label="Gain" hint="Linearity gain step, 0–21">
              <input
                type="range"
                min={0}
                max={21}
                value={src.gain}
                onChange={(e) => edit((x) => x.kind === "airspy" && void (x.gain = Number(e.target.value)))}
                aria-valuetext={String(src.gain)}
              />
              <span className="mono small">{src.gain}</span>
            </Field>
            <Field label="Frequency correction, ppm">
              <input className="mono" value={src.ppm} onChange={(e) => edit((x) => x.kind === "airspy" && void (x.ppm = Number(e.target.value) || 0))} />
            </Field>
            <Toggle label="Bias-T" hint="powers an LNA over the antenna cable" checked={src.biasTee} onChange={(v) => edit((x) => x.kind === "airspy" && void (x.biasTee = v))} />
          </>
        )}
        {src.kind === "file" && (
          <>
            {!web && (
              <Field label="Sample format">
                <select value={src.format ?? "cu8"} onChange={(e) => edit((x) => x.kind === "file" && void (x.format = e.target.value as "cu8" | "cs16" | "cf32"))}>
                  <option value="cu8">cu8 — rtl_sdr (unsigned 8-bit)</option>
                  <option value="cs16">cs16 — signed 16-bit</option>
                  <option value="cf32">cf32 — float (GNU Radio, UHD)</option>
                </select>
              </Field>
            )}
            <Toggle label="Real-time pace" hint="off = as fast as the computer decodes" checked={src.realtime} onChange={(v) => edit((x) => x.kind === "file" && void (x.realtime = v))} />
          </>
        )}
      </div>
      {center ? (
        <p className="muted small">
          Covers {formatMhz(center - half, 3)} – {formatMhz(center + half, 3)} MHz.
        </p>
      ) : null}
    </div>
  );
}

let nextRowId = 1;

/**
 * Conventional channels: a table editor, bulk add, CSV import / export, and
 * (desktop) a linked CSV file to edit in a spreadsheet instead.
 */
function ConventionalPanel(props: { c: Config }) {
  const { c } = props;
  const conv = c.conventional ?? { squelchDb: 8, channels: [] };
  const chans = conv.channels;
  // Stable row keys (each edit clones the config; a frequency input keeps its own text).
  const ids = useRef<number[]>([]);
  if (ids.current.length !== chans.length) ids.current = chans.map(() => nextRowId++);
  const csvRef = useRef<HTMLInputElement>(null);
  const [bulk, setBulk] = useState("");
  const [pending, setPending] = useState<{ name: string; channels: Channel[]; notes: string[] } | null>(null);
  const [filePath, setFilePath] = useState("channels.csv");
  const linked = !web && !!conv.channelFile;
  const [bulkMode, setBulkMode] = useState<Channel["mode"]>("fm");
  const edit = (fn: (x: Config["conventional"]) => void) =>
    updateConfig((x) => {
      x.conventional ??= { squelchDb: 8, channels: [] };
      fn(x.conventional);
    });
  const editRow = (i: number, fn: (ch: Channel) => void) => edit((x) => fn(x.channels[i]));
  const remove = (i: number) => {
    ids.current.splice(i, 1);
    edit((x) => void x.channels.splice(i, 1));
  };
  const add = (list: Channel[]) => {
    ids.current.push(...list.map(() => nextRowId++));
    edit((x) => void x.channels.push(...list));
  };
  const replaceAll = (list: Channel[]) => {
    ids.current = list.map(() => nextRowId++);
    edit((x) => void (x.channels = list));
  };
  const onCsv = async (f: File | undefined) => {
    if (!f) return;
    const { channels, notes, error } = parseChannelCsv(await f.text());
    if (error) return setNotice(`${f.name}: ${error}`);
    if (!chans.length) {
      replaceAll(channels);
      setNotice(`Read ${channels.length} channel(s) from ${f.name}.${notes.length ? " " + notes.join(" ") : ""}`);
    } else setPending({ name: f.name, channels, notes });
  };
  const exportCsv = () => downloadText(`${c.system.shortName || "channels"}-channels.csv`, channelsToCsv(chans));
  const enabled = chans.filter((ch) => ch.enabled).length;
  const optNum = (v: string): number | undefined => (v.trim() === "" || !Number.isFinite(Number(v)) ? undefined : Number(v));

  return (
    <section className="panel">
      <header className="panel-head">
        <h2>Conventional channels</h2>
        <div className="row">
          {!linked && (
            <button className="btn ghost" onClick={() => csvRef.current?.click()}>
              Import CSV…
            </button>
          )}
          <button className="btn ghost" disabled={!chans.length} onClick={exportCsv} title="Save the list as a CSV to edit in a spreadsheet">
            Export CSV
          </button>
          {!linked && (
            <button className="btn ghost" onClick={() => add([{ freqHz: 0, mode: bulkMode, name: "", enabled: true }])}>
              Add a channel
            </button>
          )}
        </div>
        <input
          ref={csvRef}
          type="file"
          accept=".csv,text/csv"
          hidden
          onChange={(e) => {
            void onCsv(e.target.files?.[0]);
            e.target.value = "";
          }}
        />
      </header>
      <div className="stack">
        <p className="muted small">
          Analog FM or P25 channels anywhere inside a source's bandwidth, alongside a trunked system or on their own. Each is watched in the spectrum the recorder
          already computes, so an idle channel costs almost nothing; a call starts when its signal rises above the noise floor by the squelch level.
        </p>
        {pending && (
          <div className="banner">
            <div>
              Read {pending.channels.length} channel{pending.channels.length === 1 ? "" : "s"} from {pending.name}.{pending.notes.length ? " " + pending.notes.join(" ") : ""}
            </div>
            <div className="row">
              <button
                className="btn primary"
                onClick={() => {
                  replaceAll(pending.channels);
                  setPending(null);
                }}
              >
                Replace the list ({chans.length} now)
              </button>
              <button
                className="btn"
                onClick={() => {
                  add(pending.channels);
                  setPending(null);
                }}
              >
                Add to the list
              </button>
              <button className="btn ghost" onClick={() => setPending(null)}>
                Cancel
              </button>
            </div>
          </div>
        )}
        {!web &&
          (linked ? (
            <div className="banner">
              <div>
                Channels come from <code>{conv.channelFile}</code> on the recorder's computer (next to the config unless the path is absolute). Edit it in a
                spreadsheet, then Reload; recording also re-reads it each time it starts.
              </div>
              {conv.channelFileStatus && <div className="small mono">{conv.channelFileStatus}</div>}
              <div className="row">
                <button className="btn" onClick={() => setChannelFile(conv.channelFile ?? "")}>
                  Reload
                </button>
                <button className="btn ghost" onClick={() => setChannelFile("")} title="Keep the channels here, in the app's settings, and stop reading the file">
                  Unlink
                </button>
              </div>
            </div>
          ) : (
            <Field
              label="Channel file (optional)"
              hint="Keep the channels in a CSV on the recorder's computer and edit them in Excel, Numbers or LibreOffice. Relative paths are next to the config file. A new file is created from this list."
              wide
            >
              <div className="row">
                <input className="mono grow" value={filePath} placeholder="channels.csv" onChange={(e) => setFilePath(e.target.value)} />
                <button className="btn" disabled={!filePath.trim()} onClick={() => setChannelFile(filePath.trim())}>
                  Use this file
                </button>
              </div>
            </Field>
          ))}
        <div className="grid3">
          <Field label="Squelch, dB above noise" hint="For every channel without its own. Raise it if noise opens channels.">
            <input className="mono" value={conv.squelchDb} onChange={(e) => edit((x) => void (x.squelchDb = Math.max(3, Math.min(40, Number(e.target.value) || 8))))} />
          </Field>
          {!linked && (
            <Field label="Add frequencies, MHz" hint="Comma or space separated" wide>
              <div className="row">
                <input className="mono grow" value={bulk} placeholder="154.430, 155.100, 460.125" onChange={(e) => setBulk(e.target.value)} />
                <select value={bulkMode} onChange={(e) => setBulkMode(e.target.value as Channel["mode"])} aria-label="Mode for the added channels">
                  <option value="fm">Analog FM</option>
                  <option value="p25">P25</option>
                </select>
                <button
                  className="btn"
                  disabled={!parseFreqList(bulk).length}
                  onClick={() => {
                    add(parseFreqList(bulk).map((freqHz) => ({ freqHz, mode: bulkMode, name: "", enabled: true })));
                    setBulk("");
                  }}
                >
                  Add
                </button>
              </div>
            </Field>
          )}
        </div>
        {chans.length > 0 && (
          <>
            <div className="table-wrap channels-wrap">
              <table className="calls channels">
                <thead>
                  <tr>
                    <th title="Record this channel">On</th>
                    <th>Frequency, MHz</th>
                    <th>Mode</th>
                    <th>Name</th>
                    <th title="Calls are filed under this number; P25 uses the talkgroup on the air when there is one">Talkgroup</th>
                    <th title="dB above the noise floor">Squelch</th>
                    <th />
                  </tr>
                </thead>
                <tbody>
                  {chans.map((ch, i) => (
                    <tr key={ids.current[i]} className={ch.enabled ? "" : "st-monitoring"}>
                      <td>
                        <input type="checkbox" disabled={linked} checked={ch.enabled} aria-label="Record this channel" onChange={(e) => editRow(i, (x) => void (x.enabled = e.target.checked))} />
                      </td>
                      <td>
                        {linked ? <span className="mono">{mhzCell(ch.freqHz)}</span> : <MhzInput hz={ch.freqHz} onChange={(hz) => editRow(i, (x) => void (x.freqHz = hz))} />}
                      </td>
                      <td>
                        <select value={ch.mode} disabled={linked} aria-label="Mode" onChange={(e) => editRow(i, (x) => void (x.mode = e.target.value as Channel["mode"]))}>
                          <option value="fm">Analog FM</option>
                          <option value="p25">P25</option>
                        </select>
                      </td>
                      <td>
                        <input value={ch.name} disabled={linked} placeholder="Name" aria-label="Name" onChange={(e) => editRow(i, (x) => void (x.name = e.target.value))} />
                      </td>
                      <td>
                        <input
                          className="mono narrow"
                          disabled={linked}
                          value={ch.talkgroup ?? ""}
                          placeholder={ch.freqHz ? String(defaultTalkgroup(ch.freqHz)) : "auto"}
                          aria-label="Talkgroup"
                          onChange={(e) =>
                            editRow(i, (x) => {
                              const v = optNum(e.target.value);
                              if (v === undefined || v <= 0) delete x.talkgroup;
                              else x.talkgroup = Math.round(v);
                            })
                          }
                        />
                      </td>
                      <td>
                        <input
                          className="mono narrow"
                          disabled={linked}
                          value={ch.squelchDb ?? ""}
                          placeholder={String(conv.squelchDb)}
                          aria-label="Squelch, dB above noise"
                          onChange={(e) =>
                            editRow(i, (x) => {
                              const v = optNum(e.target.value);
                              if (v === undefined) delete x.squelchDb;
                              else x.squelchDb = Math.max(3, Math.min(40, v));
                            })
                          }
                        />
                      </td>
                      <td>
                        {!linked && (
                            <button className="btn ghost small danger" title="Remove" aria-label="Remove channel" onClick={() => remove(i)}>
                              ×
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              <div className="row">
                <span className="muted small">
                  {chans.length} channel{chans.length === 1 ? "" : "s"}, {enabled} on
                </span>
                <span className="spacer" />
                {!linked && (
                <button
                  className="btn ghost small danger"
                  onClick={() => {
                    if (window.confirm(`Remove all ${chans.length} conventional channels?`)) {
                      ids.current = [];
                      edit((x) => void (x.channels = []));
                    }
                  }}
                >
                  Remove all
                </button>
              )}
            </div>
          </>
        )}
        <details className="help">
          <summary>CSV format</summary>
          <p className="small">
            A header row, then one channel per row; <b>Export CSV</b> writes one to start from. Columns, in any order: <code>TG Number</code> (empty = the
            frequency in kHz), <code>Frequency</code> (MHz with a decimal point, or Hz), <code>Mode</code> (<code>fm</code> or <code>p25</code>; empty = fm),{" "}
            <code>Alpha Tag</code>, <code>Description</code>, <code>Tag</code>, <code>Category</code>, <code>Squelch dB</code> (above the noise; empty = the
            default), <code>Enable</code> (<code>false</code> = off). Trunk Recorder's channel file reads as is; its <code>Squelch</code> column (an absolute
            level) isn't used.
          </p>
        </details>
      </div>
    </section>
  );
}

export function Setup() {
  const s = useApp();
  const c = s.config;
  const [ccText, setCcText] = useState(() => (c ? c.system.controlChannels.map((f) => formatMhz(f)).join(", ") : ""));
  // The config arrives from the recorder after the first render.
  const loaded = c !== null;
  useEffect(() => {
    if (loaded && !ccText) setCcText((s.config?.system.controlChannels ?? []).map((f) => formatMhz(f)).join(", "));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loaded]);
  // The survey filled the config in: show its control channels.
  useEffect(() => {
    if (s.configEpoch) setCcText((s.config?.system.controlChannels ?? []).map((f) => formatMhz(f)).join(", "));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [s.configEpoch]);
  const importRef = useRef<HTMLInputElement>(null);
  const tgRef = useRef<HTMLInputElement>(null);
  if (!c) return <p className="muted">Connecting to the recorder…</p>;
  const tgCount = c.system.talkgroupsCsv ? parseTalkgroupCsv(c.system.talkgroupsCsv).size : 0;

  const onImport = async (f: File | undefined) => {
    if (!f) return;
    try {
      const { config, notes } = importTrunkRecorderConfig(await f.text(), c);
      updateConfig((x) => Object.assign(x, config));
      setCcText(config.system.controlChannels.map((v) => formatMhz(v)).join(", "));
      setNotice(`Imported ${f.name}.${notes.length ? " " + notes.join(" ") : ""}`);
    } catch (e) {
      setNotice(`Couldn't read ${f.name}: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  const onTalkgroups = async (f: File | undefined) => {
    if (!f) return;
    const text = await f.text();
    const n = parseTalkgroupCsv(text).size;
    updateConfig((x) => {
      x.system.talkgroupsCsv = text;
      x.system.talkgroupsName = f.name;
    });
    setNotice(`Loaded ${n} talkgroups from ${f.name}.`);
  };

  return (
    <div className="setup">
      <SurveyPanel c={c} />
      <section className="panel">
        <header className="panel-head">
          <h2>System</h2>
          <button className="btn ghost" onClick={() => importRef.current?.click()}>
            Import Trunk Recorder config…
          </button>
          <input ref={importRef} type="file" accept=".json,application/json" hidden onChange={(e) => void onImport(e.target.files?.[0])} />
        </header>
        <div className="grid2">
          <Field label="Short name" hint="Folder name for this system's calls">
            <input value={c.system.shortName} onChange={(e) => updateConfig((x) => void (x.system.shortName = e.target.value.replace(/[^\w.-]/g, "") || "sys1"))} />
          </Field>
          <Field label="Type">
            <select value="p25" disabled>
              <option value="p25">P25 (Phase 1 and 2)</option>
            </select>
          </Field>
          <Field label="Control channels, MHz" hint="Comma separated. The first one in range is tried first; the rest are fallbacks. Leave empty to record conventional channels only." wide>
            <input
              className="mono"
              value={ccText}
              placeholder="857.9875, 858.9875"
              onChange={(e) => {
                setCcText(e.target.value);
                const list = parseFreqList(e.target.value);
                updateConfig((x) => void (x.system.controlChannels = list));
              }}
            />
          </Field>
          <Field label="Modulation" hint="Auto runs C4FM and CQPSK receivers side by side and keeps the best of each frame.">
            <select value={c.system.modulation} onChange={(e) => updateConfig((x) => void (x.system.modulation = e.target.value as Config["system"]["modulation"]))}>
              <option value="auto">Auto (both receivers)</option>
              <option value="fsk4">C4FM (fsk4)</option>
              <option value="qpsk">CQPSK / LSM simulcast (qpsk)</option>
            </select>
          </Field>
          <Field label="Talkgroups" hint="Trunk Recorder's talkgroup CSV">
            <div className="row">
              <button className="btn" onClick={() => tgRef.current?.click()}>
                Load CSV…
              </button>
              <span className="mono">{tgCount ? `${tgCount} from ${c.system.talkgroupsName}` : "none"}</span>
              {tgCount > 0 && (
                <button
                  className="btn ghost"
                  onClick={() =>
                    updateConfig((x) => {
                      x.system.talkgroupsCsv = "";
                      x.system.talkgroupsName = "";
                    })
                  }
                >
                  Clear
                </button>
              )}
            </div>
            <input ref={tgRef} type="file" accept=".csv,text/csv" hidden onChange={(e) => void onTalkgroups(e.target.files?.[0])} />
          </Field>
        </div>
      </section>

      <ConventionalPanel c={c} />

      <section className="panel">
        <header className="panel-head">
          <h2>Sources</h2>
          <button className="btn ghost" onClick={() => updateConfig((x) => void x.sources.push(newDongle()))}>
            Add a source
          </button>
        </header>
        <div className="stack">
          {c.sources.map((_, i) => (
            <SourceCard key={`${i}-${s.configEpoch}`} c={c} i={i} />
          ))}
          <p className="muted small">
            Several dongles can cover one system: the control channel runs on whichever covers it, and each call is recorded from whichever covers its frequency.
          </p>
          <details className="help">
            <summary>Dongle not showing up?</summary>
            {web ? (
              <ul>
                <li>The browser version needs Chrome or Edge (WebUSB). Press Connect and choose the dongle; the browser remembers it for this site.</li>
                <li>
                  <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig.
                </li>
                <li>
                  <b>Linux:</b> add a udev rule giving your user access to USB 0bda:2838 and unload <code>dvb_usb_rtl28xxu</code>.
                </li>
                <li>For several dongles or long unattended runs, the desktop app is sturdier.</li>
              </ul>
            ) : (
            <ul>
              <li>
                <b>Windows:</b> install the WinUSB driver for “Bulk-In, Interface 0” with Zadig (the same step every RTL-SDR app needs).
              </li>
              <li>
                <b>Linux:</b> add a udev rule giving your user access to USB 0bda:2838, and unload the DVB driver (<code>sudo rmmod dvb_usb_rtl28xxu</code>) if it holds the dongle.
              </li>
              <li>
                <b>macOS:</b> works as-is. Quit other SDR apps that hold the dongle.
              </li>
            </ul>
            )}
          </details>
        </div>
      </section>

      <section className="panel">
        <header className="panel-head">
          <h2>Recording</h2>
        </header>
        <div className="stack">
          {web ? (
            <p className="muted small">Calls are kept in this browser's storage (<i>Recorded calls</i> → Export to folder copies them out in Trunk Recorder's layout).</p>
          ) : (
            <Field label="Recordings folder" hint="On the recorder's computer. Calls go to <folder>/<short name>/<year>/<month>/<day>/." wide>
              <input className="mono" value={c.recording.captureDir} onChange={(e) => updateConfig((x) => void (x.recording.captureDir = e.target.value))} />
            </Field>
          )}
          <Toggle label="Record talkgroups not in the CSV" checked={c.recording.recordUnknown} onChange={(v) => updateConfig((x) => void (x.recording.recordUnknown = v))} />
          <Toggle label="Record unit-to-unit calls" checked={c.recording.recordUnitToUnit} onChange={(v) => updateConfig((x) => void (x.recording.recordUnitToUnit = v))} />
          <Toggle label="Keep calls with no audio" hint="encrypted, or nothing decoded" checked={c.recording.keepSilentCalls} onChange={(v) => updateConfig((x) => void (x.recording.keepSilentCalls = v))} />
          {!web && (
            <Toggle
              label="Save vocoder frames"
              hint="diagnostics: each call's decoded voice frames and error counts, as <call>.frames.jsonl"
              checked={c.recording.captureFrames}
              onChange={(v) => updateConfig((x) => void (x.recording.captureFrames = v))}
            />
          )}
          {!web && (
            <Toggle
              label="Start recording when the app starts"
              hint="for a machine that records unattended, e.g. after a reboot"
              checked={c.server.autoStart}
              onChange={(v) => updateConfig((x) => void (x.server.autoStart = v))}
            />
          )}
          <div className="grid3">
            <Field label="Pre-roll, s" hint="Air replayed from before the grant">
              <input className="mono" value={c.recording.prerollS} onChange={(e) => updateConfig((x) => void (x.recording.prerollS = Math.max(0, Math.min(3, Number(e.target.value) || 0))))} />
            </Field>
            <Field label="Call timeout, s">
              <input className="mono" value={c.recording.callTimeoutS} onChange={(e) => updateConfig((x) => void (x.recording.callTimeoutS = Math.max(1, Number(e.target.value) || 3)))} />
            </Field>
            <Field label="Max recorders">
              <input className="mono" value={c.recording.maxRecorders} onChange={(e) => updateConfig((x) => void (x.recording.maxRecorders = Math.max(1, Math.min(64, Number(e.target.value) || 32))))} />
            </Field>
          </div>
        </div>
      </section>
    </div>
  );
}
