// A system's "Share with the community" switch (Setup → Systems → Basics):
// while it records, the recorder shares what it hears of the system with
// the directory at trunkrecorder.pro/directory, again as it hears new
// talkgroups or voice channels (at most hourly; daily otherwise). The
// engine does the sharing (crates/trunk-app share.rs); this shows how it went.

import { useState } from "react";
import type { Config, System } from "./protocol.ts";
import { siteFrequencies, updateConfig, useApp } from "./controller.ts";
import { ago } from "./fmt.ts";
import { IdentifyDialog } from "./Identify.tsx";
import { Toggle } from "./inputs.tsx";

/** Protocols the directory takes shares of. */
export const SHAREABLE: System["type"][] = ["p25", "smartnet", "dmr", "nxdn"];

/** The directory's page for a system, on the API base the config names. */
export function directoryUrl(c: Config, id: string): string {
  const base = (c.server?.trpApi || "https://trunkrecorder.pro").replace(/\/+$/, "");
  return `${base}/directory/systems/${encodeURIComponent(id)}`;
}

/** For now, a system is shared only once it has an FCC call sign (Identify, the air, or typed): switching Share on without one asks Identify first. */
export const hasCallSign = (sys: System) => Object.values(sys.fcc ?? {}).some((l) => l.callSign.trim() !== "");

export function ShareSystem(props: { c: Config; i: number }) {
  const { c, i } = props;
  const sys = c.systems[i];
  const app = useApp();
  const [explain, setExplain] = useState(false);
  const [identifying, setIdentifying] = useState(false);
  const [noCallSign, setNoCallSign] = useState(false);
  const can = SHAREABLE.includes(sys.type);
  const status = app.status?.systems.find((y) => y.shortName === sys.shortName)?.share ?? null;
  const set = (on: boolean) =>
    updateConfig((x) => {
      if (on) x.systems[i].share = true;
      else delete x.systems[i].share;
    });
  const turnOn = () => {
    set(true);
    setExplain(!sys.directory);
  };
  const freqs = siteFrequencies(sys.sites).map((x) => x.freqHz);
  return (
    <div className="field wide share-system">
      <Toggle
        label="Share channel and talkgroup config with the community"
        checked={!!sys.share && can}
        disabled={!can}
        onChange={(v) => {
          setNoCallSign(false);
          if (!v) return (set(false), setExplain(false));
          // Sharing needs an FCC call sign: without one, Identify first; on only if it finds one.
          if (hasCallSign(sys)) turnOn();
          else setIdentifying(true);
        }}
        hint={
          can
            ? "Its sites, channels and the talkgroups it hears (numbers, how often, encrypted or not) go to the directory at trunkrecorder.pro, and again as it hears new ones. Not talkgroup names, and not where you are."
            : "Conventional and EDACS systems can't be shared yet."
        }
      />
      {identifying && (
        <IdentifyDialog
          target={{ kind: "system", index: i }}
          what={sys.name || sys.shortName}
          freqs={freqs}
          fcc={sys.fcc}
          intro={
            <p className="small">
              Sharing needs the system's FCC call sign, so the directory knows which licensed system it is. Look up who is licensed on its frequencies
              near you: Share turns on once a call sign is kept.
            </p>
          }
          onClose={(kept) => {
            setIdentifying(false);
            if (kept && Object.values(kept).some((l) => l.callSign.trim())) turnOn();
            else setNoCallSign(true);
          }}
        />
      )}
      {noCallSign && !sys.share && (
        <p className="small muted share-explain">
          {freqs.length
            ? "No FCC call sign was kept, so Share stays off. A Motorola P25 system's is often heard on the air while recording; or try Identify farther out."
            : "Add its control channels first: Identify looks up who is licensed on them."}
        </p>
      )}
      {explain && sys.share && (
        <p className="small muted share-explain">
          It shares once it has decoded {sys.shortName} for a few minutes while recording, then at most hourly as it hears new talkgroups or voice
          channels (daily otherwise). Shares carry an anonymous ID for this install, nothing else about you.
        </p>
      )}
      {sys.share && can && <ShareLine c={c} sys={sys} state={status?.state} lastShared={status?.lastShared ?? null} error={status?.error ?? null} />}
    </div>
  );
}

function ShareLine(props: { c: Config; sys: System; state?: string; lastShared: number | null; error: string | null }) {
  const { c, sys } = props;
  const last = props.lastShared ?? sys.directory?.sharedAt ?? null;
  const link = sys.directory?.id && (
    <a href={directoryUrl(c, sys.directory.id)} target="_blank" rel="noreferrer">
      View in the directory
    </a>
  );
  if (!hasCallSign(sys) || props.state === "needsCallSign")
    return <p className="small share-status error">Not shared until it has an FCC call sign: Identify finds it.</p>;
  if (props.state === "error")
    return (
      <p className="small share-status error">
        {`Not shared: ${props.error ?? ""}`}
        {link && <> · {link}</>}
      </p>
    );
  return (
    <p className="small muted share-status">
      {last ? `Shared ${ago(last / 1000)}` : props.state === "waiting" ? "Shares once it has been decoded a few minutes" : "Shares while it records"}
      {link && <> · {link}</>}
    </p>
  );
}
