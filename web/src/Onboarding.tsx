// The setup guide: a first run, step by step. Pick the radios (or find out
// why none shows up), find one system with the survey, place the radios over
// its voice channels, name it, choose where calls go and — optionally — load
// talkgroup names. Every step edits the same config as the setup form, so
// leaving part way keeps what was done. Experienced users skip it.

import { useEffect, useMemo, useRef, useState } from "react";
import { autoCenter, enabledChannels, formatMhz, importTrunkRecorderConfig, newAirspy, resolvedCenters, sourceCovering, newDongle, newSoapy, newSystem, newUsrp, parseFreqList, startProblem, trConfigFiles, uniqueShortName, usableHalfWidth, type ImportTodo } from "./config.ts";
import {
  applySurvey,
  addDmrSite,
  bumpEpoch,
  dismissError,
  findRadios,
  listDir,
  readTrConfig,
  refreshDevices,
  setTodo,
  start,
  startSurvey,
  stopSurvey,
  surveyListen,
  surveyRescan,
  updateConfig,
  useApp,
  web,
  type AppState,
} from "./controller.ts";
import type { Config, Source, SurveyCandidate, SurveyMonitor, SurveySuggestion, System } from "./protocol.ts";
import { parseRadioReferencePaste, parseTalkgroupCsv, talkgroupsToCsv, type Talkgroup } from "./talkgroups.ts";
import { openTodos } from "./todo.ts";
import { Waterfall } from "./Waterfall.tsx";

// ── when to offer it ─────────────────────────────────────────────────────────

const SEEN_KEY = "trp.setupGuide";

/** Offer the guide: nothing set up yet, and it wasn't finished or skipped in this browser. */
export function guideWanted(c: Config): boolean {
  let seen: string | null = null;
  try {
    seen = localStorage.getItem(SEEN_KEY);
  } catch {
    // Storage blocked: offer it on an empty config anyway.
  }
  return !seen && c.systems.length === 0 && enabledChannels(c).length === 0;
}

function remember(how: "done" | "skipped"): void {
  try {
    localStorage.setItem(SEEN_KEY, how);
  } catch {
    // Not remembered; the guide is offered again only while nothing is set up.
  }
}

// ── steps ────────────────────────────────────────────────────────────────────

type Step = "welcome" | "radios" | "scan" | "coverage" | "name" | "storage" | "talkgroups" | "done";

const STEPS: { id: Step; label: string }[] = [
  { id: "radios", label: "Radios" },
  { id: "scan", label: "Find a system" },
  { id: "coverage", label: "Coverage" },
  { id: "name", label: "Name" },
  { id: "storage", label: "Storage" },
  { id: "talkgroups", label: "Talkgroups" },
];

/** The system the guide added, and what listening to it showed. */
interface Found {
  system: number;
  /** The source that scanned. */
  source: number;
  sug: SurveySuggestion | null;
  voice: SurveyMonitor["voice"];
}

export function SetupGuide(props: { mode: "start" | "import"; onClose: () => void }) {
  const s = useApp();
  const c = s.config;
  const [flow, setFlow] = useState(props.mode);
  const [step, setStep] = useState<Step>("welcome");
  const [found, setFound] = useState<Found | null>(null);
  const defaultDir = useRef(c?.recording.captureDir ?? "");
  const body = useRef<HTMLDivElement>(null);
  const order: Step[] = ["welcome", ...STEPS.filter((x) => !web || x.id !== "storage").map((x) => x.id), "done"];
  const go = (to: Step) => {
    setStep(to);
    body.current?.scrollTo({ top: 0 });
  };
  const next = () => go(order[Math.min(order.length - 1, order.indexOf(step) + 1)]);
  const back = () => go(order[Math.max(0, order.indexOf(step) - 1)]);
  const leave = (how: "done" | "skipped") => {
    if (s.survey.stage !== "idle") stopSurvey();
    remember(how);
    props.onClose();
  };
  const sys = found && c ? c.systems[found.system] : undefined;
  // The page behind doesn't scroll under the guide.
  useEffect(() => {
    const was = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => void (document.body.style.overflow = was);
  }, []);

  return (
    <div className="ob" role="dialog" aria-modal="true" aria-label="Setup guide" ref={body}>
      <header className="ob-top">
        <div className="ob-brand">
          <span className="logo" aria-hidden="true" />
          <span>Setup guide</span>
        </div>
        {flow === "start" && step !== "welcome" && step !== "done" && <Progress steps={STEPS.filter((x) => order.includes(x.id))} at={step} />}
        <button className="ob-link" onClick={() => leave(flow === "import" ? "done" : "skipped")}>
          {flow === "start" && step === "welcome" ? "Skip" : flow === "import" ? "Close" : "Exit guide"}
        </button>
      </header>
      <main className="ob-body">
        {!s.connected && <div className="ob-note bad">Waiting for the recorder… is Trunk Recorder Pro running?</div>}
        {s.error && (
          <div className="ob-note bad" role="alert">
            <span>{s.error}</span>
            <button className="ob-link" onClick={dismissError}>
              Dismiss
            </button>
          </div>
        )}
        {!c ? (
          <p className="ob-lead">Connecting…</p>
        ) : flow === "import" ? (
          <ImportFlow s={s} c={c} onBack={props.mode === "start" ? () => setFlow("start") : undefined} onStart={() => (leave("done"), start())} onClose={() => leave("done")} />
        ) : step === "welcome" ? (
          <Welcome onStart={next} onImport={() => setFlow("import")} onSkip={() => leave("skipped")} ready={s.connected} />
        ) : step === "radios" ? (
          <RadiosStep s={s} c={c} onNext={next} onBack={back} />
        ) : step === "scan" ? (
          <ScanStep s={s} c={c} found={found} onFound={(f) => (setFound(f), next())} onBack={back} onNext={next} />
        ) : step === "coverage" ? (
          <CoverageStep c={c} found={found} onNext={next} onBack={back} />
        ) : step === "name" && sys ? (
          <NameStep c={c} sys={sys} index={found!.system} onNext={next} onBack={back} />
        ) : step === "storage" ? (
          <StorageStep s={s} c={c} defaultDir={defaultDir.current} onNext={next} onBack={back} />
        ) : step === "talkgroups" && sys ? (
          <TalkgroupsStep sys={sys} index={found!.system} onNext={next} onBack={back} />
        ) : step === "done" ? (
          <DoneStep s={s} c={c} sys={sys} onStart={() => (leave("done"), start())} onClose={() => leave("done")} />
        ) : (
          // Name / talkgroups with no system (a step was reached without adding one).
          <Missing onBack={() => go("scan")} />
        )}
      </main>
    </div>
  );
}

function Progress(props: { steps: { id: Step; label: string }[]; at: Step }) {
  const at = props.steps.findIndex((x) => x.id === props.at);
  return (
    <ol className="ob-progress" aria-label="Steps">
      {props.steps.map((x, i) => (
        <li key={x.id} className={i < at ? "done" : i === at ? "on" : ""} aria-current={i === at ? "step" : undefined}>
          <span className="ob-dot" aria-hidden="true">
            {i < at ? "✓" : i + 1}
          </span>
          <span className="ob-step-label">{x.label}</span>
        </li>
      ))}
    </ol>
  );
}

/** Back / Continue at the foot of a step. */
function Nav(props: { onBack?: () => void; onNext?: () => void; nextLabel?: string; disabled?: boolean; why?: string; children?: React.ReactNode }) {
  return (
    <footer className="ob-nav">
      {props.onBack ? (
        <button className="ob-btn ghost" onClick={props.onBack}>
          Back
        </button>
      ) : (
        <span />
      )}
      <div className="ob-nav-right">
        {props.disabled && props.why && <span className="ob-why">{props.why}</span>}
        {props.children}
        {props.onNext && (
          <button className="ob-btn primary" disabled={props.disabled} onClick={props.onNext}>
            {props.nextLabel ?? "Continue"}
          </button>
        )}
      </div>
    </footer>
  );
}

function Head(props: { art: React.ReactNode; title: string; lead?: React.ReactNode }) {
  return (
    <div className="ob-head">
      {props.art && <div className="ob-art">{props.art}</div>}
      <h2 className="ob-title">{props.title}</h2>
      {props.lead && <p className="ob-lead">{props.lead}</p>}
    </div>
  );
}

function Missing(props: { onBack: () => void }) {
  return (
    <section className="ob-step">
      <Head art={<ArtSearch />} title="First, find a system" lead="This step needs a system to work on." />
      <Nav onBack={props.onBack} />
    </section>
  );
}

// ── welcome ──────────────────────────────────────────────────────────────────

function Welcome(props: { onStart: () => void; onImport: () => void; onSkip: () => void; ready: boolean }) {
  return (
    <section className="ob-step ob-welcome">
      <div className="ob-hero">
        <ArtWelcome />
      </div>
      <h1 className="ob-title big">Let&apos;s start recording</h1>
      <p className="ob-lead">A few short steps to set up your radio and find a system near you. Everything can be changed later.</p>
      <ol className="ob-flow" aria-label="What happens">
        <li>
          <IconDongle />
          <span>Connect your radio</span>
        </li>
        <li aria-hidden="true" className="ob-flow-arrow">
          →
        </li>
        <li>
          <IconTower />
          <span>Find a system</span>
        </li>
        <li aria-hidden="true" className="ob-flow-arrow">
          →
        </li>
        <li>
          <IconWave />
          <span>Record calls</span>
        </li>
      </ol>
      <div className="ob-center-actions">
        <button className="ob-btn primary big" disabled={!props.ready} onClick={props.onStart}>
          Get started
        </button>
        <button className="ob-btn ghost" disabled={!props.ready} onClick={props.onImport}>
          I have a Trunk Recorder config
        </button>
        <button className="ob-link" onClick={props.onSkip}>
          I&apos;ve done this before — go straight to the app
        </button>
      </div>
    </section>
  );
}

// ── radios ───────────────────────────────────────────────────────────────────

interface Detected {
  key: string;
  kind: Source["kind"];
  name: string;
  detail: string;
  source: Source;
  /** Why it can't be used now (another program has it), or null. */
  busy: string | null;
}

/** A source's identity, as a detected radio's key. */
function sourceKey(x: Source): string {
  switch (x.kind) {
    case "rtlsdr":
      return `r:${x.serial}`;
    case "airspy":
      return `a:${x.serial}`;
    case "usrp":
      return `u:${x.args}`;
    case "soapy":
      return `s:${x.args}`;
    default:
      return "";
  }
}

/** Every radio the recorder can see, as a source each. */
function detectRadios(s: AppState): Detected[] {
  const out: Detected[] = s.devices.map((d) => ({
    key: `r:${d.serial}`,
    kind: "rtlsdr",
    name: d.product || "RTL-SDR",
    detail: d.serial ? `Serial ${d.serial}` : "RTL-SDR",
    source: { ...newDongle(), serial: d.serial },
    busy: d.busy ?? null,
  }));
  const r = s.radios;
  for (const d of r?.airspy.devices ?? []) out.push({ key: `a:${d.serial ?? ""}`, kind: "airspy", name: "Airspy", detail: d.label, source: { ...newAirspy(), serial: d.serial ?? "" } as Source, busy: null });
  for (const d of r?.usrp.devices ?? []) out.push({ key: `u:${d.args ?? ""}`, kind: "usrp", name: "USRP", detail: d.label, source: { ...newUsrp(), args: d.args ?? "" } as Source, busy: null });
  // SoapySDR also lists radios with a driver of their own here; those are shown once, natively.
  const native = new Set([...(s.devices.length ? ["rtlsdr"] : []), ...(r?.airspy.devices?.length ? ["airspy"] : []), ...(r?.usrp.devices?.length ? ["uhd"] : [])]);
  for (const d of r?.soapy?.devices ?? []) {
    if (native.has(d.driver)) continue;
    out.push({ key: `s:${d.args}`, kind: "soapy", name: d.label || d.driver, detail: `SoapySDR · ${d.driver}`, source: { ...newSoapy(), args: d.args } as Source, busy: null });
  }
  // Two dongles with no serial can't be told apart; keep one.
  return out.filter((d, i) => out.findIndex((o) => o.key === d.key) === i);
}

async function connectWebDongle(): Promise<void> {
  try {
    await web?.requestUsb();
  } catch {
    // No WebUSB: the troubleshooter says to use Chrome or Edge.
  }
}

function RadiosStep(props: { s: AppState; c: Config; onNext: () => void; onBack: () => void }) {
  const { s } = props;
  const detected = useMemo(() => detectRadios(s), [s.devices, s.radios]);
  const [picked, setPicked] = useState<Set<string> | null>(null);
  const [helping, setHelping] = useState(false);
  const free = detected.filter((d) => !d.busy);
  const chosen = picked ?? new Set(free.map((d) => d.key));
  const selected = free.filter((d) => chosen.has(d.key));
  const look = () => {
    refreshDevices();
    if (!web) findRadios();
  };
  useEffect(look, []);

  const toggle = (key: string) => {
    const n = new Set(chosen);
    if (n.has(key)) n.delete(key);
    else n.add(key);
    setPicked(n);
  };
  const use = () => {
    updateConfig((x) => {
      // A radio already set up keeps its settings (frequency correction, gain).
      x.sources = selected.map((d) => x.sources.find((o) => sourceKey(o) === d.key) ?? d.source);
    });
    bumpEpoch();
    props.onNext();
  };

  if (helping) return <Troubleshooter s={s} found={free.length} onFound={() => setHelping(false)} onBack={() => setHelping(false)} look={look} />;

  return (
    <section className="ob-step">
      <Head
        art={detected.length ? <ArtDongle /> : <ArtSearch />}
        title={!detected.length ? "Let's connect your radio" : free.length ? "Which radios should record?" : "Your radios are busy"}
        lead={
          !detected.length
            ? "Plug in your SDR — an RTL-SDR dongle, Airspy, HackRF or similar — then check again."
            : !free.length
              ? "Another program is using them. Close it, then look again."
              : free.length === 1
                ? "We found one you can use. More radios can listen to more of a system at once."
                : `We found ${free.length} you can use. Use them all to hear more of a system at once.`
        }
      />
      {detected.length > 0 && (
        <div className="ob-cards" role="group" aria-label="Radios">
          {[...free, ...detected.filter((d) => d.busy)].map((d) => {
            const on = !d.busy && chosen.has(d.key);
            return (
              <button key={d.key} className={`ob-card pick${on ? " on" : ""}${d.busy ? " busy" : ""}`} role="checkbox" aria-checked={on} disabled={!!d.busy} onClick={() => toggle(d.key)}>
                {!d.busy && (
                  <span className="ob-check" aria-hidden="true">
                    {on ? "✓" : ""}
                  </span>
                )}
                <span className="ob-card-art">{d.kind === "usrp" || d.kind === "soapy" ? <IconBox /> : <IconDongle />}</span>
                <span className="ob-card-title">{d.name}</span>
                <span className="ob-card-sub">{d.detail}</span>
                {d.busy && <span className="ob-busy">{d.busy.replace(/^./, (ch) => ch.toUpperCase())}</span>}
              </button>
            );
          })}
        </div>
      )}
      <div className="ob-center-actions">
        {detected.length === 0 && (
          <div className="ob-row">
            {web ? (
              <button className="ob-btn primary" onClick={() => void connectWebDongle()}>
                Connect a radio
              </button>
            ) : (
              <button className="ob-btn primary" onClick={look}>
                Check again
              </button>
            )}
            <button className="ob-btn ghost" onClick={() => setHelping(true)}>
              Help me find it
            </button>
          </div>
        )}
        {detected.length > 0 && (
          <div className="ob-row">
            {web ? (
              <button className="ob-link" onClick={() => void connectWebDongle()}>
                Connect another radio
              </button>
            ) : (
              <button className="ob-link" onClick={look}>
                Look again
              </button>
            )}
            <button className="ob-link" onClick={() => setHelping(true)}>
              A radio is missing
            </button>
          </div>
        )}
        {s.findingRadios && (
          <p className="ob-quiet">
            <span className="ob-spin" aria-hidden="true" /> Still looking for other kinds of radio…
          </p>
        )}
      </div>
      <Nav onBack={props.onBack} onNext={use} disabled={!selected.length} why={detected.length ? "Pick at least one radio" : undefined} nextLabel={selected.length > 1 ? `Use ${selected.length} radios` : "Continue"} />
    </section>
  );
}

// ── troubleshooter ───────────────────────────────────────────────────────────

type Os = "windows" | "macos" | "linux";

function guessOs(platform: string): Os {
  if (platform === "windows" || platform === "macos" || platform === "linux") return platform;
  const ua = navigator.userAgent;
  return /Windows/.test(ua) ? "windows" : /Mac/.test(ua) ? "macos" : "linux";
}

interface Tip {
  title: string;
  art: React.ReactNode;
  body: React.ReactNode;
  cmd?: string;
}

const UDEV_RULE = `echo 'SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0666"' | sudo tee /etc/udev/rules.d/20-rtlsdr.rules
sudo udevadm control --reload-rules && sudo udevadm trigger`;
const DVB_BLACKLIST = `echo 'blacklist dvb_usb_rtl28xxu' | sudo tee /etc/modprobe.d/blacklist-rtlsdr.conf
sudo rmmod dvb_usb_rtl28xxu`;

const DRIVERS: Record<Os, { airspy: string; usrp: string; soapy: string }> = {
  macos: { airspy: "brew install airspy", usrp: "brew install uhd", soapy: "brew install soapysdr soapyhackrf" },
  linux: { airspy: "sudo apt install libairspy0", usrp: "sudo apt install libuhd-dev uhd-host", soapy: "sudo apt install soapysdr0.8-module-all" },
  windows: { airspy: "", usrp: "", soapy: "" },
};

function tipsFor(os: Os, s: AppState): Tip[] {
  const tips: Tip[] = [
    {
      title: "Plug it straight into the computer",
      art: <IconPlug />,
      body: <p>Use a USB port on the computer itself — not a hub or extension cable. If one port doesn&apos;t work, try another.</p>,
    },
    {
      title: "Close other radio apps",
      art: <IconApps />,
      body: <p>Only one program can use a radio at a time. Quit SDR#, SDR++, GQRX, CubicSDR, rtl_tcp or another copy of Trunk Recorder, then check again.</p>,
    },
  ];
  if (web) {
    tips.push({
      title: "Use Chrome or Edge",
      art: <IconLaptop />,
      body: (
        <>
          <p>The browser version reaches the radio through WebUSB, which only Chrome and Edge have. Press the button and choose your radio in the list.</p>
          <button className="ob-btn primary" onClick={() => void connectWebDongle()}>
            Connect a radio
          </button>
        </>
      ),
    });
  }
  if (os === "windows") {
    tips.push({
      title: "Install the USB driver",
      art: <IconChip />,
      body: (
        <>
          <p>Windows needs a one-time driver change, done with the free Zadig tool:</p>
          <ol className="ob-numbered">
            <li>
              Download Zadig from{" "}
              <a href="https://zadig.akeo.ie" target="_blank" rel="noreferrer">
                zadig.akeo.ie
              </a>{" "}
              and open it.
            </li>
            <li>
              Choose <b>Options → List All Devices</b>.
            </li>
            <li>
              Pick <b>Bulk-In, Interface (Interface 0)</b>.
            </li>
            <li>
              Make sure <b>WinUSB</b> is selected, then press <b>Replace Driver</b>.
            </li>
          </ol>
          <p>Unplug the radio, plug it back in, and check again.</p>
        </>
      ),
    });
  } else if (os === "linux") {
    tips.push(
      {
        title: "Let your account use the radio",
        art: <IconKey />,
        body: <p>Linux only lets the administrator open USB radios until you add a rule. Run this in a terminal, then unplug the radio and plug it back in.</p>,
        cmd: UDEV_RULE,
      },
      {
        title: "Stop the TV-tuner driver",
        art: <IconChip />,
        body: <p>RTL-SDR dongles started life as TV tuners, and Linux may grab them as one. This stops that, now and after a restart.</p>,
        cmd: DVB_BLACKLIST,
      },
    );
  } else {
    tips.push({
      title: "Check that your Mac sees it",
      art: <IconLaptop />,
      body: (
        <p>
          Open <b>Apple menu → About This Mac → More Info → System Report → USB</b>. Look for <b>RTL2838</b> or your radio&apos;s name. If it isn&apos;t listed,
          try another cable or port — the Mac can&apos;t see it at all.
        </p>
      ),
    });
  }
  if (!web && s.radios) {
    const r = s.radios;
    const d = DRIVERS[os];
    const missing = [!r.airspy.available && ["Airspy", d.airspy], !r.usrp.available && ["USRP (UHD)", d.usrp], r.soapy && !r.soapy.available && ["HackRF, SDRplay, LimeSDR… (SoapySDR)", d.soapy]].filter(
      (x): x is [string, string] => !!x,
    );
    tips.push({
      title: "Not an RTL-SDR?",
      art: <IconBox />,
      body: (
        <>
          <p>Other radios need their own driver installed. Here is what this computer has:</p>
          <ul className="ob-drivers">
            <li className="ok">✓ RTL-SDR — built in</li>
            <li className={r.airspy.available ? "ok" : "no"}>{r.airspy.available ? "✓" : "✕"} Airspy</li>
            <li className={r.usrp.available ? "ok" : "no"}>{r.usrp.available ? "✓" : "✕"} USRP</li>
            {r.soapy && <li className={r.soapy.available ? "ok" : "no"}>{r.soapy.available ? "✓" : "✕"} SoapySDR (HackRF, SDRplay, LimeSDR, PlutoSDR…)</li>}
          </ul>
          {missing.length > 0 &&
            (os === "windows" ? (
              <p>On Windows, install PothosSDR (SoapySDR and most radios), or your radio maker&apos;s driver, then restart Trunk Recorder Pro.</p>
            ) : (
              <>
                <p>To add one, install it and restart Trunk Recorder Pro:</p>
                {missing.map(([name, cmd]) => (
                  <Command key={name} label={name} cmd={cmd} />
                ))}
              </>
            ))}
        </>
      ),
    });
  }
  tips.push({
    title: "Still stuck?",
    art: <IconQuestion />,
    body: (
      <>
        <p>Try the radio in another program, such as the free SDR++. If it doesn&apos;t work there either, the radio or its cable may be faulty.</p>
        <p>If it works elsewhere, you can still set everything up by hand in the app — use <b>Exit guide</b> above.</p>
      </>
    ),
  });
  return tips;
}

function Troubleshooter(props: { s: AppState; found: number; onFound: () => void; onBack: () => void; look: () => void }) {
  const [os, setOs] = useState<Os>(() => guessOs(props.s.platform));
  const tips = tipsFor(os, props.s);
  const [i, setI] = useState(0);
  const tip = tips[Math.min(i, tips.length - 1)];
  const [checking, setChecking] = useState<"no" | "busy" | "none">("no");
  const before = useRef(props.found);
  const check = () => {
    before.current = props.found;
    setChecking("busy");
    props.look();
  };
  useEffect(() => {
    if (checking !== "busy") return;
    const t = setTimeout(() => setChecking("none"), 2500);
    return () => clearTimeout(t);
  }, [checking]);
  const newlyFound = props.found > before.current;

  return (
    <section className="ob-step">
      <div className="ob-trouble-top">
        <span className="ob-kicker">
          Tip {Math.min(i, tips.length - 1) + 1} of {tips.length}
        </span>
        <div className="ob-seg" role="radiogroup" aria-label="Your computer">
            {(["windows", "macos", "linux"] as Os[]).map((o) => (
              <button
                key={o}
                role="radio"
                aria-checked={os === o}
                className={os === o ? "on" : ""}
                onClick={() => {
                  setOs(o);
                  setI(0);
                }}
              >
                {o === "windows" ? "Windows" : o === "macos" ? "Mac" : "Linux"}
              </button>
            ))}
        </div>
      </div>
      <div className="ob-trouble-dots" aria-hidden="true">
        {tips.map((_, k) => (
          <span key={k} className={k === i ? "on" : k < i ? "done" : ""} />
        ))}
      </div>
      <div className="ob-tip">
        <div className="ob-tip-art">{tip.art}</div>
        <div className="ob-tip-text">
          <h2 className="ob-title">{tip.title}</h2>
          {tip.body}
          {tip.cmd && <Command cmd={tip.cmd} />}
        </div>
      </div>
      {newlyFound ? (
        <div className="ob-note ok">
          <span>
            <b>Found it!</b> {props.found === 1 ? "A radio is" : `${props.found} radios are`} connected.
          </span>
          <button className="ob-btn primary" onClick={props.onFound}>
            Use it
          </button>
        </div>
      ) : checking === "none" ? (
        <div className="ob-note">Still not there. Try the next tip.</div>
      ) : null}
      <footer className="ob-nav">
        <button className="ob-btn ghost" onClick={() => (i > 0 ? setI(i - 1) : props.onBack())}>
          Back
        </button>
        <div className="ob-nav-right">
          <button className="ob-btn" onClick={check} disabled={checking === "busy"}>
            {checking === "busy" ? "Checking…" : "Check again"}
          </button>
          {i < tips.length - 1 && (
            <button
              className="ob-btn primary"
              onClick={() => {
                setI(i + 1);
                setChecking("no");
              }}
            >
              Next tip
            </button>
          )}
        </div>
      </footer>
    </section>
  );
}

function Command(props: { cmd: string; label?: string }) {
  const [copied, setCopied] = useState(false);
  if (!props.cmd) return null;
  const copy = () => {
    void navigator.clipboard?.writeText(props.cmd).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  };
  return (
    <div className="ob-cmd">
      {props.label && <span className="ob-cmd-label">{props.label}</span>}
      <pre>{props.cmd}</pre>
      <button className="ob-btn small" onClick={copy}>
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

// ── find a system ────────────────────────────────────────────────────────────

const isControl = (x: SurveyCandidate) => x.kind === "control" || x.kind === "smartnet";
const hex = (v: number | null | undefined) => (v == null ? "?" : v.toString(16).toUpperCase());
const raster = (hz: number) => Math.round(hz / 6250) * 6250;

function ScanStep(props: { s: AppState; c: Config; found: Found | null; onFound: (f: Found) => void; onBack: () => void; onNext: () => void }) {
  const { s, c, found } = props;
  const sv = s.survey;
  const radios = c.sources.map((x, i) => ({ x, i })).filter(({ x }) => x.kind !== "file");
  const [source, setSource] = useState(radios[0]?.i ?? 0);
  const [bands, setBands] = useState<string[] | null>(null);
  const [manual, setManual] = useState(false);
  const picked = bands ?? s.surveyBands.filter((b) => b.defaultOn).map((b) => b.id);
  const unsupported = web && ["usrp", "airspy", "soapy"].includes(c.sources[source]?.kind ?? "");
  const scan = () => startSurvey(source, picked, c.sources[source]?.kind === "rtlsdr");
  // Re-scanning after adding one replaces it rather than adding a second.
  const target = found ? found.system : ("new" as const);

  const useIt = (sug: SurveySuggestion, m: SurveyMonitor) => {
    const system = target === "new" ? c.systems.length : target;
    applySurvey(sv.stage === "idle" ? source : sv.source, sug, target);
    stopSurvey();
    props.onFound({ system, source: sv.stage === "idle" ? source : sv.source, sug, voice: m.voice });
  };
  const useDmr = (cand: SurveyCandidate) => {
    const system = c.systems.length;
    addDmrSite(raster(cand.correctedHz ?? cand.freqHz), cand.dmr?.colorCode ?? null);
    stopSurvey();
    props.onFound({ system, source, sug: null, voice: [] });
  };

  if (manual) return <ManualSystem c={c} target={target} onDone={(system) => props.onFound({ system, source, sug: null, voice: [] })} onBack={() => setManual(false)} />;

  // Came back after adding one.
  if (found && sv.stage === "idle" && c.systems[found.system]) {
    const sys = c.systems[found.system];
    return (
      <section className="ob-step">
        <Head art={<ArtDone />} title="System added" lead={<>{sys.name || sys.shortName} is set up. Continue, or scan again to replace it.</>} />
        <div className="ob-center-actions">
          <button className="ob-link" onClick={scan}>
            Scan again
          </button>
        </div>
        <Nav onBack={props.onBack} onNext={props.onNext} />
      </section>
    );
  }

  if (sv.stage === "idle") {
    return (
      <section className="ob-step">
        <Head
          art={<ArtScan still />}
          title="Find a system near you"
          lead="Your radio will sweep the public-safety bands and listen for a trunked system's control channel. It takes a minute or two."
        />
        <p className="ob-aside">Start with one system. You can add more any time from the main screen.</p>
        {radios.length > 1 && (
          <div className="ob-field">
            <span className="ob-label">Scan with</span>
            <div className="ob-seg">
              {radios.map(({ x, i }) => (
                <button key={i} className={source === i ? "on" : ""} onClick={() => setSource(i)}>
                  Radio {i + 1} · {kindName(x)}
                </button>
              ))}
            </div>
          </div>
        )}
        <div className="ob-more">
          <h3 className="ob-more-title">Bands to scan</h3>
          <div className="ob-chips">
            {s.surveyBands.map((b) => (
              <button
                key={b.id}
                className={`ob-chip${picked.includes(b.id) ? " on" : ""}`}
                aria-pressed={picked.includes(b.id)}
                onClick={() => setBands(picked.includes(b.id) ? picked.filter((x) => x !== b.id) : [...picked, b.id])}
              >
                {b.label}
                <span className="ob-chip-sub">
                  {(b.loHz / 1e6).toFixed(0)}–{(b.hiHz / 1e6).toFixed(0)} MHz
                </span>
              </button>
            ))}
          </div>
        </div>
        {unsupported && <div className="ob-note bad">This radio needs the desktop app to scan.</div>}
        <div className="ob-center-actions">
          <button className="ob-btn primary big" disabled={!s.connected || s.phase !== "idle" || unsupported || !picked.length || !s.surveyBands.length} onClick={scan}>
            Start scan
          </button>
          <button className="ob-link" onClick={() => setManual(true)}>
            I already know the control channel
          </button>
        </div>
        <Nav onBack={props.onBack} />
      </section>
    );
  }

  const m = sv.monitor;
  const controls = sv.candidates.filter(isControl);
  const dmr = sv.candidates.filter((x) => x.kind === "dmrControl");
  const bandLabel = (id: string) => s.surveyBands.find((b) => b.id === id)?.label ?? id;

  if (sv.stage === "scanning") {
    const p = sv.progress;
    return (
      <section className="ob-step">
        <Head art={<ArtScan />} title="Scanning…" lead={p ? <>Listening around {bandLabel(p.band)}</> : "Getting the radio ready"} />
        {p && (
          <div className="ob-meter">
            <progress value={p.hop - 1} max={p.hops} />
            <span>
              {Math.round(((p.hop - 1) / p.hops) * 100)}% · {controls.length + dmr.length ? `${controls.length + dmr.length} found so far` : "nothing yet"}
            </span>
          </div>
        )}
        {sv.error && <div className="ob-note bad">{sv.error}</div>}
        <Nav onBack={() => stopSurvey()} />
      </section>
    );
  }

  if (m) {
    const sug = sv.suggest;
    const total = m.good + m.bad;
    const pct = total ? Math.round((100 * m.good) / total) : 0;
    const id = m.identity;
    const others = controls.filter((x) => Math.abs(x.freqHz - (m.freqHz ?? m.heardHz)) > 6000);
    return (
      <section className="ob-step">
        <Head
          art={<ArtTower />}
          title={sug ? "Found a system" : "Found one — listening…"}
          lead={
            <>
              Control channel <b className="ob-mono">{formatMhz(m.advertisedHz ?? m.heardHz, 4)} MHz</b>
              {m.smartnet ? " · SmartNet" : " · P25"}
              {sv.stage === "monitoring" && <span className="ob-quiet"> · {m.elapsedS.toFixed(0)} s</span>}
            </>
          }
        />
        <ul className="ob-checks">
          <Check ok={m.good >= 10} title="Hearing it clearly">
            {total ? `${pct}% of messages decoded` : "Waiting for messages…"}
          </Check>
          <Check ok={id.sysId !== null} title="Who it is">
            {id.sysId !== null ? (
              <span className="ob-mono">
                System ID {hex(id.sysId)}
                {id.wacn !== null ? ` · WACN ${hex(id.wacn)}` : ""}
                {id.site !== null ? ` · site ${id.rfss ?? "?"}-${id.site}` : ""}
              </span>
            ) : (
              "Waiting for it to say…"
            )}
          </Check>
          <Check ok={m.ppm !== null} title="Radio tuning">
            {m.ppm !== null ? `Corrected by ${m.ppm > 0 ? "+" : ""}${m.ppm.toFixed(1)} ppm` : "Measuring…"}
          </Check>
          <Check ok={m.voice.length >= 3} title="Voice channels">
            {m.voice.length ? `${m.voice.length} seen so far — listening longer finds more` : "None yet — they show up as people talk"}
          </Check>
        </ul>
        {sv.stage === "monitoring" && s.surveySpectrum && (
          <div className="ob-waterfall">
            <Waterfall radio={s.surveySpectrum} label="What the radio hears" ccs={[{ hz: m.heardHz, label: "Control", color: "var(--accent)" }]} calls={[]} />
          </div>
        )}
        {others.length > 0 && (
          <details className="ob-more">
            <summary>Not the one you want? {others.length} other control channel{others.length === 1 ? "" : "s"} found</summary>
            <div className="ob-chips">
              {others.map((x) => (
                <button key={x.freqHz} className="ob-chip" onClick={() => surveyListen(x.freqHz)}>
                  <span className="ob-mono">{formatMhz(x.correctedHz ?? x.freqHz, 4)}</span>
                  <span className="ob-chip-sub">{x.identity.sysId !== null ? `System ID ${hex(x.identity.sysId)}` : x.kind === "smartnet" ? "SmartNet" : "P25"}</span>
                </button>
              ))}
            </div>
          </details>
        )}
        <Nav onBack={() => stopSurvey()} onNext={sug ? () => useIt(sug, m) : undefined} nextLabel="Use this system" disabled={!sug} why="Learning about it…">
          {sug && !m.ready && <span className="ob-why">A few more seconds gives a better result</span>}
        </Nav>
      </section>
    );
  }

  // Scanned, and nothing to listen to.
  return (
    <section className="ob-step">
      <Head
        art={<ArtSearch />}
        title={dmr.length ? "Found a DMR system" : "No system found"}
        lead={dmr.length ? "It is a trunked DMR site. Add it, or scan again for P25 and SmartNet." : "Nothing trunked was heard in the bands scanned."}
      />
      {sv.error && <div className="ob-note bad">{sv.error}</div>}
      {dmr.length > 0 ? (
        <div className="ob-cards">
          {dmr.map((x) => (
            <button key={x.freqHz} className="ob-card" onClick={() => useDmr(x)}>
              <span className="ob-card-art">
                <IconTower />
              </span>
              <span className="ob-card-title ob-mono">{formatMhz(raster(x.correctedHz ?? x.freqHz), 4)} MHz</span>
              <span className="ob-card-sub">
                {x.dmr?.variant ?? "DMR"}
                {x.dmr?.colorCode != null ? ` · colour code ${x.dmr.colorCode}` : ""}
              </span>
            </button>
          ))}
        </div>
      ) : (
        <ul className="ob-tips">
          <li>
            <IconAntenna /> Put the antenna near a window, or higher up.
          </li>
          <li>
            <IconTower /> Try more bands — press Back and tick more of them.
          </li>
          <li>
            <IconQuestion /> Look up your county&apos;s systems on RadioReference.com and enter the control channel yourself.
          </li>
        </ul>
      )}
      <div className="ob-center-actions">
        <div className="ob-row">
          <button className="ob-btn primary" onClick={surveyRescan}>
            Scan again
          </button>
          <button className="ob-btn ghost" onClick={() => (stopSurvey(), setManual(true))}>
            Enter it myself
          </button>
        </div>
      </div>
      <Nav onBack={() => stopSurvey()} />
    </section>
  );
}

function kindName(x: Source): string {
  return x.kind === "rtlsdr" ? "RTL-SDR" : x.kind === "usrp" ? "USRP" : x.kind === "airspy" ? "Airspy" : x.kind === "soapy" ? "SoapySDR" : "file";
}

function Check(props: { ok: boolean; title: string; children: React.ReactNode }) {
  return (
    <li className={props.ok ? "ok" : "wait"}>
      <span className="ob-check-mark" aria-hidden="true">
        {props.ok ? "✓" : <span className="ob-spin" />}
      </span>
      <span>
        <b>{props.title}</b>
        <span className="ob-check-sub">{props.children}</span>
      </span>
    </li>
  );
}

/** A control channel typed in: the system made from it. */
function ManualSystem(props: { c: Config; target: number | "new"; onDone: (system: number) => void; onBack: () => void }) {
  const [text, setText] = useState("");
  const [type, setType] = useState<System["type"]>("p25");
  const ccs = parseFreqList(text);
  const add = () => {
    const system = props.target === "new" ? props.c.systems.length : props.target;
    updateConfig((x) => {
      const sys = newSystem(x, { controlChannels: ccs, type, ...(type === "smartnet" ? { bandplan: "800_reband" } : {}) });
      if (props.target === "new") x.systems.push(sys);
      else x.systems[props.target] = { ...sys, shortName: x.systems[props.target].shortName, name: x.systems[props.target].name };
    });
    bumpEpoch();
    props.onDone(system);
  };
  return (
    <section className="ob-step">
      <Head art={<ArtTower />} title="Enter the control channel" lead="RadioReference.com lists each system's control channels — they are the frequencies marked in red or with a “c”." />
      <div className="ob-form">
        <label className="ob-field">
          <span className="ob-label">Control channel, MHz</span>
          <input className="ob-input ob-mono" value={text} placeholder="851.0125" onChange={(e) => setText(e.target.value)} autoFocus />
          <span className="ob-hint">Several? Separate them with commas.</span>
        </label>
        <div className="ob-field">
          <span className="ob-label">Kind of system</span>
          <div className="ob-seg">
            {(["p25", "smartnet", "dmr"] as const).map((t) => (
              <button key={t} className={type === t ? "on" : ""} onClick={() => setType(t)}>
                {t === "p25" ? "P25" : t === "smartnet" ? "SmartNet" : "DMR"}
              </button>
            ))}
          </div>
        </div>
      </div>
      <Nav onBack={props.onBack} onNext={add} disabled={!ccs.length} why="Enter a frequency" nextLabel="Add it" />
    </section>
  );
}

// ── coverage: where to tune each radio ───────────────────────────────────────

interface Chan {
  hz: number;
  /** Calls seen on it (1 when unknown). */
  weight: number;
}

const covers = (center: number, rate: number, hz: number) => Math.abs(hz - center) <= usableHalfWidth(rate);

/** Off the DC spike at the centre: nudge until no channel sits within 10 kHz, keeping what it covers. */
function offDc(center: number, rate: number, chans: number[]): number {
  const n = (c: number) => chans.filter((f) => covers(c, rate, f)).length;
  const want = n(center);
  for (let k = 0; k < 24; k++) {
    const c = center + (k % 2 ? -1 : 1) * Math.ceil(k / 2) * 15_000;
    if (chans.every((f) => Math.abs(f - c) > 10_000) && n(c) >= want) return Math.round(c / 1000) * 1000;
  }
  return center;
}

/**
 * Centres for every radio: the scanning one where the survey put it (it
 * covers the control channel), the others over the busiest voice channels
 * still out of reach — or next to the first when nothing is.
 */
function planCenters(rates: (number | null)[], fixed: (number | null)[], first: number, firstCenter: number, ccs: number[], voice: Chan[]): (number | null)[] {
  const out: (number | null)[] = fixed.slice();
  if (fixed[first] === null) out[first] = firstCenter;
  const all = [...ccs, ...voice.map((v) => v.hz)];
  const reached = (hz: number) => out.some((c, k) => c !== null && rates[k] !== null && covers(c, rates[k]!, hz));
  rates.forEach((rate, k) => {
    if (rate === null || out[k] !== null) return;
    const half = usableHalfWidth(rate);
    const left = voice.filter((v) => !reached(v.hz));
    let best: number;
    if (left.length) {
      // Each channel still out of reach as the window's low edge; keep the window that takes in the most calls.
      let score = -1;
      best = left[0].hz;
      for (const v of left) {
        const c = v.hz + half - 1000;
        const sc = left.filter((o) => covers(c, rate, o.hz)).reduce((a, o) => a + o.weight, 0);
        if (sc > score) [score, best] = [sc, c];
      }
    } else {
      // Nothing left: alongside the first, on the side with more of the system.
      const mid = all.reduce((a, f) => a + f, 0) / Math.max(1, all.length);
      const firstHalf = usableHalfWidth(rates[first] ?? rate);
      best = firstCenter + (mid >= firstCenter ? 1 : -1) * (firstHalf + half);
    }
    out[k] = offDc(best, rate, all);
  });
  return out;
}

function CoverageStep(props: { c: Config; found: Found | null; onNext: () => void; onBack: () => void }) {
  const { c, found } = props;
  const sys = found ? c.systems[found.system] : undefined;
  const ccs = sys?.controlChannels ?? [];
  const voice: Chan[] = useMemo(() => {
    if (found?.voice.length) return found.voice.map((v) => ({ hz: v.freqHz, weight: Math.max(1, v.grants) }));
    return (sys?.voiceChannels ?? []).map((hz) => ({ hz, weight: 1 }));
  }, [found, sys]);
  const rates = c.sources.map((x) => x.rateHz);
  // A capture file hears what it recorded: it can't be moved.
  const fixed = c.sources.map((x) => (x.kind === "file" ? x.centerHz || null : null));
  const first = found && c.sources[found.source] ? found.source : 0;
  const recommended = useMemo(() => {
    const firstCenter = found?.sug?.centerHz || autoCenter(ccs, rates[first] ?? 2_400_000) || ccs[0] || 0;
    return planCenters(rates, fixed, first, firstCenter, ccs, voice);
  }, [c.sources.length, found]);
  const [centers, setCenters] = useState<(number | null)[]>(recommended);

  if (!sys || !ccs.length) return <Missing onBack={props.onBack} />;

  const reached = (hz: number) => centers.some((ctr, k) => ctr !== null && rates[k] !== null && covers(ctr, rates[k]!, hz));
  const ccOk = ccs.some(reached);
  const inVoice = voice.filter((v) => reached(v.hz));
  const allWeight = voice.reduce((a, v) => a + v.weight, 0);
  const callsPct = allWeight ? Math.round((100 * inVoice.reduce((a, v) => a + v.weight, 0)) / allWeight) : 0;
  const changed = centers.some((x, k) => x !== recommended[k]);
  const lo = Math.min(...ccs, ...voice.map((v) => v.hz));
  const hi = Math.max(...ccs, ...voice.map((v) => v.hz));
  const radios = c.sources.length;
  const groups = clusters(ccs, voice, centers, rates);
  const save = () => {
    updateConfig((x) => {
      centers.forEach((ctr, k) => {
        if (ctr !== null && x.sources[k]) x.sources[k].centerHz = ctr;
      });
    });
    bumpEpoch();
    props.onNext();
  };

  return (
    <section className="ob-step">
      <Head
        title="Where each radio listens"
        art={null}
        lead={
          radios > 1
            ? "Each radio hears a slice of the band. We've spread yours over the channels this system used most."
            : "Your radio hears a slice of the band. We've centred it to catch the channels this system used most."
        }
      />
      {groups.map((g, n) => (
        <div key={n} className="ob-plan-group">
          {groups.length > 1 && (
            <span className="ob-plan-range">
              {g.hi - g.lo < 100_000 ? formatMhz(g.lo, 3) : `${formatMhz(g.lo, 1)}–${formatMhz(g.hi, 1)}`} MHz
            </span>
          )}
          <PlanDiagram ccs={g.ccs} voice={g.voice} centers={g.centers} rates={rates} reached={reached} />
        </div>
      ))}
      {groups.length > 1 && <p className="ob-aside">This system uses channels in {groups.length} places far apart, shown separately.</p>}
      <div className="ob-legend" aria-hidden="true">
        <span>
          <i className="lg-cc" /> Control channel
        </span>
        <span>
          <i className="lg-in" /> Voice channel it hears
        </span>
        <span>
          <i className="lg-out" /> Out of reach
        </span>
        <span className="ob-quiet">Taller = busier</span>
      </div>
      <div className="ob-stats">
        {voice.length > 0 ? (
          <>
            <div className="ob-stat">
              <span className="ob-stat-num">
                {inVoice.length}
                <small> / {voice.length}</small>
              </span>
              <span className="ob-stat-label">voice channels in reach</span>
            </div>
            <div className="ob-stat">
              <span className="ob-stat-num">{callsPct}%</span>
              <span className="ob-stat-label">of the calls heard while scanning</span>
            </div>
          </>
        ) : (
          <p className="ob-aside">No calls were heard while listening, so the radio is centred on the control channel. Once recording, the waterfall shows where calls land.</p>
        )}
      </div>
      {!ccOk && <div className="ob-note bad">The control channel must stay inside a radio&apos;s slice — without it nothing records.</div>}
      {ccOk && inVoice.length < voice.length && (
        <p className="ob-aside">
          This system spreads over {((hi - lo) / 1e6).toFixed(1)} MHz, wider than {radios > 1 ? "your radios reach" : "one radio hears"}. Calls out of reach are skipped
          {radios > 1 ? "." : " — a second radio, added later under Sources, catches them."}
        </p>
      )}
      <div className="ob-tuners">
        {centers.map((ctr, k) =>
          ctr === null || rates[k] === null ? null : fixed[k] !== null ? (
            <div key={k} className="ob-tuner" style={{ ["--radio" as string]: `var(--sys-${k % 6})` }}>
              <span className="ob-tuner-name">
                <i />
                Radio {k + 1} <span className="ob-quiet">· capture file</span>
              </span>
              <span className="ob-quiet">recorded here; can&apos;t move</span>
              <span className="ob-mono ob-tuner-hz">{formatMhz(ctr, 3)} MHz</span>
            </div>
          ) : (
            <div key={k} className="ob-tuner" style={{ ["--radio" as string]: `var(--sys-${k % 6})` }}>
              <span className="ob-tuner-name">
                <i />
                Radio {k + 1} <span className="ob-quiet">· {(rates[k]! / 1e6).toFixed(1)} MHz wide</span>
              </span>
              <input
                type="range"
                aria-label={`Radio ${k + 1} center`}
                min={lo - usableHalfWidth(rates[k]!)}
                max={hi + usableHalfWidth(rates[k]!)}
                step={12_500}
                value={ctr}
                onChange={(e) => setCenters(centers.map((x, j) => (j === k ? Number(e.target.value) : x)))}
              />
              <span className="ob-mono ob-tuner-hz">{formatMhz(ctr, 3)} MHz</span>
            </div>
          ),
        )}
        {changed && (
          <button className="ob-link" onClick={() => setCenters(recommended)}>
            Back to the suggestion
          </button>
        )}
      </div>
      <Nav onBack={props.onBack} onNext={save} disabled={!ccOk} why="Put the control channel in reach" />
    </section>
  );
}

/**
 * Channels and radio slices in groups far apart from each other (a system on
 * 700 and 800 MHz): each is drawn on its own, so neither shrinks to a sliver.
 */
function clusters(ccs: number[], voice: Chan[], centers: (number | null)[], rates: (number | null)[]) {
  type Item = { lo: number; hi: number; cc?: number; v?: Chan; k?: number };
  const items: Item[] = [
    ...ccs.map((f) => ({ lo: f, hi: f, cc: f })),
    ...voice.map((v) => ({ lo: v.hz, hi: v.hz, v })),
    ...centers.flatMap((c, k) => (c === null || rates[k] === null ? [] : [{ lo: c - rates[k]! / 2, hi: c + rates[k]! / 2, k }])),
  ].sort((a, b) => a.lo - b.lo);
  const out: { lo: number; hi: number; ccs: number[]; voice: Chan[]; centers: (number | null)[] }[] = [];
  for (const it of items) {
    let g = out.at(-1);
    if (!g || it.lo - g.hi > 3e6) out.push((g = { lo: it.lo, hi: it.hi, ccs: [], voice: [], centers: centers.map(() => null) }));
    g.hi = Math.max(g.hi, it.hi);
    if (it.cc !== undefined) g.ccs.push(it.cc);
    if (it.v) g.voice.push(it.v);
    if (it.k !== undefined) g.centers[it.k] = centers[it.k];
  }
  return out;
}

/** The band: each radio's slice, the control channel, and the voice channels by how busy they were. */
function PlanDiagram(props: { ccs: number[]; voice: Chan[]; centers: (number | null)[]; rates: (number | null)[]; reached: (hz: number) => boolean }) {
  const W = 760;
  const H = 230;
  const base = 172;
  const slices = props.centers.map((c, k) => (c === null || props.rates[k] === null ? null : { k, lo: c - props.rates[k]! / 2, hi: c + props.rates[k]! / 2, c }));
  const xs = [...props.ccs, ...props.voice.map((v) => v.hz), ...slices.flatMap((s) => (s ? [s.lo, s.hi] : []))];
  const span = Math.max(...xs) - Math.min(...xs) || 1e6;
  const lo = Math.min(...xs) - span * 0.04;
  const hi = Math.max(...xs) + span * 0.04;
  const x = (hz: number) => 20 + ((hz - lo) / (hi - lo)) * (W - 40);
  const maxW = Math.max(1, ...props.voice.map((v) => v.weight));
  // About five round-number ticks.
  const step = [0.1, 0.2, 0.25, 0.5, 1, 2, 5].map((m) => m * 1e6).find((st) => (hi - lo) / st <= 6) ?? 10e6;
  const ticks: number[] = [];
  for (let t = Math.ceil(lo / step) * step; t <= hi; t += step) ticks.push(t);

  return (
    <svg className="ob-plan" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Where each radio listens across the band">
      {slices.map((sl) =>
        sl ? (
          <g key={sl.k} style={{ ["--radio" as string]: `var(--sys-${sl.k % 6})` }}>
            <rect className="pl-slice" x={x(sl.lo)} y={26} width={x(sl.hi) - x(sl.lo)} height={base - 26} rx={10} />
            <line className="pl-center" x1={x(sl.c)} x2={x(sl.c)} y1={30} y2={base} />
            <text className="pl-radio" x={x(sl.lo) + 10} y={46}>
              Radio {sl.k + 1}
            </text>
          </g>
        ) : null,
      )}
      {props.voice.map((v) => {
        const h = 22 + (88 * v.weight) / maxW;
        const on = props.reached(v.hz);
        return (
          <g key={v.hz} className={on ? "pl-in" : "pl-out"}>
            <line x1={x(v.hz)} x2={x(v.hz)} y1={base} y2={base - h} />
            <circle cx={x(v.hz)} cy={base - h} r={5} />
          </g>
        );
      })}
      {props.ccs.map((f) => (
        <g key={f} className="pl-cc">
          <line x1={x(f)} x2={x(f)} y1={base} y2={58} />
          <path d={`M${x(f)} 50 l9 9 l-9 9 l-9 -9 z`} />
        </g>
      ))}
      <line className="pl-axis" x1={10} x2={W - 10} y1={base} y2={base} />
      {ticks.map((t) => (
        <g key={t} className="pl-tick">
          <line x1={x(t)} x2={x(t)} y1={base} y2={base + 6} />
          <text x={x(t)} y={base + 26} textAnchor="middle">
            {(t / 1e6).toFixed(step < 1e6 ? 2 : 0)}
          </text>
        </g>
      ))}
      <text className="pl-unit" x={W - 12} y={base + 50} textAnchor="end">
        MHz
      </text>
    </svg>
  );
}

// ── name ─────────────────────────────────────────────────────────────────────

/** "Metro County P25" → "metrocountyp25". */
const slug = (name: string) => name.toLowerCase().replace(/[^a-z0-9]+/g, "").slice(0, 20);

function NameStep(props: { c: Config; sys: System; index: number; onNext: () => void; onBack: () => void }) {
  const { c, sys, index } = props;
  const [touched, setTouched] = useState(false);
  const edit = (fn: (x: System) => void) => updateConfig((x) => fn(x.systems[index]));
  const dup = c.systems.some((x, k) => k !== index && x.shortName === sys.shortName);
  const now = new Date();
  const folder = c.recording.captureDir.split(/[\\/]/).filter(Boolean).pop() || "Recordings";
  return (
    <section className="ob-step">
      <Head art={<ArtTag />} title="Name your system" lead="Give it a name you'll recognise, and a short name for its folder." />
      <div className="ob-form">
        <label className="ob-field">
          <span className="ob-label">System name</span>
          <input
            className="ob-input"
            value={sys.name ?? ""}
            placeholder="Metro County Public Safety"
            autoFocus
            onChange={(e) => {
              const name = e.target.value;
              edit((x) => {
                x.name = name;
                if (!touched && slug(name)) x.shortName = uniqueShortName(c, slug(name), c.systems[index]);
              });
            }}
          />
        </label>
        <label className="ob-field">
          <span className="ob-label">Short name</span>
          <input
            className="ob-input ob-mono"
            value={sys.shortName}
            onChange={(e) => {
              setTouched(true);
              edit((x) => void (x.shortName = e.target.value.replace(/[^\w.-]/g, "")));
            }}
          />
          <span className={`ob-hint${dup || !sys.shortName ? " bad" : ""}`}>
            {!sys.shortName ? "It needs a short name." : dup ? "Another system has this name." : "Letters, numbers and dashes. Plugins and uploads use it too."}
          </span>
        </label>
      </div>
      <div className="ob-tree" aria-label="Where calls are saved">
        <div>
          <IconFolder /> {folder}
        </div>
        <div className="d1 hl">
          <IconFolder /> <b>{sys.shortName || "…"}</b>
        </div>
        <div className="d2">
          <IconFolder /> {now.getFullYear()} / {now.getMonth() + 1} / {now.getDate()}
        </div>
        <div className="d3">
          <IconWave /> <span className="ob-mono">1201-{Math.floor(now.getTime() / 1000)}_{Math.round(sys.controlChannels[0] ?? 851012500)}.wav</span>
        </div>
      </div>
      <Nav onBack={props.onBack} onNext={props.onNext} disabled={!sys.shortName || dup} why="Pick a short name" />
    </section>
  );
}

// ── storage ──────────────────────────────────────────────────────────────────

function StorageStep(props: { s: AppState; c: Config; defaultDir: string; onNext: () => void; onBack: () => void }) {
  const { c } = props;
  const dir = c.recording.captureDir;
  const [choosing, setChoosing] = useState(false);
  const isDefault = dir === props.defaultDir;
  const set = (path: string) => updateConfig((x) => void (x.recording.captureDir = path));
  return (
    <section className="ob-step">
      <Head art={<ArtFolder />} title="Where should calls be saved?" lead="Each call is saved as a sound file, with its details beside it." />
      <div className="ob-cards two" role="radiogroup">
        <button className={`ob-card pick${isDefault && !choosing ? " on" : ""}`} role="radio" aria-checked={isDefault && !choosing} onClick={() => (set(props.defaultDir), setChoosing(false))}>
          <span className="ob-check" aria-hidden="true">
            {isDefault && !choosing ? "✓" : ""}
          </span>
          <span className="ob-card-art">
            <IconFolder />
          </span>
          <span className="ob-card-title">The usual place</span>
          <span className="ob-card-sub ob-mono">{props.defaultDir}</span>
        </button>
        <button className={`ob-card pick${!isDefault || choosing ? " on" : ""}`} role="radio" aria-checked={!isDefault || choosing} onClick={() => setChoosing(true)}>
          <span className="ob-check" aria-hidden="true">
            {!isDefault || choosing ? "✓" : ""}
          </span>
          <span className="ob-card-art">
            <IconDrive />
          </span>
          <span className="ob-card-title">Somewhere else</span>
          <span className="ob-card-sub ob-mono">{isDefault ? "A bigger drive, a shared folder…" : dir}</span>
        </button>
      </div>
      {choosing && (
        <FolderPicker
          start={dir}
          onPick={(p) => {
            set(p);
            setChoosing(false);
          }}
        />
      )}
      <p className="ob-aside">
        Your settings are kept in <span className="ob-mono">{props.s.configPath}</span>.
      </p>
      <Nav onBack={props.onBack} onNext={props.onNext} disabled={!dir.trim() || choosing} why={choosing ? "Choose a folder" : "Pick a folder"} />
    </section>
  );
}

/** The folder shown; type or paste another and press Enter to go there. */
function PathBox(props: { path: string }) {
  const [text, setText] = useState(props.path);
  useEffect(() => setText(props.path), [props.path]);
  return (
    <input
      className="ob-mono ob-picker-path"
      value={text}
      aria-label="Folder"
      spellCheck={false}
      onChange={(e) => setText(e.target.value)}
      onKeyDown={(e) => e.key === "Enter" && listDir(text.trim())}
      onBlur={() => setText(props.path)}
    />
  );
}

/** Browse the recorder's computer for a folder. */
function FolderPicker(props: { start: string; onPick: (path: string) => void }) {
  const d = useApp().dir;
  const [made, setMade] = useState("");
  useEffect(() => listDir(props.start), []);
  if (!d) return <p className="ob-quiet">Opening…</p>;
  const join = (name: string) => (d.path.endsWith(d.sep) ? d.path + name : d.path + d.sep + name);
  return (
    <div className="ob-picker">
      <div className="ob-picker-bar">
        <button className="ob-btn small" disabled={!d.parent} onClick={() => d.parent && listDir(d.parent)} aria-label="Up one folder">
          ↑ Up
        </button>
        <button className="ob-btn small" onClick={() => listDir(d.home)}>
          Home
        </button>
        <PathBox path={d.path} />
      </div>
      {d.error && <div className="ob-note bad">{d.error}</div>}
      <ul className="ob-picker-list">
        {d.dirs.length === 0 && <li className="ob-quiet">No folders inside</li>}
        {d.dirs.map((name) => (
          <li key={name}>
            <button onClick={() => listDir(join(name))}>
              <IconFolder /> {name}
            </button>
          </li>
        ))}
      </ul>
      <div className="ob-picker-foot">
        <input className="ob-input" value={made} placeholder="New folder name (optional)" onChange={(e) => setMade(e.target.value.replace(/[\\/:*?"<>|]/g, ""))} />
        <button className="ob-btn primary" onClick={() => props.onPick(made.trim() ? join(made.trim()) : d.path)}>
          {made.trim() ? `Use new folder “${made.trim()}”` : "Use this folder"}
        </button>
      </div>
    </div>
  );
}

// ── talkgroups ───────────────────────────────────────────────────────────────

function TalkgroupsStep(props: { sys: System; index: number; onNext: () => void; onBack: () => void }) {
  const { sys, index } = props;
  const [how, setHow] = useState<"later" | "paste" | "file">(sys.talkgroupsCsv ? "file" : "later");
  const [paste, setPaste] = useState("");
  const parsed = useMemo(() => parseRadioReferencePaste(paste), [paste]);
  const fileRef = useRef<HTMLInputElement>(null);
  const loaded = sys.talkgroupsCsv ? parseTalkgroupCsv(sys.talkgroupsCsv).size : 0;
  const setList = (csv: string, name: string) =>
    updateConfig((x) => {
      x.systems[index].talkgroupsCsv = csv;
      x.systems[index].talkgroupsName = name;
    });
  const onFile = async (f: File | undefined) => {
    if (!f) return;
    setList(await f.text(), f.name);
    props.onNext();
  };
  const id = sys.expect;
  return (
    <section className="ob-step">
      <Head
        art={<ArtTag talkgroup />}
        title="Name the talkgroups"
        lead="Radios talk in groups, each with a number. A talkgroup list turns the numbers into names. It's optional — everything is recorded either way."
      />
      <div className="ob-cards three" role="radiogroup">
        <OptionCard on={how === "later"} onPick={() => setHow("later")} art={<IconClock />} title="Not now" sub="Calls show numbers; add names later" />
        <OptionCard on={how === "paste"} onPick={() => setHow("paste")} art={<IconCopy />} title="Copy from RadioReference" sub="Free to view, no subscription" />
        <OptionCard on={how === "file"} onPick={() => setHow("file")} art={<IconFile />} title="I have a file" sub="A Trunk Recorder talkgroup CSV" />
      </div>

      {loaded > 0 && (
        <div className="ob-note ok">
          <span>
            <b>{loaded} talkgroup names loaded</b> from {sys.talkgroupsName || "your list"}.
          </span>
          <button className="ob-link" onClick={() => setList("", "")}>
            Remove
          </button>
        </div>
      )}

      {how === "paste" && (
        <ol className="ob-howto">
          <li>
            <span className="ob-num">1</span>
            <div>
              <b>Find your system</b>
              <p>
                Open{" "}
                <a href="https://www.radioreference.com/db/browse/" target="_blank" rel="noreferrer">
                  RadioReference&apos;s database
                </a>
                , pick your state and county, then your trunked system.
                {id.sysId != null && (
                  <>
                    {" "}
                    Look for <b className="ob-mono">System ID {hex(id.sysId)}</b>
                    {id.wacn != null && (
                      <>
                        {" "}
                        and <b className="ob-mono">WACN {hex(id.wacn)}</b>
                      </>
                    )}
                    .
                  </>
                )}
              </p>
            </div>
          </li>
          <li>
            <span className="ob-num">2</span>
            <div>
              <b>Copy the talkgroup table</b>
              <p>Drag from the first talkgroup number to the last one, then copy. Headings and all is fine.</p>
            </div>
          </li>
          <li>
            <span className="ob-num">3</span>
            <div className="ob-grow">
              <b>Paste it here</b>
              <textarea className="ob-input ob-paste" value={paste} onChange={(e) => setPaste(e.target.value)} placeholder={"DEC\tHEX\tMode\tAlpha Tag\tDescription\tTag\n1201\t4b1\tD\tFire Disp\tFire Dispatch\tFire Dispatch"} />
              {paste.trim() &&
                (parsed.length ? (
                  <div className="ob-preview">
                    <table>
                      <tbody>
                        {parsed.slice(0, 5).map((t: Talkgroup) => (
                          <tr key={t.number}>
                            <td className="ob-mono">{t.number}</td>
                            <td>
                              <b>{t.alphaTag}</b>
                            </td>
                            <td className="ob-quiet">{t.description}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                    <div className="ob-row">
                      <span>
                        {parsed.length > 5 ? `…and ${parsed.length - 5} more` : `${parsed.length} talkgroup${parsed.length === 1 ? "" : "s"}`}
                      </span>
                      <button
                        className="ob-btn primary"
                        onClick={() => {
                          setList(talkgroupsToCsv(parsed), "RadioReference (pasted)");
                          props.onNext();
                        }}
                      >
                        Use these {parsed.length}
                      </button>
                    </div>
                  </div>
                ) : (
                  <div className="ob-note bad">That doesn&apos;t look like a talkgroup table — each row should start with a talkgroup number.</div>
                ))}
            </div>
          </li>
        </ol>
      )}

      {how === "file" && (
        <div className="ob-center-actions">
          <button className="ob-btn" onClick={() => fileRef.current?.click()}>
            Choose a CSV file…
          </button>
          <input ref={fileRef} type="file" accept=".csv,text/csv" hidden onChange={(e) => void onFile(e.target.files?.[0])} />
        </div>
      )}
      <Nav onBack={props.onBack} onNext={props.onNext} nextLabel={how === "later" && !loaded ? "Skip for now" : "Continue"} />
    </section>
  );
}

function OptionCard(props: { on: boolean; onPick: () => void; art: React.ReactNode; title: string; sub: string }) {
  return (
    <button className={`ob-card pick${props.on ? " on" : ""}`} role="radio" aria-checked={props.on} onClick={props.onPick}>
      <span className="ob-check" aria-hidden="true">
        {props.on ? "✓" : ""}
      </span>
      <span className="ob-card-art">{props.art}</span>
      <span className="ob-card-title">{props.title}</span>
      <span className="ob-card-sub">{props.sub}</span>
    </button>
  );
}

// ── done ─────────────────────────────────────────────────────────────────────

function DoneStep(props: { s: AppState; c: Config; sys: System | undefined; onStart: () => void; onClose: () => void }) {
  const { c, sys } = props;
  const problem = startProblem(c);
  const radios = c.sources.length;
  const tgs = sys?.talkgroupsCsv ? parseTalkgroupCsv(sys.talkgroupsCsv).size : 0;
  return (
    <section className="ob-step ob-welcome">
      <div className="ob-hero">
        <ArtDone />
      </div>
      <h1 className="ob-title big">You&apos;re all set</h1>
      <ul className="ob-summary">
        <li>
          <IconDongle />
          <span>
            {radios} radio{radios === 1 ? "" : "s"}
          </span>
        </li>
        {sys && (
          <li>
            <IconTower />
            <span>
              {sys.name || sys.shortName}
              <span className="ob-quiet ob-mono"> · {sys.controlChannels.map((f) => formatMhz(f, 4)).join(", ")} MHz</span>
            </span>
          </li>
        )}
        {!web && (
          <li>
            <IconFolder />
            <span className="ob-mono">{c.recording.captureDir}</span>
          </li>
        )}
        <li>
          <IconTagSmall />
          <span>{tgs ? `${tgs} talkgroup names` : "No talkgroup names yet"}</span>
        </li>
      </ul>
      {problem && <div className="ob-note bad">{problem}</div>}
      <div className="ob-center-actions">
        <button className="ob-btn primary big" disabled={!!problem || !props.s.connected} onClick={props.onStart}>
          Start recording
        </button>
        <button className="ob-link" onClick={props.onClose}>
          Open the app without starting
        </button>
      </div>
      <p className="ob-aside">Add more systems any time with “Find my system” on the main screen.</p>
    </section>
  );
}

// ── bringing over a Trunk Recorder config ────────────────────────────────────

interface Loaded {
  text: string;
  /** Where it came from (a path on the recorder's computer, or an uploaded file's name). */
  name: string;
  /** The talkgroup and channel files it names, by the name it gives. */
  files: Record<string, string>;
}

function ImportFlow(props: { s: AppState; c: Config; onBack?: () => void; onStart: () => void; onClose: () => void }) {
  const { s, c } = props;
  const [step, setStep] = useState<"pick" | "review" | "done">("pick");
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  // The recorder read one (desktop): review it.
  useEffect(() => {
    const t = s.trConfig;
    if (step === "pick" && t?.text && !t.error) {
      setLoaded({ text: t.text, name: t.path, files: t.files ?? {} });
      setStep("review");
    }
  }, [s.trConfig]);

  return (
    <>
      <ol className="ob-progress solo" aria-label="Steps">
        {["Choose your config", "Review", "Finish"].map((label, i) => {
          const at = ["pick", "review", "done"].indexOf(step);
          return (
            <li key={label} className={i < at ? "done" : i === at ? "on" : ""} aria-current={i === at ? "step" : undefined}>
              <span className="ob-dot" aria-hidden="true">
                {i < at ? "✓" : i + 1}
              </span>
              <span className="ob-step-label">{label}</span>
            </li>
          );
        })}
      </ol>
      {step === "pick" ? (
        <ImportPick s={s} onLoaded={(l) => (setLoaded(l), setStep("review"))} onBack={props.onBack} />
      ) : step === "review" && loaded ? (
        <ImportReview s={s} c={c} loaded={loaded} onBack={() => setStep("pick")} onDone={() => setStep("done")} />
      ) : (
        <ImportDone s={s} c={c} onStart={props.onStart} onClose={props.onClose} />
      )}
    </>
  );
}

function ImportPick(props: { s: AppState; onLoaded: (l: Loaded) => void; onBack?: () => void }) {
  const { s } = props;
  const fileRef = useRef<HTMLInputElement>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const upload = async (f: File | undefined) => {
    if (!f) return;
    const text = await f.text();
    try {
      JSON.parse(text);
      props.onLoaded({ text, name: f.name, files: {} });
    } catch (e) {
      setProblem(`${f.name} isn't a Trunk Recorder config: ${e instanceof Error ? e.message : String(e)}`);
    }
  };
  const error = problem ?? s.trConfig?.error ?? null;
  return (
    <section className="ob-step">
      <Head
        art={<ArtImport />}
        title="Bring over your Trunk Recorder setup"
        lead={
          web
            ? "Choose your Trunk Recorder config.json. Its talkgroup files can be added on the next step."
            : "Choose the folder Trunk Recorder runs from. Its config.json comes over with the talkgroup and channel files it uses."
        }
      />
      {error && <div className="ob-note bad">{error}</div>}
      {!web && <ConfigPicker />}
      <div className="ob-center-actions">
        {web ? (
          <button className="ob-btn primary big" onClick={() => fileRef.current?.click()}>
            Choose config.json…
          </button>
        ) : (
          <button className="ob-link" onClick={() => fileRef.current?.click()}>
            Or upload a config.json from this browser&apos;s computer
          </button>
        )}
        <input ref={fileRef} type="file" accept=".json,application/json" hidden onChange={(e) => void upload(e.target.files?.[0])} />
      </div>
      <Nav onBack={props.onBack} />
    </section>
  );
}

/** Browse the recorder's computer for a Trunk Recorder config: folders, and the .json files in each. */
function ConfigPicker() {
  const d = useApp().dir;
  useEffect(() => listDir(""), []);
  if (!d) return <p className="ob-quiet">Opening…</p>;
  const join = (name: string) => (d.path.endsWith(d.sep) ? d.path + name : d.path + d.sep + name);
  const files = d.files ?? [];
  const main = files.find((f) => f.toLowerCase() === "config.json");
  return (
    <div className="ob-picker">
      <div className="ob-picker-bar">
        <button className="ob-btn small" disabled={!d.parent} onClick={() => d.parent && listDir(d.parent)} aria-label="Up one folder">
          ↑ Up
        </button>
        <button className="ob-btn small" onClick={() => listDir(d.home)}>
          Home
        </button>
        <PathBox path={d.path} />
      </div>
      {main && (
        <div className="ob-picker-found">
          <IconFile />
          <span>
            <b>config.json</b> is in this folder
          </span>
          <button className="ob-btn primary" onClick={() => readTrConfig(join(main))}>
            Use it
          </button>
        </div>
      )}
      <ul className="ob-picker-list">
        {d.dirs.length + files.length === 0 && <li className="ob-quiet">Nothing here</li>}
        {d.dirs.map((name) => (
          <li key={`d/${name}`}>
            <button onClick={() => listDir(join(name))}>
              <IconFolder /> {name}
            </button>
          </li>
        ))}
        {files
          .filter((f) => f !== main)
          .map((name) => (
            <li key={`f/${name}`}>
              <button className="file" onClick={() => readTrConfig(join(name))}>
                <IconFile /> {name}
              </button>
            </li>
          ))}
      </ul>
    </div>
  );
}

/** How an imported source will fare here. */
function sourceState(src: Source, s: AppState): { tone: "ok" | "warn" | "bad"; text: string } {
  if (src.kind === "rtlsdr") {
    if (!src.serial) return s.devices.some((d) => !d.busy) ? { tone: "ok", text: "The first free dongle" } : { tone: "warn", text: "No free dongle plugged in" };
    const d = s.devices.find((x) => x.serial === src.serial);
    return !d ? { tone: "warn", text: "Not plugged in" } : d.busy ? { tone: "warn", text: d.busy.replace(/^./, (ch) => ch.toUpperCase()) } : { tone: "ok", text: "Plugged in and free" };
  }
  if (src.kind === "file") return { tone: "ok", text: "Capture file" };
  if (!s.radios) return { tone: "bad", text: "Needs the desktop app" };
  const d = s.radios[src.kind];
  return d?.available ? { tone: "ok", text: "Driver installed" } : { tone: "bad", text: `Needs ${src.kind === "usrp" ? "UHD" : src.kind === "airspy" ? "libairspy" : "SoapySDR"} installed` };
}

const sourceLabel = (src: Source) =>
  src.kind === "rtlsdr"
    ? `RTL-SDR${src.serial ? ` · serial ${src.serial}` : ""}`
    : src.kind === "airspy"
      ? `Airspy${src.serial ? ` · ${src.serial}` : ""}`
      : src.kind === "usrp"
        ? `USRP${src.args ? ` · ${src.args}` : ""}`
        : src.kind === "soapy"
          ? `SoapySDR${src.args ? ` · ${src.args}` : ""}`
          : `Capture · ${src.path.split(/[\\/]/).pop()}`;

function ImportReview(props: { s: AppState; c: Config; loaded: Loaded; onBack: () => void; onDone: () => void }) {
  const { s, c, loaded } = props;
  const [files, setFiles] = useState(loaded.files);
  // Dongles swapped in for ones that aren't free: source index → serial.
  const [swap, setSwap] = useState<Record<number, string>>({});
  const result = useMemo(() => {
    try {
      return importTrunkRecorderConfig(loaded.text, c, files);
    } catch (e) {
      return { error: e instanceof Error ? e.message : String(e) };
    }
  }, [loaded, files]);
  if ("error" in result) {
    return (
      <section className="ob-step">
        <Head art={<ArtSearch />} title="That config couldn't be read" lead={result.error} />
        <Nav onBack={props.onBack} />
      </section>
    );
  }
  const cfg: Config = { ...result.config, sources: result.config.sources.map((x, i) => (x.kind === "rtlsdr" && swap[i] !== undefined ? { ...x, serial: swap[i] } : x)) };
  const named = trConfigFiles(loaded.text);
  const missing = (file: string) => !(file in files);
  const later = result.todo.filter((t) => t.kind === "siteLock" || t.kind === "plugins" || t.kind === "squelch");
  const replaces = c.systems.length > 0 || enabledChannels(c).length > 0;
  const used = (i: number) => new Set(cfg.sources.filter((x, k) => k !== i && x.kind === "rtlsdr").map((x) => (x.kind === "rtlsdr" ? x.serial : "")));
  const addFile = async (name: string, f: File | undefined) => {
    if (f) setFiles({ ...files, [name]: await f.text() });
  };
  const convCount = cfg.conventional.reduce((n, v) => n + v.channels.length, 0);
  // Systems whose control channel no radio hears (as the radios stand, swaps included).
  const centers = resolvedCenters(cfg);
  const unheard = (x: System) => x.enabled && x.controlChannels.length > 0 && !x.controlChannels.some((f) => sourceCovering(cfg, centers, f) >= 0);
  const installed = (id: string) => {
    const p = s.plugins?.plugins.find((x) => x.id === id);
    return !!p && !p.problem;
  };

  const apply = () => {
    updateConfig((x) => {
      const plugins = { ...x.plugins };
      Object.assign(x, cfg);
      if (web) return;
      // Uploaders and streamers: their settings, kept beside any already there; those installed are turned on.
      for (const p of result.plugins) {
        const had = plugins[p.id] ?? { enabled: false };
        plugins[p.id] = { ...had, enabled: had.enabled || installed(p.id), ...(Object.keys(p.config).length ? { settings: { ...had.settings, ...p.config } } : {}) };
        for (const [name, values] of Object.entries(p.systems)) {
          const sys = x.systems.find((y) => y.shortName === name) ?? x.conventional.find((y) => y.shortName === name);
          if (sys) sys.plugins = { ...sys.plugins, [p.id]: { ...sys.plugins?.[p.id], ...values } };
        }
      }
      x.plugins = plugins;
    });
    bumpEpoch();
    // Radios that aren't ready yet join what's left to do.
    const radios: ImportTodo[] = cfg.sources.flatMap((src, index): ImportTodo[] => {
      if (src.kind === "rtlsdr" && src.serial && sourceState(src, s).tone !== "ok") return [{ kind: "source", index, serial: src.serial }];
      if ((src.kind === "usrp" || src.kind === "airspy" || src.kind === "soapy") && sourceState(src, s).tone === "bad") return [{ kind: "driver", index, driver: src.kind }];
      return [];
    });
    const plugins: ImportTodo[] = web ? [] : result.plugins.map((p) => ({ kind: "plugin", id: p.id, name: s.plugins?.plugins.find((x) => x.id === p.id)?.manifest?.name ?? p.name }));
    const coverage: ImportTodo[] = cfg.systems.filter(unheard).map((x) => ({ kind: "coverage", system: x.shortName }));
    setTodo([...result.todo, ...radios, ...coverage, ...plugins]);
    props.onDone();
  };

  return (
    <section className="ob-step">
      <Head art={null} title="Here's what comes over" lead={<span className="ob-mono ob-path">{loaded.name}</span>} />

      <div className="ob-review">
        <h3 className="ob-sub">
          <IconDongle /> Radios
        </h3>
        <ul className="ob-rows">
          {cfg.sources.map((src, i) => {
            const st = sourceState(src, s);
            const free = s.devices.filter((d) => !d.busy && !used(i).has(d.serial));
            const orig = result.config.sources[i];
            return (
              <li key={i} className={st.tone}>
                <span className="ob-row-main">
                  <b>{sourceLabel(src)}</b>
                  <span className="ob-quiet">
                    {src.centerHz ? `centred on ${formatMhz(src.centerHz, 3)} MHz` : "placed automatically"} · {(src.rateHz / 1e6).toFixed(1)} MHz wide
                  </span>
                </span>
                <span className={`ob-pill ${st.tone}`}>{st.text}</span>
                {src.kind === "rtlsdr" && orig.kind === "rtlsdr" && (st.tone !== "ok" || swap[i] !== undefined) && free.length > 0 && (
                  <select
                    className="ob-select"
                    aria-label={`Use another dongle for radio ${i + 1}`}
                    value={swap[i] ?? ""}
                    onChange={(e) => {
                      const n = { ...swap };
                      if (e.target.value === "") delete n[i];
                      else n[i] = e.target.value;
                      setSwap(n);
                    }}
                  >
                    <option value="">{orig.serial ? `Keep serial ${orig.serial}` : "Keep: first free"}</option>
                    {free.map((d) => (
                      <option key={d.serial} value={d.serial}>
                        Use serial {d.serial}
                      </option>
                    ))}
                  </select>
                )}
              </li>
            );
          })}
        </ul>

        <h3 className="ob-sub">
          <IconTower /> Systems
        </h3>
        <ul className="ob-rows">
          {cfg.systems.map((x) => {
            const file = named.find((n) => n.kind === "talkgroups" && n.system === x.shortName)?.file;
            const tgs = x.talkgroupsCsv ? parseTalkgroupCsv(x.talkgroupsCsv).size : 0;
            return (
              <li key={x.shortName} className={(file && missing(file)) || unheard(x) ? "warn" : "ok"}>
                <span className="ob-row-main">
                  <b>{x.shortName}</b>
                  <span className="ob-quiet">
                    {x.type === "smartnet" ? "SmartNet" : x.type === "dmr" ? "DMR" : "P25"} · {x.controlChannels.map((f) => formatMhz(f, 4)).join(", ") || "no control channel"}
                  </span>
                </span>
                {unheard(x) && <span className="ob-pill warn">No radio hears its control channel</span>}
                {file && missing(file) ? (
                  <FilePick label={`“${file}” not found`} onFile={(f) => void addFile(file, f)} />
                ) : (
                  <span className="ob-pill ok">{tgs ? `${tgs} talkgroups` : "No talkgroup file"}</span>
                )}
              </li>
            );
          })}
          {named
            .filter((n) => n.kind === "units" && missing(n.file))
            .map((n) => (
              <li key={n.file} className="warn">
                <span className="ob-row-main">
                  <b>Unit names for {n.system}</b>
                  <span className="ob-quiet">{n.file}</span>
                </span>
                <FilePick label="Not found" onFile={(f) => void addFile(n.file, f)} />
              </li>
            ))}
          {named
            .filter((n) => n.kind === "channels")
            .map((n) => (
              <li key={n.file} className={missing(n.file) ? "warn" : "ok"}>
                <span className="ob-row-main">
                  <b>Conventional channels</b>
                  <span className="ob-quiet">{n.file}</span>
                </span>
                {missing(n.file) ? <FilePick label="Not found" onFile={(f) => void addFile(n.file, f)} /> : <span className="ob-pill ok">{convCount} channels</span>}
              </li>
            ))}
          {convCount > 0 && !named.some((n) => n.kind === "channels") && (
            <li className="ok">
              <span className="ob-row-main">
                <b>Conventional channels</b>
              </span>
              <span className="ob-pill ok">{convCount} channels</span>
            </li>
          )}
        </ul>

        {!web && (
          <>
            <h3 className="ob-sub">
              <IconFolder /> Recordings
            </h3>
            <ul className="ob-rows">
              <li className="ok">
                <span className="ob-row-main ob-mono">{cfg.recording.captureDir}</span>
              </li>
            </ul>
          </>
        )}

        {result.plugins.length > 0 && (
          <>
            <h3 className="ob-sub">
              <IconUpload /> Uploads and streaming
            </h3>
            <ul className="ob-rows">
              {result.plugins.map((p) => {
                const systems = Object.keys(p.systems);
                const streams = Array.isArray(p.config.streams) ? p.config.streams.length : 0;
                const ok = !web && installed(p.id);
                return (
                  <li key={p.id} className={ok ? "ok" : "warn"}>
                    <span className="ob-row-main">
                      <b>{s.plugins?.plugins.find((x) => x.id === p.id)?.manifest?.name ?? p.name}</b>
                      <span className="ob-quiet">
                        {streams ? `${streams} stream${streams === 1 ? "" : "s"}` : systems.length ? `for ${systems.join(", ")}` : "settings for every system"}
                      </span>
                    </span>
                    <span className={`ob-pill ${ok ? "ok" : "warn"}`}>{web ? "Needs the desktop app" : ok ? "Will be turned on" : "Settings kept · add the plugin"}</span>
                  </li>
                );
              })}
            </ul>
          </>
        )}

        {later.length > 0 && (
          <>
            <h3 className="ob-sub">
              <IconClock /> To finish afterwards
            </h3>
            <ul className="ob-rows">
              {later.map((t, k) => (
                <li key={k} className="info">
                  <span className="ob-row-main">
                    <b>{t.kind === "siteLock" ? `Site lock for ${t.system}` : t.kind === "plugins" ? "Other plugins" : "Squelch"}</b>
                    <span className="ob-quiet">
                      {t.kind === "siteLock"
                        ? `Trunk Recorder followed only site ${t.siteId}.`
                        : t.kind === "plugins"
                          ? `${t.names.join(", ")} — nothing like ${t.names.length === 1 ? "it" : "them"} here yet.`
                          : "Works differently here; check it once recording."}
                    </span>
                  </span>
                </li>
              ))}
            </ul>
          </>
        )}
        {result.notes.length > 0 && (
          <details className="ob-more">
            <summary>Good to know ({result.notes.length})</summary>
            <ul className="ob-notes">
              {result.notes.map((n, k) => (
                <li key={k}>{n}</li>
              ))}
            </ul>
          </details>
        )}
      </div>
      {replaces && <div className="ob-note">This replaces the systems and radios set up here now.</div>}
      <Nav onBack={props.onBack} onNext={apply} nextLabel="Bring it over" />
    </section>
  );
}

function FilePick(props: { label: string; onFile: (f: File | undefined) => void }) {
  const ref = useRef<HTMLInputElement>(null);
  return (
    <span className="ob-filepick">
      <span className="ob-pill warn">{props.label}</span>
      <button className="ob-btn small" onClick={() => ref.current?.click()}>
        Choose file…
      </button>
      <input ref={ref} type="file" accept=".csv,text/csv,text/plain" hidden onChange={(e) => props.onFile(e.target.files?.[0])} />
    </span>
  );
}

function ImportDone(props: { s: AppState; c: Config; onStart: () => void; onClose: () => void }) {
  const todos = openTodos(props.s);
  const problem = startProblem(props.c);
  return (
    <section className="ob-step ob-welcome">
      <div className="ob-hero">
        <ArtDone />
      </div>
      <h1 className="ob-title big">{todos.length ? "Almost there" : "Your setup is here"}</h1>
      {todos.length > 0 && (
        <>
          <p className="ob-lead">
            {todos.length === 1 ? "One thing" : `${todos.length} things`} to finish. They&apos;re highlighted on the setup page.
          </p>
          <ul className="ob-summary">
            {todos.map((t, k) => (
              <li key={k}>
                <span className="ob-todo-dot" aria-hidden="true" />
                <span>
                  <b>{t.title}</b>
                  <span className="ob-check-sub">{t.text}</span>
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
      <div className="ob-center-actions">
        {todos.length ? (
          <>
            <button className="ob-btn primary big" onClick={props.onClose}>
              Open setup
            </button>
            {!problem && (
              <button className="ob-link" onClick={props.onStart}>
                Start recording anyway
              </button>
            )}
          </>
        ) : (
          <>
            <button className="ob-btn primary big" disabled={!!problem || !props.s.connected} onClick={props.onStart}>
              Start recording
            </button>
            <button className="ob-link" onClick={props.onClose}>
              Open the app without starting
            </button>
          </>
        )}
      </div>
      {problem && !todos.length && <div className="ob-note bad">{problem}</div>}
    </section>
  );
}

// ── pictures ─────────────────────────────────────────────────────────────────
// Line art in the theme's colours: .ln strokes in the text colour, .ac fills
// and .acs strokes in the accent, .sf a soft accent wash.

function ArtWelcome() {
  return (
    <svg className="art" viewBox="0 0 360 180" aria-hidden="true">
      <circle className="sf" cx="180" cy="92" r="84" />
      {/* tower */}
      <path className="ln" d="M70 160 L92 52 L114 160 M78 124 h28 M84 92 h16 M74 142 l36 -18 M80 110 l26 -14" />
      <circle className="ac" cx="92" cy="48" r="6" />
      <path className="acs wave w1" d="M108 34 a24 24 0 0 1 0 28" />
      <path className="acs wave w2" d="M120 24 a40 40 0 0 1 0 48" />
      <path className="acs wave w3" d="M132 14 a56 56 0 0 1 0 68" />
      {/* dongle */}
      <rect className="ln fillp" x="166" y="104" width="54" height="22" rx="6" />
      <path className="ln" d="M220 110 h12 v10 h-12 M178 104 L170 56" />
      <circle className="ac" cx="170" cy="54" r="4" />
      {/* laptop */}
      <rect className="ln fillp" x="244" y="70" width="88" height="58" rx="6" />
      <path className="ln" d="M232 136 h112 l-8 10 h-96 z" />
      <path className="acs" d="M254 100 l8 -10 l8 18 l8 -24 l8 26 l8 -16 l8 8 l10 0" />
    </svg>
  );
}

function ArtDongle() {
  return (
    <svg className="art" viewBox="0 0 200 140" aria-hidden="true">
      <circle className="sf" cx="100" cy="74" r="62" />
      <rect className="ln fillp" x="56" y="78" width="80" height="30" rx="8" />
      <path className="ln" d="M136 86 h18 v14 h-18 M70 78 L60 20" />
      <circle className="ac" cx="60" cy="18" r="5" />
      <circle className="ac" cx="118" cy="93" r="4" />
      <path className="acs wave w1" d="M76 14 a16 16 0 0 1 0 20" />
      <path className="acs wave w2" d="M86 6 a28 28 0 0 1 0 36" />
    </svg>
  );
}

function ArtSearch() {
  return (
    <svg className="art" viewBox="0 0 200 140" aria-hidden="true">
      <circle className="sf" cx="100" cy="74" r="62" />
      <rect className="ln fillp" x="44" y="80" width="70" height="26" rx="7" />
      <path className="ln" d="M114 87 h14 v12 h-14" />
      <circle className="ln fillp" cx="132" cy="56" r="24" />
      <path className="ln" d="M149 73 l20 20" />
      <path className="acs" d="M124 50 a9 9 0 1 1 10 9 v5 M134 70 v1" />
    </svg>
  );
}

function ArtScan(props: { still?: boolean }) {
  return (
    <svg className={`art${props.still ? "" : " scanning"}`} viewBox="0 0 200 140" aria-hidden="true">
      <circle className="sf" cx="100" cy="74" r="62" />
      <path className="ln" d="M100 128 L100 64 M86 128 h28" />
      <circle className="ac" cx="100" cy="60" r="6" />
      <path className="acs wave w1" d="M84 44 a22 22 0 0 0 0 32 M116 44 a22 22 0 0 1 0 32" />
      <path className="acs wave w2" d="M72 32 a40 40 0 0 0 0 56 M128 32 a40 40 0 0 1 0 56" />
      <path className="acs wave w3" d="M60 20 a58 58 0 0 0 0 80 M140 20 a58 58 0 0 1 0 80" />
    </svg>
  );
}

function ArtTower() {
  return (
    <svg className="art" viewBox="0 0 200 140" aria-hidden="true">
      <circle className="sf" cx="100" cy="74" r="62" />
      <path className="ln" d="M80 130 L100 34 L120 130 M86 104 h28 M92 76 h16 M84 120 l32 -16 M88 90 l24 -14" />
      <circle className="ac" cx="100" cy="30" r="6" />
      <path className="acs wave w1" d="M116 18 a20 20 0 0 1 0 24 M84 18 a20 20 0 0 0 0 24" />
      <path className="acs wave w2" d="M128 8 a36 36 0 0 1 0 44 M72 8 a36 36 0 0 0 0 44" />
    </svg>
  );
}

function ArtFolder() {
  return (
    <svg className="art" viewBox="0 0 200 140" aria-hidden="true">
      <circle className="sf" cx="100" cy="74" r="62" />
      <path className="ln fillp" d="M48 48 h36 l10 10 h58 v62 h-104 z" />
      <path className="acs" d="M68 96 l6 -10 l6 18 l6 -26 l6 30 l6 -18 l6 8 h20" />
    </svg>
  );
}

function ArtTag(props: { talkgroup?: boolean }) {
  return (
    <svg className="art" viewBox="0 0 260 120" aria-hidden="true">
      <circle className="sf" cx="130" cy="60" r="56" />
      <rect className="ln fillp" x="14" y="42" width="78" height="36" rx="18" />
      <text className="art-text mono" x="53" y="66" textAnchor="middle">
        1201
      </text>
      <path className="acs" d="M104 60 h36 m-10 -9 l10 9 l-10 9" />
      <rect className="ac-soft" x="152" y="42" width="96" height="36" rx="18" />
      <text className="art-text" x="200" y="66" textAnchor="middle">
        {props.talkgroup ? "Fire Disp" : "Metro P25"}
      </text>
    </svg>
  );
}

function ArtDone() {
  return (
    <svg className="art" viewBox="0 0 200 140" aria-hidden="true">
      <circle className="sf" cx="100" cy="70" r="62" />
      <circle className="ac" cx="100" cy="70" r="36" />
      <path className="tick" d="M84 70 l11 11 l22 -24" />
      <path className="acs" d="M40 30 v10 M35 35 h10 M160 100 v10 M155 105 h10 M158 26 v8 M154 30 h8" />
    </svg>
  );
}

function ArtImport() {
  return (
    <svg className="art" viewBox="0 0 260 140" aria-hidden="true">
      <circle className="sf" cx="130" cy="70" r="62" />
      <path className="ln fillp" d="M28 30 h44 l14 14 v66 h-58 z" />
      <path className="ln" d="M72 30 v14 h14" />
      <path className="acs" d="M40 64 h32 M40 76 h24 M40 88 h30" />
      <path className="acs" d="M104 70 h46 m-12 -11 l12 11 l-12 11" />
      <rect className="ln fillp" x="166" y="38" width="70" height="56" rx="8" />
      <path className="ln" d="M160 102 h82" />
      <path className="acs" d="M176 74 l7 -9 l7 15 l7 -20 l7 22 l7 -12 l7 4" />
    </svg>
  );
}

const icon = (d: React.ReactNode) => (
  <svg className="icon" viewBox="0 0 32 32" aria-hidden="true">
    {d}
  </svg>
);
export const IconDongle = () => icon(<><rect className="ln" x="4" y="14" width="18" height="10" rx="3" /><path className="ln" d="M22 17 h5 v4 h-5 M9 14 L7 4" /><circle className="ac" cx="7" cy="4" r="2" /></>);
const IconBox = () => icon(<><rect className="ln" x="4" y="12" width="24" height="14" rx="3" /><path className="ln" d="M10 12 V4 M22 12 V6" /><circle className="ac" cx="10" cy="19" r="2" /><circle className="ac" cx="16" cy="19" r="2" /></>);
export const IconTower = () => icon(<><path className="ln" d="M10 29 L16 7 L22 29 M12 21 h8 M14 14 h4" /><circle className="ac" cx="16" cy="6" r="2.5" /><path className="acs" d="M22 3 a7 7 0 0 1 0 8 M10 3 a7 7 0 0 0 0 8" /></>);
export const IconWave = () => icon(<path className="acs" d="M3 16 h4 l3 -7 l4 15 l4 -19 l4 17 l3 -9 l2 3 h2" />);
const IconPlug = () => icon(<><path className="ln" d="M11 4 v6 M21 4 v6 M8 10 h16 v6 a8 8 0 0 1 -16 0 z M16 24 v5" /></>);
const IconApps = () => icon(<><rect className="ln" x="4" y="6" width="24" height="20" rx="3" /><path className="ln" d="M4 11 h24" /><path className="acs" d="M12 15 l8 8 M20 15 l-8 8" /></>);
const IconLaptop = () => icon(<><rect className="ln" x="6" y="7" width="20" height="14" rx="2" /><path className="ln" d="M3 25 h26" /></>);
const IconChip = () => icon(<><rect className="ln" x="8" y="8" width="16" height="16" rx="2" /><path className="ln" d="M12 4 v4 M20 4 v4 M12 24 v4 M20 24 v4 M4 12 h4 M4 20 h4 M24 12 h4 M24 20 h4" /><rect className="ac" x="13" y="13" width="6" height="6" rx="1" /></>);
const IconKey = () => icon(<><circle className="ln" cx="11" cy="16" r="6" /><path className="ln" d="M17 16 h11 M24 16 v5 M28 16 v4" /><circle className="ac" cx="11" cy="16" r="2" /></>);
const IconQuestion = () => icon(<><circle className="ln" cx="16" cy="16" r="12" /><path className="acs" d="M12 12 a4 4 0 1 1 5 4 v3 M17 23 v.5" /></>);
export const IconAntenna = () => icon(<><path className="ln" d="M16 29 V12 M11 29 h10" /><circle className="ac" cx="16" cy="10" r="2.5" /><path className="acs" d="M21 5 a8 8 0 0 1 0 10 M11 5 a8 8 0 0 0 0 10" /></>);
export const IconFolder = () => icon(<path className="ln" d="M3 8 h9 l3 3 h14 v15 h-26 z" />);
const IconDrive = () => icon(<><rect className="ln" x="3" y="10" width="26" height="12" rx="3" /><circle className="ac" cx="23" cy="16" r="2" /><path className="ln" d="M7 16 h9" /></>);
const IconClock = () => icon(<><circle className="ln" cx="16" cy="16" r="12" /><path className="acs" d="M16 9 v7 l5 3" /></>);
const IconCopy = () => icon(<><rect className="ln" x="10" y="10" width="16" height="18" rx="2" /><path className="ln" d="M6 22 V6 a2 2 0 0 1 2 -2 h12" /><path className="acs" d="M14 16 h8 M14 21 h6" /></>);
const IconFile = () => icon(<><path className="ln" d="M8 3 h11 l6 6 v20 h-17 z M19 3 v6 h6" /><path className="acs" d="M12 16 h9 M12 21 h9" /></>);
export const IconPuzzle = () => icon(<><path className="ln" d="M6 9 h6 a3 3 0 1 1 6 0 h6 v6 a3 3 0 1 0 0 6 v6 h-18 z" /><circle className="ac" cx="12" cy="20" r="2" /></>);
export const IconPeople = () => icon(<><circle className="ln" cx="12" cy="11" r="4" /><path className="ln" d="M4 26 a8 8 0 0 1 16 0" /><circle className="ac" cx="22" cy="12" r="3" /><path className="acs" d="M21 19 a7 7 0 0 1 8 7" /></>);
export const IconUpload = () => icon(<><path className="ln" d="M6 22 v5 h20 v-5" /><path className="acs" d="M16 21 V5 M10 11 l6 -6 l6 6" /></>);
const IconTagSmall = () => icon(<><path className="ln" d="M4 6 h12 l12 10 l-12 10 h-12 z" /><circle className="ac" cx="10" cy="16" r="2" /></>);
