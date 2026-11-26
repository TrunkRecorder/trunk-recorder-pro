// Signal taps (Setup → Recording → Advanced): what the receive chain makes
// on the way to a call's audio, saved to files or taken by plugins. One table:
// a row per tap saved, and — read-only — a row per tap a plugin takes (it
// asks by subscribing to `tap.<id>`; turning the plugin off is its switch).

import { pluginOn, updateConfig, useApp } from "./controller.ts";
import { Info } from "./charts.tsx";
import { systemsOf } from "./Plugins.tsx";
import { signalTaps } from "./rules.ts";
import { Group } from "./SetupLayout.tsx";
import type { TapInfo, TapRoute } from "./protocol.ts";

const FILES: Record<TapInfo["carries"], string> = { samples: ".wav + .jsonl", frames: ".jsonl", notes: ".jsonl" };
// (The vocoded frames are also an .sdr file, where the voice has a layout there: P25 and DMR.)
const filesOf = (t: TapInfo) => (t.id === "voice.frames" ? ".sdr + .jsonl" : FILES[t.carries]);

export function SignalTaps() {
  const s = useApp();
  const c = s.config;
  if (!c) return null;
  const taps = signalTaps();
  const info = (id: string) => taps.find((t) => t.id === id);
  const rows = c.recording.taps ?? [];
  const systems = systemsOf(c);
  // What plugins that are on take: theirs for as long as they run.
  const taken = (s.plugins?.plugins ?? [])
    .filter((p) => pluginOn(c, p.id) && p.manifest)
    .flatMap((p) => p.manifest!.subscribe.filter((t) => t.startsWith("tap.") && info(t.slice(4))).map((t) => ({ plugin: p.manifest!.name, tap: info(t.slice(4))! })));
  const change = (fn: (rows: TapRoute[]) => void) =>
    updateConfig((x) => {
      const next = [...(x.recording.taps ?? [])];
      fn(next);
      if (next.length) x.recording.taps = next;
      else delete x.recording.taps;
    });
  const free = taps.find((t) => !rows.some((r) => r.tap === t.id)) ?? taps[0];
  return (
    <section className="set-card" aria-labelledby="taps-title">
      <header className="set-card-head">
        <div className="set-card-title">
          <h3 id="taps-title">Signal taps</h3>
          <span className="set-card-sub">
            Mid-chain data
            <Info>
              What a voice channel's receiver makes on the way to a call's audio, for other decoders and for diagnosis. Each transmission is a file of its own
              beside the system's calls, with a <code>.jsonl</code> of what's known about it. A plugin takes a tap by subscribing to it: it needs no row.
            </Info>
          </span>
        </div>
        <span className="spacer" />
        <span className={`chip ${rows.length + taken.length ? "ok" : ""}`}>{rows.length + taken.length ? `${rows.length + taken.length} on` : "off"}</span>
      </header>
      <Group
        title="Taps"
        full
        actions={
          <button type="button" className="btn small" onClick={() => change((r) => void r.push({ tap: free.id }))}>
            Add tap
          </button>
        }
      >
        {rows.length + taken.length > 0 && (
          <table className="taps small">
            <thead>
              <tr>
                <th>Tap</th>
                <th>From</th>
                <th>Systems</th>
                <th>To</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => {
                const t = info(r.tap);
                const picked = r.systems ?? [];
                const toggle = (id: number) =>
                  change((x) => {
                    const now = picked.includes(id) ? picked.filter((y) => y !== id) : [...picked, id];
                    x[i] = now.length ? { tap: r.tap, systems: now } : { tap: r.tap };
                  });
                return (
                  <tr key={i}>
                    <td>
                      <select aria-label={`Tap ${i + 1}`} value={r.tap} onChange={(e) => change((x) => void (x[i] = { ...x[i], tap: e.target.value }))}>
                        {!t && <option value={r.tap}>{r.tap}</option>}
                        {taps.map((o) => (
                          <option key={o.id} value={o.id}>
                            {o.title}
                          </option>
                        ))}
                      </select>
                      {t && <Info>{t.about}</Info>}
                    </td>
                    <td className="muted">{t?.from ?? "Unknown"}</td>
                    <td>
                      <div className="plugin-systems" role="group" aria-label={`Systems tap ${i + 1} is taken from`}>
                        <button type="button" className={`chip tick${picked.length ? "" : " on"}`} aria-pressed={!picked.length} onClick={() => change((x) => void (x[i] = { tap: r.tap }))}>
                          All
                        </button>
                        {systems.map((x) => (
                          <button
                            type="button"
                            key={x.name}
                            className={`chip tick${x.id !== undefined && picked.includes(x.id) ? " on" : ""}`}
                            aria-pressed={x.id !== undefined && picked.includes(x.id)}
                            disabled={x.id === undefined}
                            title={x.id === undefined ? "Saved first" : ""}
                            onClick={() => toggle(x.id!)}
                          >
                            {x.name}
                          </button>
                        ))}
                      </div>
                    </td>
                    <td>
                      Files <span className="muted mono">{t ? filesOf(t) : ""}</span>
                    </td>
                    <td>
                      <button type="button" className="btn ghost small danger" aria-label={`Remove tap ${i + 1}`} onClick={() => change((x) => void x.splice(i, 1))}>
                        Remove
                      </button>
                    </td>
                  </tr>
                );
              })}
              {taken.map((x) => (
                <tr key={`${x.plugin}-${x.tap.id}`} className="auto">
                  <td>
                    {x.tap.title}
                    <Info>{x.tap.about}</Info>
                  </td>
                  <td className="muted">{x.tap.from}</td>
                  <td className="muted">The plugin's</td>
                  <td>{x.plugin}</td>
                  <td className="muted">Automatic</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Group>
    </section>
  );
}
