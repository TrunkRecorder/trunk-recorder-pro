// Send To: which plugins a conventional channel's or a talkgroup's calls go
// to. Every plugin unless it names some, so one set of radios can feed
// services with different lists. The recorder decides
// (crates/trunk-pro/src/plugins/route.rs); a channel says it in its sendTo, a
// talkgroup in its system's talkgroup file (the Send To column).

import type { Channel, Config, PluginInfo } from "./protocol.ts";
import { takers } from "./Delays.tsx";
import { normPlugin } from "./talkgroups.ts";

/**
 * The plugins a channel can be sent to: those on that take finished calls. (One that only
 * streams live audio, such as simplestream, gets every call and filters by its own settings.)
 */
export function sendToChoices(c: Config, plugins: PluginInfo[]): PluginInfo[] {
  return takers(c, plugins).filter((p) => p.manifest?.subscribe.includes("call.concluded"));
}

/** Whether the Send to column is shown: a plugin to choose (or leave out), or a channel already naming some. */
export function showSendTo(choices: PluginInfo[], channels: Channel[]): boolean {
  return choices.length > 0 || channels.some((ch) => ch.sendTo !== undefined);
}

const nameOf = (p: PluginInfo) => p.manifest?.name ?? p.id;

/** `s` (as written: any case, spaces, `-`, `_`) names plugin `id`, or the plugin `id` is a copy of ("rdioscanner#2"). */
const names = (s: string, id: string) => normPlugin(s) === normPlugin(id) || normPlugin(s) === normPlugin(id.replace(/#\d+$/, ""));

/** Whether `sendTo` takes plugin `id`. */
const takes = (sendTo: string[] | undefined, id: string) => sendTo === undefined || sendTo.some((s) => names(s, id));

/** What a Send To says, in words. */
export function sendToText(sendTo: string[] | undefined): string {
  return sendTo === undefined ? "every plugin" : sendTo.length ? sendTo.join(", ") : "no plugin";
}

/** A conventional channel's Send to cell in the channel table. */
export function SendToCell(props: { ch: Channel; choices: PluginInfo[]; disabled?: boolean; onChange: (sendTo: string[] | undefined) => void }) {
  return <SendToPicker sendTo={props.ch.sendTo} choices={props.choices} disabled={props.disabled} onChange={props.onChange} />;
}

/**
 * One checkbox per plugin. All ticked is every plugin (no Send To), so a
 * plugin added later gets the calls too; untick one and it names the rest.
 */
export function SendToPicker(props: { sendTo: string[] | undefined; choices: PluginInfo[]; disabled?: boolean; onChange: (sendTo: string[] | undefined) => void }) {
  const { sendTo, choices } = props;
  const ids = choices.map((p) => p.id);
  // Names it holds that aren't among the choices (a plugin switched off, a copy's label): kept as they are.
  const others = (sendTo ?? []).filter((s) => !ids.some((id) => names(s, id)));
  const set = (id: string, on: boolean) => {
    const now = ids.filter((x) => (x === id ? on : takes(sendTo, x)));
    props.onChange(now.length === ids.length && !others.length ? undefined : [...now, ...others].sort());
  };
  return (
    <span className="send-to" title={`Its calls go to ${sendToText(sendTo)}`}>
      {choices.map((p) => (
        <label key={p.id} className="send-to-option">
          <input type="checkbox" disabled={props.disabled} checked={takes(sendTo, p.id)} aria-label={`Send to ${nameOf(p)}`} onChange={(e) => set(p.id, e.target.checked)} />
          {nameOf(p)}
        </label>
      ))}
      {others.map((o) => (
        <span key={o} className="send-to-option muted">
          {o}
        </span>
      ))}
    </span>
  );
}
