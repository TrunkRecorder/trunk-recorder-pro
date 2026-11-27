// Where the recorder can be reached (the recorder's `platform.reach`,
// crates/trunk-pro/src/reach.rs): the address it listens on (server.bind,
// switched here between this computer and every address, from the next start;
// a --bind on the command line overrides it), and every way to the built-in
// interface it found this run: this computer, each network's address, the
// tailnet's, Tailscale Serve or Funnel, a Cloudflare Tunnel, server.hostname.
// A tunnel or proxy on this computer reaches it whatever it listens on.

import { updateConfig, useSelect } from "./controller.ts";
import { Info } from "./charts.tsx";
import { CopyButton } from "./inputs.tsx";
import { Group } from "./SetupLayout.tsx";
import type { Config, Reach, Route } from "./protocol.ts";
import { confirmed } from "./asking.ts";

/** Whether a page's host name is this computer's own. */
export const onThisComputer = (hostname: string) => {
  const h = hostname.toLowerCase().replace(/^\[|\]$/g, "");
  return h === "localhost" || h.endsWith(".localhost") || /^127\./.test(h) || h === "::1";
};

const VIA: Record<Route["via"], string> = {
  local: "This computer",
  network: "Network",
  tailnet: "Tailnet",
  tailscaleServe: "Tailscale Serve",
  funnel: "Tailscale Funnel",
  cloudflare: "Cloudflare Tunnel",
  hostname: "Host name",
};

const VIA_INFO: Record<Route["via"], string> = {
  local: "A browser on this computer.",
  network: "Devices on that network, if this computer's firewall lets the port through.",
  tailnet: "Devices on your tailnet, by this computer's Tailscale address.",
  tailscaleServe: "Devices on your tailnet, over HTTPS: tailscale serve passes them to this port.",
  funnel: "Anyone on the internet: Tailscale Funnel passes them to this port.",
  cloudflare: "Anyone on the internet who gets past Cloudflare: cloudflared passes them to this port. Cloudflare Access, set up in front, decides who.",
  hostname: "server.hostname (Interfaces → Ports): nothing found here leads to it, so it's reached however that name is (a proxy, a tunnel elsewhere).",
};

/** The recorder's last word on where it can be reached (none: an older recorder, or not yet sampled). */
export const useReach = () => useSelect((x) => x.host?.platform.reach ?? null, (a, b) => JSON.stringify(a) === JSON.stringify(b));

/**
 * Direct connections (server.bind): this computer only, or other devices too. Then where it can be
 * reached now, and a warning while sign-in is off and anything but this computer can.
 */
export function Connections(props: { c: Config; signIn: boolean }) {
  const reach = useReach();
  const container = useSelect((x) => x.host?.platform.container ?? null);
  const bind = props.c.server.bind;
  // (The saved address's kind is the recorder's word; before it has said, the two it's switched between.)
  const listen = reach?.saved.address === bind ? reach.saved.listen : bind === "127.0.0.1" ? "thisComputer" : bind === "0.0.0.0" ? "every" : "one";
  const set = async (to: string) => {
    if (to === bind) return;
    // A browser that came in directly loses its way in; one on this computer, or through a tunnel, keeps it.
    const tunnel = reach?.routes.some((r) => !r.direct && hostOf(r.url) === location.hostname.toLowerCase());
    const question = `From the next start, this browser can't reach the recorder at ${location.host}: only this computer, and tunnels on it, can.`;
    if (to === "127.0.0.1" && !onThisComputer(location.hostname) && !tunnel && !(await confirmed({ title: "Lock this browser out?", question, action: "Go on" }))) return;
    updateConfig((x) => void (x.server.bind = to));
  };
  const pending = reach && !reach.flag && reach.now.address !== bind;
  const port = props.c.server.port;
  const hint =
    listen === "thisComputer"
      ? container
        ? "Only programs in this container: the port Docker publishes doesn't reach it"
        : "Only programs on this computer: a browser here, or a tunnel or proxy here (Tailscale Serve, Cloudflare Tunnel)"
      : listen === "every"
        ? container
          ? "Devices that reach the Docker host, at the port it publishes"
          : `Any device that reaches one of this computer's addresses, if its firewall lets port ${port} through`
        : `Devices that reach ${bind}, as the config says`;
  return (
    <>
      <Group
        title="Direct connections"
        hint={
          <>
            <code>server.bind</code>, from the next start
          </>
        }
        actions={
          <>
            {pending && (
              <span className="chip warn" role="status">
                Restart to apply
              </span>
            )}
            {reach?.flag && (
              <span className="chip" role="status">
                --bind {reach.now.address}
                <Info>Started with --bind on the command line, which this run listens on. The setting here is used when it starts without one.</Info>
              </span>
            )}
          </>
        }
        cols={1}
      >
        <div className="field wide">
          <div className="seg small" role="radiogroup" aria-label="Direct connections">
            <button role="radio" aria-checked={listen === "thisComputer"} className={listen === "thisComputer" ? "on" : ""} onClick={() => set("127.0.0.1")}>
              This computer only
            </button>
            <button role="radio" aria-checked={listen === "every"} className={listen === "every" ? "on" : ""} onClick={() => set("0.0.0.0")}>
              Other devices too
            </button>
          </div>
          <span className="field-hint">
            {hint}
            <Info>
              Saved as <code>server.bind</code>: <code>127.0.0.1</code> (this computer) or <code>0.0.0.0</code> (every address), used from the next start. A <code>--bind</code> on the command line overrides it.
              Don&apos;t forward the port from your router: from outside, use Tailscale, a VPN or Cloudflare Tunnel, with sign-in on.
            </Info>
          </span>
        </div>
      </Group>
      {reach && <ReachList reach={reach} c={props.c} signIn={props.signIn} container={!!container} />}
    </>
  );
}

/** A URL's host, lowercase, no brackets. */
const hostOf = (url: string) => {
  try {
    return new URL(url).hostname.toLowerCase().replace(/^\[|\]$/g, "");
  } catch {
    return "";
  }
};

/** Every way to the built-in interface found this run. */
export function ReachList(props: { reach: Reach; c: Config; signIn: boolean; container: boolean }) {
  const { reach } = props;
  const cfAccess = !!props.c.server.auth?.proxy?.cloudflare;
  const open = reach.routes.filter((r) => r.via !== "local");
  const internet = open.some((r) => r.public && !(r.via === "cloudflare" && cfAccess));
  return (
    <Group title="Reachable at" hint={`This run, listening on ${reach.now.address}`} cols={1}>
      <div className="iface-ports reach" role="table" aria-label="Reachable at">
        {reach.routes.map((r) => (
          <div className="iface-ports-row" role="row" key={r.url}>
            <span role="cell">
              <strong>{VIA[r.via]}</strong> {r.iface && <span className="muted small">{r.iface}</span>}
              <Info>{VIA_INFO[r.via]}</Info>
            </span>
            <span role="cell">
              {r.public && <span className="chip warn">Internet</span>}
              {r.via === "cloudflare" && cfAccess && <span className="chip ok">Access</span>}
            </span>
            <span role="cell">
              <a href={r.url} target="_blank" rel="noreferrer">
                {r.url}
              </a>{" "}
              <CopyButton text={r.url} />
            </span>
          </div>
        ))}
        {props.container && reach.now.listen === "every" && (
          <div className="iface-ports-row" role="row">
            <span role="cell">
              <strong>Docker host</strong>
            </span>
            <span role="cell" />
            <span role="cell" className="muted small">
              Its address, at the port it publishes
            </span>
          </div>
        )}
      </div>
      {!props.signIn && open.length > 0 && (
        <p className="warn-text small" role="status">
          Sign-in is off: anyone {internet ? "on the internet" : "who can open these"} can change settings, stop recording and download recordings. Turn on Sign-in above.
        </p>
      )}
    </Group>
  );
}
