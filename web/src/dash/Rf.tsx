// How well the radios capture the air: each source's noise floor, headroom
// (how close to clipping), frequency error against the control channels,
// dropped samples — then, for one source, its band: the floor across it, each
// channel's power above the floor, the waterfall, and how all of it went over
// time.

import { useState } from "react";
import { formatGain, formatMhz, systemColor } from "../config.ts";
import { setView, useApp, useTopic, type AppState, type StatsRange } from "../controller.ts";
import { Card, Choice, Hint, LiveSpark, Meter, Stat, TimeSeries, type Line } from "../charts.tsx";
import { compact, mhz, num, signed } from "../fmt.ts";
import type { ChannelSnapshot, Source, SourceStatus } from "../protocol.ts";
import { Waterfall } from "../Waterfall.tsx";
import { K, recentAvg, running, sourceHealth, useHistory, val } from "./data.ts";
import { ccMarks } from "./Systems.tsx";
import { systemChoices } from "./Calls.tsx";

/** A source's gain setting, as configured. */
function gainText(c: Source | undefined): string {
  if (!c) return "—";
  switch (c.type) {
    case "rtlsdr":
      return c.agc ? "AGC" : `${formatGain(c.gainDb)} dB`;
    case "usrp":
      return c.agc ? "AGC" : `${c.gainDb} dB${c.antenna ? ` · ${c.antenna}` : ""}`;
    case "airspy":
      return c.gainMode === "manual" ? (c.agc ? `AGC (VGA ${c.vgaStep})` : `LNA ${c.lnaStep} · mixer ${c.mixerStep} · VGA ${c.vgaStep}`) : `${c.gainMode} ${c.gainStep}`;
    case "soapy":
      return c.agc ? "AGC" : c.gainDb !== null ? `${c.gainDb} dB` : Object.entries(c.gains ?? {}).map(([k, v]) => `${k} ${v}`).join(" · ") || "default";
    case "file":
      return "capture file";
  }
}

const autoTuneOf = (c: Source | undefined) => !!c && "autoTune" in c && !!c.autoTune;

/** One source: floor, headroom, frequency error, drops. `compact`: the overview's size. */
export function SourceCard({ s, src, i, compact: small }: { s: AppState; src: SourceStatus; i: number; compact?: boolean }) {
  const cfg = s.config?.sources[i];
  const h = sourceHealth(s, src, autoTuneOf(cfg));
  const noise = val(s, K.src(src.label, "noise"));
  const peak = val(s, K.src(src.label, "peak"));
  const clip = val(s, K.src(src.label, "clipPct"));
  const drops = recentAvg(K.src(src.label, "dropped"), 60) ?? 0;
  return (
    <Card title={src.label} level={h.level} why={h.why} onTitle={() => setView("rf", i)} actions={<span className="badge-kind">{mhz(src.centerHz, 3)} MHz · {num(src.rateHz / 1e6, 2)} MS/s</span>}>
      <div className={small ? "src-grid small" : "src-grid"}>
        <Stat
          label="Noise floor"
          value={num(noise)}
          unit="dBFS"
          spark={<LiveSpark k={K.src(src.label, "noise")} color="var(--series-1)" minSpan={6} title="Noise floor, last 10 min" />}
          hint="The median power across the band per FFT bin. It rises with gain and with interference; a jump without a gain change means something new nearby."
        />
        <div className="stat stat-kpi">
          <div className="stat-label">Headroom</div>
          <div className="stat-value">
            {peak === null ? "—" : num(-peak, 0)}
            <span className="stat-unit">dB</span>
          </div>
          <Meter value={peak === null ? null : peak + 60} max={60} zones={[54, 59]} title={`Peaks at ${num(peak)} dBFS`} label="peak level" />
          <div className="stat-sub">{clip ? `${num(clip, 2)}% clipping` : "no clipping"}</div>
        </div>
        <Stat
          label="Frequency error"
          value={src.errorPpm == null ? "—" : signed(src.errorPpm, 2)}
          unit="ppm"
          sub={autoTuneOf(cfg) ? `AutoTune corrects ${signed(src.tunePpm ?? 0, 2)}` : src.errorPpm == null ? "measured on a control channel" : "measured; not corrected (AutoTune off)"}
          spark={small ? undefined : <LiveSpark k={K.src(src.label, "ppm")} color="var(--series-2)" minSpan={1} title="Measured error, last 10 min" />}
          hint="How far signals come in from where they should, measured on the control channels. A steady offset is the dongle's crystal (set its ppm); a drifting one is temperature."
        />
        <Stat
          label="Dropped"
          value={compact(drops)}
          unit="samples/s"
          level={drops > 0 ? "bad" : undefined}
          sub={`${compact(src.dropped)} this run`}
          hint="Samples the driver lost: USB or CPU couldn't keep up. Calls on this source lose audio when it happens."
        />
      </div>
      {!small && (
        <div className="kv small">
          <span>
            Gain <b>{gainText(cfg)}</b>
          </span>
          <span>
            Rate <b>{num(src.rateMeasured / 1e6, 3)}</b> of {num(src.rateHz / 1e6, 3)} MS/s
          </span>
          {src.errors > 0 && (
            <span>
              Errors <b>{src.errors}</b>
              {src.lastError ? ` — ${src.lastError}` : ""}
            </span>
          )}
        </div>
      )}
    </Card>
  );
}

// ── the band ─────────────────────────────────────────────────────────────────

/** Every source's passband on one axis, with the control channels, conventional channels and calls on it. */
function BandOverview({ s }: { s: AppState }) {
  if (!s.sources.length) return null;
  const lo = Math.min(...s.sources.map((x) => x.centerHz - x.rateHz / 2));
  const hi = Math.max(...s.sources.map((x) => x.centerHz + x.rateHz / 2));
  const W = 1000;
  const x = (hz: number) => ((hz - lo) / (hi - lo)) * W;
  const ccs = s.status?.systems.flatMap((y) => (y.controlChannelHz ? [{ hz: y.controlChannelHz, name: y.shortName }] : [])) ?? [];
  const conv = (s.config?.conventional ?? []).flatMap((c) => c.channels.filter((ch) => ch.enabled).map((ch) => ({ hz: ch.freqHz, name: c.shortName, label: ch.name })));
  return (
    <Card title="The band" className="band">
      <svg viewBox={`0 0 ${W} 74`} className="band-svg" preserveAspectRatio="none" role="img" aria-label="sources and channels by frequency">
        {s.sources.map((src, i) => (
          <g key={i}>
            <rect x={x(src.centerHz - src.rateHz / 2)} width={x(src.centerHz + src.rateHz / 2) - x(src.centerHz - src.rateHz / 2)} y={30} height={20} rx={4} className="band-src" />
            <rect x={x(src.centerHz - src.rateHz * 0.4)} width={x(src.centerHz + src.rateHz * 0.4) - x(src.centerHz - src.rateHz * 0.4)} y={30} height={20} rx={4} className="band-usable">
              <title>{`${src.label}: ${mhz(src.centerHz - src.rateHz / 2, 3)}–${mhz(src.centerHz + src.rateHz / 2, 3)} MHz`}</title>
            </rect>
          </g>
        ))}
        {conv.map((c, i) => (
          <line key={`v${i}`} x1={x(c.hz)} x2={x(c.hz)} y1={30} y2={50} className="band-conv" vectorEffect="non-scaling-stroke">
            <title>{`${c.name}${c.label ? ` · ${c.label}` : ""} · ${mhz(c.hz)} MHz`}</title>
          </line>
        ))}
        {s.calls.map((c) => (
          <line key={`c${c.id}`} x1={x(c.freqHz)} x2={x(c.freqHz)} y1={22} y2={58} className={`band-call ${c.state}`} vectorEffect="non-scaling-stroke">
            <title>{`${c.systemName} TG ${c.alphaTag || c.talkgroup} · ${mhz(c.freqHz)} MHz · ${c.state}`}</title>
          </line>
        ))}
        {ccs.map((c) => (
          <g key={`${c.name}${c.hz}`}>
            <line x1={x(c.hz)} x2={x(c.hz)} y1={14} y2={56} stroke={systemColor(s.config, c.name)} strokeWidth={3} vectorEffect="non-scaling-stroke">
              <title>{`${c.name} control channel · ${mhz(c.hz)} MHz`}</title>
            </line>
          </g>
        ))}
        <text x={2} y={70} className="axis">
          {mhz(lo, 3)}
        </text>
        <text x={W - 2} y={70} textAnchor="end" className="axis">
          {mhz(hi, 3)} MHz
        </text>
      </svg>
      <div className="legend small">
        <span>
          <i className="sw-src" />
          source passband (usable part darker)
        </span>
        <span>
          <i className="sw-cc" />
          control channel
        </span>
        <span>
          <i className="sw-rec" />
          call recording
        </span>
        <span>
          <i className="sw-mon" />
          call followed, not recorded
        </span>
        {conv.length > 0 && (
          <span>
            <i className="sw-conv" />
            conventional channel
          </span>
        )}
      </div>
    </Card>
  );
}

/** The floor across the band (bars) and each channel's power above it (stems). */
function ChannelPlot({ profile, channels, src, color }: { profile: number[]; channels: ChannelSnapshot[]; src: SourceStatus; color: (sys: string) => string }) {
  const W = 1000;
  const H = 180;
  const all = [...profile, ...channels.map((c) => c.powerDb)];
  const lo = Math.floor(Math.min(...profile) - 3);
  const hi = Math.ceil(Math.max(...all) + 3);
  const y = (db: number) => H - 18 - ((db - lo) / Math.max(1, hi - lo)) * (H - 30);
  const x = (hz: number) => ((hz - (src.centerHz - src.rateHz / 2)) / src.rateHz) * W;
  const bw = W / profile.length;
  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="chplot" preserveAspectRatio="none" role="img" aria-label="noise floor and channel power">
      {profile.map((p, i) => (
        <rect key={i} x={i * bw + 1} width={bw - 2} y={y(p)} height={H - 18 - y(p)} className="floor-bar">
          <title>{`${mhz(src.centerHz - src.rateHz / 2 + (i + 0.5) * (src.rateHz / profile.length), 3)} MHz: floor ${num(p)} dBFS`}</title>
        </rect>
      ))}
      {channels.map((c) => (
        <g key={`${c.system}${c.freqHz}`}>
          <line x1={x(c.freqHz)} x2={x(c.freqHz)} y1={y(c.noiseDb)} y2={y(c.powerDb)} stroke={color(c.system)} strokeWidth={2} vectorEffect="non-scaling-stroke" />
          <circle cx={x(c.freqHz)} cy={y(c.powerDb)} r={c.kind === "control" ? 6 : 4} fill={color(c.system)} className="ring" />
          <title>{`${c.system} ${c.kind} · ${mhz(c.freqHz)} MHz · ${num(c.snrDb)} dB above the floor${c.offsetHz !== null ? ` · ${signed(c.offsetHz, 0)} Hz off` : ""}`}</title>
        </g>
      ))}
      <text x={4} y={H - 4} className="axis">
        {mhz(src.centerHz - src.rateHz / 2, 3)}
      </text>
      <text x={W - 4} y={H - 4} textAnchor="end" className="axis">
        {mhz(src.centerHz + src.rateHz / 2, 3)} MHz
      </text>
      <text x={4} y={12} className="axis">
        {hi} dBFS
      </text>
    </svg>
  );
}

const RANGES: { v: StatsRange; label: string }[] = [
  { v: "1h", label: "1 h" },
  { v: "24h", label: "24 h" },
  { v: "7d", label: "7 days" },
];

function SourceDetail({ s, i }: { s: AppState; i: number }) {
  const src = s.sources[i];
  const [range, setRange] = useState<StatsRange>("24h");
  const [fall, setFall] = useState(false);
  useTopic(`rf:${i}`);
  useTopic(fall ? `spectrum:${i}` : null);
  const k = (x: string) => K.src(src.label, x);
  const hist = useHistory([k("noise"), k("peak"), k("ppm"), k("tune"), k("dropped"), k("clipPct")], range, 360);
  const d = s.rfDetail[i];
  const color = (sys: string) => systemColor(s.config, sys);
  const line = (key: string, label: string, c: string, extra: Partial<Line> = {}): Line => ({ label, color: c, data: hist?.[key] ?? null, ...extra });
  const offsets = (d?.channels ?? []).filter((c) => c.offsetHz !== null);
  const meanOffset = offsets.length ? offsets.reduce((a, c) => a + c.offsetHz! / c.freqHz, 0) / offsets.length : null;
  return (
    <div className="stack">
      <div className="row">
        <button className="btn ghost small" onClick={() => setView("rf")}>
          ← All sources
        </button>
      </div>
      <SourceCard s={s} src={src} i={i} />
      <Card
        title="Across the band"
        actions={
          <label className="toggle small">
            <input type="checkbox" checked={fall} onChange={(e) => setFall(e.target.checked)} />
            <span>Waterfall</span>
          </label>
        }
      >
        {d ? <ChannelPlot profile={d.profile} channels={d.channels} src={src} color={color} /> : <div className="chart-empty">Measuring…</div>}
        <Hint>
          Bars are the noise floor in each slice of the band (the SDR's passband shape shows here: lower at the edges). Stems are channels: how far each stands above the floor under
          it. Control channels want 10 dB or more; voice decodes cleanly from about 8 dB.
        </Hint>
        {fall && s.spectra[i] && (
          <Waterfall radio={s.spectra[i]} label={src.label} ccs={ccMarks(s.config, s.status?.systems ?? [])} calls={s.calls} multi={systemChoices(s).length > 1} colorOf={color} />
        )}
        {fall && !s.spectra[i] && <div className="chart-empty">Waiting for the spectrum…</div>}
      </Card>
      {d && d.channels.length > 0 && (
        <Card title="Channels on this source">
          <div className="table-wrap">
            <table className="calls">
              <thead>
                <tr>
                  <th>MHz</th>
                  <th>System</th>
                  <th>What</th>
                  <th title="Power in the channel">Power</th>
                  <th title="The floor under it">Floor</th>
                  <th title="Power above the floor">SNR</th>
                  <th title="How far the carrier is from where it should be">Offset</th>
                  <th title="The demodulator's eye opening: ~10 and up clean, ~1 noise">Eye</th>
                </tr>
              </thead>
              <tbody>
                {d.channels.map((c) => (
                  <tr key={`${c.system}${c.freqHz}`}>
                    <td className="mono">{formatMhz(c.freqHz)}</td>
                    <td className="sys-name">
                      <span className="sys-dot" style={{ background: color(c.system) }} />
                      {c.system}
                    </td>
                    <td>{c.kind}</td>
                    <td className="mono">{num(c.powerDb)}</td>
                    <td className="mono">{num(c.noiseDb)}</td>
                    <td className="mono">
                      <span className={`snr ${c.snrDb >= 15 ? "ok" : c.snrDb >= 8 ? "warn" : "bad"}`}>{num(c.snrDb, 0)} dB</span>
                    </td>
                    <td className="mono">{c.offsetHz !== null ? `${signed(c.offsetHz, 0)} Hz` : "—"}</td>
                    <td className="mono">{c.quality !== null ? num(c.quality, 1) : "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {meanOffset !== null && Math.abs(meanOffset * 1e6) > 1 && (
            <Hint>
              Every carrier here comes in about {signed(meanOffset * 1e6, 1)} ppm off. When they all agree, it's the source's crystal, not the transmitters: set its ppm (or turn on
              AutoTune).
            </Hint>
          )}
        </Card>
      )}
      <div className="row">
        <h2 className="section-title">Over time</h2>
        <span className="spacer" />
        <Choice value={range} options={RANGES} onChange={setRange} label="Span" />
      </div>
      <div className="columns">
        <Card title="Noise floor and peaks">
          <TimeSeries lines={[line(k("noise"), "Noise floor", "var(--series-1)", { band: true }), line(k("peak"), "Peak", "var(--series-2)")]} fmt={(v) => `${v.toFixed(0)}`} />
          <Hint>dBFS. Peaks near 0 mean clipping; a floor that jumps at certain hours is interference with a schedule.</Hint>
        </Card>
        <Card title="Frequency error">
          <TimeSeries lines={[line(k("ppm"), "Measured", "var(--series-2)", { band: true }), line(k("tune"), "Corrected", "var(--series-1)", { dashed: true })]} fmt={(v) => v.toFixed(2)} />
          <Hint>ppm, measured on the control channels. Drift that follows the day's temperature is normal for RTL-SDRs; a TCXO dongle stays flat.</Hint>
        </Card>
        <Card title="Dropped samples">
          <TimeSeries lines={[line(k("dropped"), "Dropped a second", "var(--critical)")]} zero fmt={(v) => compact(v)} empty="No drops recorded." />
        </Card>
        <Card title="Clipping">
          <TimeSeries lines={[line(k("clipPct"), "Samples at full scale, %", "var(--serious)")]} zero fmt={(v) => v.toFixed(2)} empty="No clipping recorded." />
        </Card>
      </div>
    </div>
  );
}

export function RfPage() {
  const s = useApp();
  const i = s.path[0] !== undefined ? Number(s.path[0]) : null;
  if (!running(s) || !s.sources.length) {
    return (
      <div className="page">
        <p className="empty">Start recording to see the sources. Their history is kept: the charts on each source's page cover the last week.</p>
        {s.config?.sources.map((src, k) => (
          <Card key={k} title={`Source ${k + 1}`}>
            <div className="kv small">
              <span>
                {src.type} · {mhz(src.centerHz, 3)} MHz · {num(src.rateHz / 1e6, 2)} MS/s · gain {gainText(src)}
              </span>
            </div>
          </Card>
        ))}
      </div>
    );
  }
  if (i !== null && s.sources[i]) return <SourceDetail s={s} i={i} />;
  return (
    <div className="page">
      <BandOverview s={s} />
      <div className="card-grid wide">
        {s.sources.map((src, k) => (
          <SourceCard key={k} s={s} src={src} i={k} />
        ))}
      </div>
      <Hint>Pick a source for its band in detail — the floor across it, each channel's level, the waterfall — and its last week.</Hint>
    </div>
  );
}
