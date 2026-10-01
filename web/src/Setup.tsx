import { useEffect, useId, useRef, useState } from "react";
import {
  activeSystems,
  AIRSPY_RATES,
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
  newSystem,
  resolvedCenters,
  SAMPLE_RATES,
  sameSystem,
  sourceCovering,
  systemColor,
  USRP_RATES,
  usableHalfWidth,
} from "./config.ts";
import { bumpEpoch, downloadText, findRadios, refreshDevices, setChannelFile, setNotice, updateConfig, useApp, web } from "./controller.ts";
import type { Channel, Config, SiteIdentity, Source, System } from "./protocol.ts";
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
          Trunk Recorder Pro.
        </span>
      ) : (
        <span>
          Airspy support needs <b>libairspy</b>, which wasn&apos;t found ({props.detail}). Install it — macOS: <code>brew install airspy</code>; Debian/Ubuntu:{" "}
          <code>sudo apt install libairspy0</code>; Windows: put <code>airspy.dll</code> (from airspy-tools) next to <code>trunk-pro.exe</code> — then
          restart Trunk Recorder Pro.
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
  const auto = src.centerHz ? null : center;
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
        <Field
          label="Center frequency, MHz"
          hint={src.centerHz ? "Manual" : auto ? `Auto: ${formatMhz(auto, 4)} MHz` : "Auto — placed over a system the sources before it don't cover; set it if nothing fits"}
        >
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
      {center ? <CoverageBar c={c} center={center} rateHz={src.rateHz} /> : null}
    </div>
  );
}

/** A source's band, with every system's control (tall) and known voice channels (short) inside it. */
function CoverageBar(props: { c: Config; center: number; rateHz: number }) {
  const { c, center, rateHz } = props;
  const half = usableHalfWidth(rateHz);
  const lo = center - rateHz / 2;
  const pos = (hz: number) => ((hz - lo) / rateHz) * 100;
  const inside = (hz: number) => Math.abs(hz - center) <= half;
  const systems = activeSystems(c);
  const ticks: { hz: number; kind: "cc" | "voice"; color: string; label: string }[] = [];
  systems.forEach((x, k) => {
    for (const f of x.controlChannels) if (inside(f)) ticks.push({ hz: f, kind: "cc", color: systemColor(k), label: `${x.shortName} control channel ${formatMhz(f)} MHz` });
    for (const f of x.voiceChannels) if (inside(f)) ticks.push({ hz: f, kind: "voice", color: systemColor(k), label: `${x.shortName} voice ${formatMhz(f)} MHz` });
  });
  const conv = c.conventional.channels.filter((ch) => ch.enabled && inside(ch.freqHz));
  for (const ch of conv) ticks.push({ hz: ch.freqHz, kind: "voice", color: "var(--text)", label: `conventional ${ch.name || formatMhz(ch.freqHz)}` });
  const here = systems.map((x, k) => ({ x, k })).filter(({ x }) => x.controlChannels.some(inside) || x.voiceChannels.some(inside));
  return (
    <div className="stack" style={{ gap: 4 }}>
      <div className="coverage" role="img" aria-label={`Covers ${formatMhz(center - half, 3)} to ${formatMhz(center + half, 3)} MHz`}>
        <div className="usable" style={{ left: `${pos(center - half)}%`, width: `${(2 * half * 100) / rateHz}%` }} />
        {ticks.map((t, k) => (
          <span key={k} className={`tick ${t.kind}`} style={{ left: `${pos(t.hz)}%`, background: t.color }} title={t.label} />
        ))}
      </div>
      <div className="legend">
        <span className="mono">
          {formatMhz(center - half, 3)} – {formatMhz(center + half, 3)} MHz
        </span>
        {here.map(({ x, k }) => (
          <span key={x.shortName}>
            <span className="sys-dot" style={{ background: systemColor(k) }} />
            {x.shortName}
          </span>
        ))}
        {conv.length > 0 && <span>{conv.length} conventional</span>}
        {!here.length && !conv.length && <span>nothing configured in this range</span>}
      </div>
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
  const conv = c.conventional;
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
  const exportCsv = () => downloadText(`${conv.shortName || "channels"}-channels.csv`, channelsToCsv(chans));
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
          {chans.length > 0 && (
            <Field label="Short name" hint="Folder name for conventional calls">
              <input value={conv.shortName} onChange={(e) => edit((x) => void (x.shortName = e.target.value.replace(/[^\w.-]/g, "") || "conv"))} />
            </Field>
          )}
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
                  <option value="dmr">DMR</option>
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
                          <option value="dmr">DMR</option>
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

/** A hex or decimal number field; empty = none. */
function NumInput(props: { value: number | null | undefined; hex?: boolean; placeholder?: string; onChange: (v: number | null) => void; label: string }) {
  const show = (v: number | null | undefined) => (v === null || v === undefined ? "" : props.hex ? v.toString(16).toUpperCase() : String(v));
  const [text, setText] = useState(show(props.value));
  useEffect(() => setText((t) => (parse(t) === (props.value ?? null) ? t : show(props.value))), [props.value]); // eslint-disable-line react-hooks/exhaustive-deps
  function parse(t: string): number | null {
    const v = t.trim();
    if (!v) return null;
    const n = props.hex ? parseInt(v, 16) : Number(v);
    return Number.isFinite(n) && n >= 0 ? n : null;
  }
  return (
    <input
      className="mono"
      aria-label={props.label}
      value={text}
      placeholder={props.placeholder ?? "any"}
      onChange={(e) => {
        setText(e.target.value);
        props.onChange(parse(e.target.value));
      }}
    />
  );
}

const hexText = (v: number | null | undefined) => (v === null || v === undefined ? "?" : v.toString(16).toUpperCase());

/** "WACN BEE00 · SysID 445 · site 1-3" from what is known. */
export function siteText(id: SiteIdentity): string {
  const parts: string[] = [];
  if (id.wacn != null) parts.push(`WACN ${hexText(id.wacn)}`);
  if (id.sysId != null) parts.push(`SysID ${hexText(id.sysId)}`);
  if (id.nac != null) parts.push(`NAC ${hexText(id.nac)}`);
  if (id.site != null) parts.push(id.rfss != null ? `site ${id.rfss}-${id.site}` : `site ${id.site}`);
  else if (id.rfss != null) parts.push(`RFSS ${id.rfss}`);
  return parts.join(" · ");
}

/** A SmartNet system's band plan (Trunk Recorder's settings) and default voice mode. */
/** "101=452.275, 102=452.3" ↔ { "101": 452275000, … }. */
function lcnText(t: Record<string, number> | undefined): string {
  return Object.entries(t ?? {})
    .map(([k, v]) => `${k}=${(v / 1e6).toFixed(5).replace(/0+$/, "")}`)
    .join(", ");
}
function parseLcn(s: string): Record<string, number> {
  const t: Record<string, number> = {};
  for (const part of s.split(/[,;\s]+/)) {
    const m = /^(\d+)=(\d+(?:\.\d+)?)$/.exec(part.trim());
    if (m) {
      const v = Number(m[2]);
      t[m[1]] = v < 1e5 ? Math.round(v * 1e6) : v;
    }
  }
  return t;
}

function DmrFields(props: { sys: System; edit: (fn: (x: System) => void) => void }) {
  const { sys, edit } = props;
  const [chText, setChText] = useState(() => (sys.channels ?? []).map((f) => (f / 1e6).toFixed(5)).join(", "));
  const [lcn, setLcn] = useState(() => lcnText(sys.lcnTable));
  return (
    <>
      <Field label="Voice frequencies, MHz" hint="Tier III / Capacity Max / Connect Plus: the site's voice channels. Each grant's channel is learned from which one the talkgroup comes up on." wide>
        <input
          className="mono"
          value={chText}
          placeholder="452.275, 452.300"
          onChange={(e) => {
            setChText(e.target.value);
            const list = parseFreqList(e.target.value);
            edit((x) => {
              if (list.length) x.channels = list;
              else delete x.channels;
            });
          }}
        />
      </Field>
      <Field label="Colour code" hint="Blank: the control channel's.">
        <input
          className="mono"
          value={sys.colorCode ?? ""}
          placeholder="auto"
          onChange={(e) =>
            edit((x) => {
              const v = parseInt(e.target.value, 10);
              if (v >= 0 && v <= 15) x.colorCode = v;
              else delete x.colorCode;
            })
          }
        />
      </Field>
      <Field label="Channel table (optional)" hint="Logical channel = MHz, e.g. 101=452.275. Wins over what is learned." wide>
        <input
          className="mono"
          value={lcn}
          placeholder="learned from the air"
          onChange={(e) => {
            setLcn(e.target.value);
            const t = parseLcn(e.target.value);
            edit((x) => {
              if (Object.keys(t).length) x.lcnTable = t;
              else delete x.lcnTable;
            });
          }}
        />
      </Field>
    </>
  );
}

function SmartnetFields(props: { sys: System; edit: (fn: (x: System) => void) => void }) {
  const { sys, edit } = props;
  const custom = (sys.bandplan ?? "").startsWith("400");
  const setNum = (k: "bandplanBase" | "bandplanSpacing" | "bandplanOffset" | "bandplanHigh", v: number | null) =>
    edit((x) => {
      if (v === null) delete x[k];
      else x[k] = v;
    });
  return (
    <>
      <Field label="Band plan" hint="The survey learns it by listening. VHF / UHF (OBT) systems need the numbers below.">
        <select value={custom ? "400_custom" : (sys.bandplan ?? "800_standard")} onChange={(e) => edit((x) => void (x.bandplan = e.target.value))}>
          <option value="800_standard">800 MHz standard</option>
          <option value="800_reband">800 MHz rebanded</option>
          <option value="800_splinter">800 MHz splinter</option>
          <option value="900">900 MHz</option>
          <option value="400_custom">VHF / UHF (custom, OBT)</option>
        </select>
      </Field>
      <Field label="Voice of unknown talkgroups" hint="Grants say P25 or analog; this is for talkgroups only ever seen in updates.">
        <select value={sys.defaultMode ?? "digital"} onChange={(e) => edit((x) => void (e.target.value === "analog" ? (x.defaultMode = "analog") : delete x.defaultMode))}>
          <option value="digital">P25</option>
          <option value="analog">Analog FM</option>
        </select>
      </Field>
      {custom && (
        <div className="id-grid wide">
          <Field label="Base, Hz" hint="Frequency of the offset channel">
            <NumInput label="Band plan base" placeholder="489087500" value={sys.bandplanBase} onChange={(v) => setNum("bandplanBase", v)} />
          </Field>
          <Field label="Spacing, Hz">
            <NumInput label="Band plan spacing" placeholder="25000" value={sys.bandplanSpacing} onChange={(v) => setNum("bandplanSpacing", v)} />
          </Field>
          <Field label="Offset (channel)">
            <NumInput label="Band plan offset" placeholder="380" value={sys.bandplanOffset} onChange={(v) => setNum("bandplanOffset", v)} />
          </Field>
          <Field label="High, Hz" hint="Highest outbound channel">
            <NumInput label="Band plan high" placeholder="496612500" value={sys.bandplanHigh} onChange={(v) => setNum("bandplanHigh", v)} />
          </Field>
        </div>
      )}
    </>
  );
}

function SystemCard(props: { c: Config; i: number }) {
  const { c, i } = props;
  const sys = c.systems[i];
  const [ccText, setCcText] = useState(() => sys.controlChannels.map((f) => formatMhz(f)).join(", "));
  const tgRef = useRef<HTMLInputElement>(null);
  const edit = (fn: (x: System) => void) => updateConfig((x) => fn(x.systems[i]));
  const setExpect = (k: keyof SiteIdentity, v: number | null) =>
    edit((x) => {
      x.expect = { ...x.expect };
      if (v === null) delete x.expect[k];
      else x.expect[k] = v;
    });
  const active = activeSystems(c);
  const idx = active.indexOf(sys);
  const color = systemColor(idx);
  const tgCount = sys.talkgroupsCsv ? parseTalkgroupCsv(sys.talkgroupsCsv).size : 0;
  const tgDonors = c.systems.filter((x, k) => k !== i && x.talkgroupsCsv);
  const siblings = c.systems.filter((x, k) => k !== i && sameSystem(x.expect, sys.expect));
  const locked = Object.values(sys.expect).some((v) => v !== null && v !== undefined);
  const dupName = c.systems.some((x, k) => k !== i && x.shortName === sys.shortName);
  // Where it lands on the sources.
  const centers = resolvedCenters(c);
  const ccOn = [...new Set(sys.controlChannels.map((f) => sourceCovering(c, centers, f)).filter((k) => k >= 0))];
  const voiceIn = sys.voiceChannels.filter((f) => sourceCovering(c, centers, f) >= 0).length;

  const onTalkgroups = async (f: File | undefined) => {
    if (!f) return;
    const text = await f.text();
    const n = parseTalkgroupCsv(text).size;
    edit((x) => {
      x.talkgroupsCsv = text;
      x.talkgroupsName = f.name;
    });
    setNotice(`Loaded ${n} talkgroups from ${f.name} into ${sys.shortName}.`);
  };

  return (
    <div className={`system-card${sys.enabled ? "" : " off"}`} style={{ ["--sys-color" as string]: color }}>
      <div className="row sys-head">
        <span className="sys-dot" style={{ background: color }} />
        <strong>{sys.shortName || "(no name)"}</strong>
        {sys.enabled && sys.controlChannels.length > 0 ? (
          ccOn.length ? (
            <span className="chip ok">control channel on source {ccOn.map((k) => k + 1).join(", ")}</span>
          ) : (
            <span className="chip bad">no control channel inside a source</span>
          )
        ) : null}
        {sys.enabled && sys.voiceChannels.length > 0 && (
          <span className={`chip ${voiceIn === sys.voiceChannels.length ? "ok" : "warn"}`} title="Voice channels the survey saw that a source covers">
            voice {voiceIn}/{sys.voiceChannels.length} covered
          </span>
        )}
        {siblings.length > 0 && <span className="chip" title={siteText(sys.expect)}>multi-site with {siblings.map((x) => x.shortName).join(", ")}</span>}
        <span className="spacer" />
        <label className="toggle small">
          <input type="checkbox" checked={sys.enabled} onChange={(e) => edit((x) => void (x.enabled = e.target.checked))} />
          <span>Record</span>
        </label>
        <button
          className="btn ghost small"
          onClick={() => {
            if (confirm(`Remove ${sys.shortName}? Its recordings stay on disk.`)) updateConfig((x) => void x.systems.splice(i, 1));
          }}
        >
          Remove
        </button>
      </div>
      <div className="grid2">
        <Field label="Short name" hint={dupName ? "Another system has this name — each needs its own folder" : "Folder name for this system's calls"}>
          <input value={sys.shortName} onChange={(e) => edit((x) => void (x.shortName = e.target.value.replace(/[^\w.-]/g, "")))} />
        </Field>
        <Field
          label="Type"
          hint={
            sys.type === "smartnet"
              ? "Motorola SmartNet / SmartZone control channel; voice is P25 or analog FM, per grant."
              : sys.type === "dmr"
                ? "Trunked DMR: Capacity Plus, Capacity Max, Connect Plus or Tier III — found by listening."
                : "P25 Phase 1 control channel (Phase 1 and 2 voice)."
          }
        >
          <select
            value={sys.type}
            onChange={(e) =>
              edit((x) => {
                x.type = e.target.value === "smartnet" ? "smartnet" : e.target.value === "dmr" ? "dmr" : "p25";
                if (x.type === "smartnet" && !x.bandplan) x.bandplan = "800_reband";
              })
            }
          >
            <option value="p25">P25</option>
            <option value="smartnet">SmartNet / SmartZone</option>
            <option value="dmr">DMR (trunked)</option>
          </select>
        </Field>
        {sys.type !== "dmr" && <Field label={sys.type === "smartnet" ? "P25 voice modulation" : "Modulation"} hint="Auto runs C4FM and CQPSK receivers side by side and keeps the best of each frame.">
          <select value={sys.modulation} onChange={(e) => edit((x) => void (x.modulation = e.target.value as System["modulation"]))}>
            <option value="auto">Auto (both receivers)</option>
            <option value="fsk4">C4FM (fsk4)</option>
            <option value="qpsk">CQPSK / LSM simulcast (qpsk)</option>
          </select>
        </Field>}
        <Field
          label={sys.type === "dmr" ? "Site frequencies, MHz" : "Control channels, MHz"}
          hint={
            sys.type === "dmr"
              ? "All watched at once. Capacity Plus: every repeater of the site. Others: the control channel(s)."
              : "Comma separated. The first one in range is tried first; the rest are fallbacks."
          }
          wide
        >
          <input
            className="mono"
            value={ccText}
            placeholder="857.9875, 858.9875"
            onChange={(e) => {
              setCcText(e.target.value);
              const list = parseFreqList(e.target.value);
              edit((x) => void (x.controlChannels = list));
            }}
          />
        </Field>
        <Field label="Talkgroups" hint="Trunk Recorder's talkgroup CSV">
          <div className="row">
            <button className="btn" onClick={() => tgRef.current?.click()}>
              Load CSV…
            </button>
            <span className="mono">{tgCount ? `${tgCount} from ${sys.talkgroupsName}` : "none"}</span>
            {tgCount > 0 && (
              <button
                className="btn ghost"
                onClick={() =>
                  edit((x) => {
                    x.talkgroupsCsv = "";
                    x.talkgroupsName = "";
                  })
                }
              >
                Clear
              </button>
            )}
            {tgDonors.length > 0 && (
              <select
                value=""
                aria-label="Copy talkgroups from another system"
                onChange={(e) => {
                  const d = c.systems.find((x) => x.shortName === e.target.value);
                  if (d)
                    edit((x) => {
                      x.talkgroupsCsv = d.talkgroupsCsv;
                      x.talkgroupsName = d.talkgroupsName;
                    });
                }}
              >
                <option value="">Copy from…</option>
                {tgDonors.map((d) => (
                  <option key={d.shortName} value={d.shortName}>
                    {d.shortName} ({d.talkgroupsName})
                  </option>
                ))}
              </select>
            )}
          </div>
          <input ref={tgRef} type="file" accept=".csv,text/csv" hidden onChange={(e) => void onTalkgroups(e.target.files?.[0])} />
        </Field>
        {sys.type === "smartnet" && <SmartnetFields sys={sys} edit={edit} />}
        {sys.type === "dmr" && <DmrFields sys={sys} edit={edit} />}
        <Field label="Talkgroups not in the CSV">
          <select
            value={sys.recordUnknown === true ? "yes" : sys.recordUnknown === false ? "no" : ""}
            onChange={(e) =>
              edit((x) => {
                if (e.target.value === "") delete x.recordUnknown;
                else x.recordUnknown = e.target.value === "yes";
              })
            }
          >
            <option value="">As in Recording ({c.recording.recordUnknown ? "record" : "skip"})</option>
            <option value="yes">Record</option>
            <option value="no">Skip</option>
          </select>
        </Field>
      </div>
      <details className="help">
        <summary>
          Site lock{locked ? <span className="muted"> — only {siteText(sys.expect)}</span> : <span className="muted"> — off (follows any control channel listed)</span>}
        </summary>
        <p className="muted small">
          Follow a control channel only when it announces this identity; leave a field empty to accept any. For a multi-site system, add each site as its own system with
          its site number here — then a control channel that hunts onto a neighbouring site is not followed. Hex for NAC, WACN and System ID; the survey fills these in.
        </p>
        <div className="id-grid">
          <Field label="NAC">
            <NumInput label="NAC" hex value={sys.expect.nac} onChange={(v) => setExpect("nac", v)} />
          </Field>
          <Field label="WACN">
            <NumInput label="WACN" hex value={sys.expect.wacn} onChange={(v) => setExpect("wacn", v)} />
          </Field>
          <Field label="System ID">
            <NumInput label="System ID" hex value={sys.expect.sysId} onChange={(v) => setExpect("sysId", v)} />
          </Field>
          <Field label="RFSS">
            <NumInput label="RFSS" value={sys.expect.rfss} onChange={(v) => setExpect("rfss", v)} />
          </Field>
          <Field label="Site">
            <NumInput label="Site" value={sys.expect.site} onChange={(v) => setExpect("site", v)} />
          </Field>
        </div>
        {locked && (
          <button className="btn ghost small" onClick={() => edit((x) => void (x.expect = {}))}>
            Clear the lock
          </button>
        )}
      </details>
    </div>
  );
}

export function Setup() {
  const s = useApp();
  const c = s.config;
  const importRef = useRef<HTMLInputElement>(null);
  if (!c) return <p className="muted">Connecting to the recorder…</p>;

  const onImport = async (f: File | undefined) => {
    if (!f) return;
    try {
      const { config, notes } = importTrunkRecorderConfig(await f.text(), c);
      updateConfig((x) => Object.assign(x, config));
      bumpEpoch();
      setNotice(`Imported ${f.name}.${notes.length ? " " + notes.join(" ") : ""}`);
    } catch (e) {
      setNotice(`Couldn't read ${f.name}: ${e instanceof Error ? e.message : String(e)}`);
    }
  };

  return (
    <div className="setup">
      <SurveyPanel c={c} />
      <section className="panel">
        <header className="panel-head">
          <h2>Systems</h2>
          <div className="row">
            <button className="btn ghost" onClick={() => updateConfig((x) => void x.systems.push(newSystem(x)))}>
              Add a system
            </button>
            <button className="btn ghost" onClick={() => importRef.current?.click()}>
              Import Trunk Recorder config…
            </button>
          </div>
          <input ref={importRef} type="file" accept=".json,application/json" hidden onChange={(e) => void onImport(e.target.files?.[0])} />
        </header>
        <div className="stack">
          {c.systems.map((_, i) => (
            <SystemCard key={`${i}-${c.systems.length}-${s.configEpoch}`} c={c} i={i} />
          ))}
          {c.systems.length === 0 ? (
            <p className="empty small">
              No trunked system yet. <b>Find my system</b> above scans for one, or press <b>Add a system</b> and type its control channels. Conventional channels alone
              work too.
            </p>
          ) : (
            <p className="muted small">
              Each system — or each site of a multi-site system — follows its own control channel and files its calls under its short name. They share the sources
              and the recorders. A call heard on two sites is recorded by both.
            </p>
          )}
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
            Sources are shared by every system: each control channel runs on whichever source covers it, and each call is recorded from whichever covers its
            frequency. A source left on Auto is placed over a system the sources before it don't cover.
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
          <Toggle label="Record talkgroups not in the CSV" hint="each system can override it" checked={c.recording.recordUnknown} onChange={(v) => updateConfig((x) => void (x.recording.recordUnknown = v))} />
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
            <Field label="Max recorders" hint="Shared by every system">
              <input className="mono" value={c.recording.maxRecorders} onChange={(e) => updateConfig((x) => void (x.recording.maxRecorders = Math.max(1, Math.min(64, Number(e.target.value) || 32))))} />
            </Field>
          </div>
        </div>
      </section>
    </div>
  );
}
