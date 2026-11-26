// A system's "Share with the directory" switch (Setup → Systems → Basics):
// while it records, the recorder shares what it hears of the system with
// the directory at trunkrecorder.pro/directory, again as it hears new
// talkgroups or voice channels (at most hourly; daily otherwise). The
// engine does the sharing (crates/trunk-app share.rs); this shows how it went.

import { useState } from "react";
import type { Config, System } from "./protocol.ts";
import { hasCallSign, trpBase } from "./config.ts";
import { siteFrequencies, snapshot, updateConfig, useApp } from "./controller.ts";
import { Fig, Info } from "./charts.tsx";
import { ago } from "./fmt.ts";
import { IdentifyDialog } from "./Identify.tsx";
import { Toggle } from "./inputs.tsx";

/** Protocols the directory takes shares of. */
export const SHAREABLE: System["type"][] = ["p25", "smartnet", "dmr", "nxdn"];

/** The directory's page for a system, on the API base the config names. */
export function directoryUrl(c: Config, id: string): string {
  return `${trpBase(c)}/directory/systems/${encodeURIComponent(id)}`;
}

export function ShareSystem(props: { c: Config; i: number }) {
  const { c, i } = props;
  const sys = c.systems[i];
  const app = useApp();
  const [identifying, setIdentifying] = useState(false);
  const [noCallSign, setNoCallSign] = useState(false);
  const can = SHAREABLE.includes(sys.type);
  const status = app.status?.systems.find((y) => y.shortName === sys.shortName)?.share ?? null;
  const set = (on: boolean) =>
    updateConfig((x) => {
      if (on) x.systems[i].share = true;
      else delete x.systems[i].share;
    });
  const turnOn = () => set(true);
  const freqs = siteFrequencies(sys.sites).map((x) => x.freqHz);
  return (
    <div className="field wide share-system">
      <div className="row share-toggle">
      <Toggle
        label="Share with the directory"
        checked={!!sys.share && can}
        disabled={!can}
        onChange={(v) => {
          setNoCallSign(false);
          if (!v) return set(false);
          // Sharing needs an FCC call sign (for now): without one, Identify first; on only if it finds one.
          if (hasCallSign(sys.fcc)) turnOn();
          else setIdentifying(true);
        }}
        hint={can ? "Sites, channels, talkgroup numbers" : "Not for EDACS yet"}
      />
      {can && (
        // (Beside the switch, not in its label: a click on it would flip the switch.)
        <Info>
          Its sites, channels and the talkgroups it hears (numbers, how often, encrypted or not) go to the directory at trunkrecorder.pro. Not talkgroup names, and
          not where you are. It shares once it has decoded the system for a few minutes while recording, then at most hourly as it hears new talkgroups or voice
          channels (daily otherwise). Shares carry an anonymous ID for this install, nothing else about you. Sharing needs an FCC call sign.
        </Info>
      )}
      </div>
      {identifying && (
        <IdentifyDialog
          target={{ kind: "system", index: i }}
          what={sys.name || sys.shortName}
          freqs={freqs}
          fcc={sys.fcc}
          intro={
            <p className="small">
              Sharing needs an FCC call sign <Info>So the directory knows which licensed system it is. Share turns on once a call sign is kept.</Info>
            </p>
          }
          onClose={() => {
            setIdentifying(false);
            // What Identify kept is in the config already.
            if (hasCallSign(snapshot().config?.systems[i]?.fcc)) turnOn();
            else setNoCallSign(true);
          }}
        />
      )}
      {noCallSign && !sys.share && (
        <p className="small share-explain">
          {freqs.length ? (
            <>
              <span className="chip warn">No call sign</span> Share stays off{" "}
              <Info>A Motorola P25 system's call sign is often heard on the air while recording; or try Identify farther out.</Info>
            </>
          ) : (
            <>
              <span className="chip warn">No control channels</span> Identify needs them
            </>
          )}
        </p>
      )}
      {sys.share && can && <ShareLine c={c} sys={sys} state={status?.state} lastShared={status?.lastShared ?? null} error={status?.error ?? null} />}
    </div>
  );
}

function ShareLine(props: { c: Config; sys: System; state?: string; lastShared: number | null; error: string | null }) {
  const { c, sys } = props;
  const last = props.lastShared;
  const link = sys.directory?.id && (
    <a href={directoryUrl(c, sys.directory.id)} target="_blank" rel="noreferrer">
      Directory ↗
    </a>
  );
  if (!hasCallSign(sys.fcc) || props.state === "needsCallSign")
    return (
      <p className="small share-status error">
        <span className="chip bad">Not shared</span> Needs a call sign: try Identify
      </p>
    );
  if (props.state === "error")
    return (
      <p className="small share-status error">
        <span className="chip bad" title={props.error ?? undefined}>
          Not shared
        </span>{" "}
        {props.error ?? ""}
        {link && <> · {link}</>}
      </p>
    );
  return (
    <p className="small muted share-status">
      {last ? (
        <>
          Shared <Fig v={ago(last / 1000)} />
        </>
      ) : props.state === "waiting" ? (
        "First share after a few minutes"
      ) : (
        "Shares while recording"
      )}
      {link && <> · {link}</>}
    </p>
  );
}
