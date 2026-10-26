// What the survey looks for: a system from the directory (by its sites, or,
// no one having shared it, only its licenses), a RadioReference sites file,
// or a system set up already. The survey scans just its frequencies, marks
// the signals on its licenses, and judges each of its sites by what its
// frequencies showed (the recorder's trunk_core::sitecheck): which of them
// can be heard here.

import { siteNameOf, watchedChannels } from "./config.ts";
import type { Bundle } from "./directoryImport.tsx";
import type { FccLicenses, Site, SiteIdentity, System } from "./protocol.ts";

/** One site to check: the frequencies it is checked on, and what it says about itself. */
export interface LookSite {
  /** Which of its bundle's (or system's) sites. */
  index: number;
  label: string;
  freqsHz: number[];
  expect: SiteIdentity;
  colorCode?: number;
}

export interface LookingFor {
  /** What it's called. */
  name: string;
  type: System["type"] | null;
  /** Its licenses by frequency: the signals on them are marked as its own. Null: none known. */
  fcc: FccLicenses | null;
  /** Its sites (none: known only by its licenses, which are scanned instead). */
  sites: LookSite[];
  /** Where it came from: a bundle to import (the directory's, or a sites file's), or the system set up already (its short name). */
  bundle: Bundle | null;
  system: string | null;
}

/** The frequencies a site is checked on: its control channels; one without (DMR Capacity Plus, EDACS with a channel list only) its channels. */
export function siteChecked(sys: Pick<System, "type">, x: Partial<Site>): number[] {
  const cc = x.controlChannelsHz ?? [];
  return cc.length ? cc : watchedChannels(sys, { dmrChannelsHz: [], nxdnChannelsHz: [], lcnTableHz: {}, ...x } as Site);
}

const lookSite = (sys: Pick<System, "type">, x: Partial<Site>, index: number, label: string): LookSite => ({
  index,
  label,
  freqsHz: siteChecked(sys, x),
  expect: x.expect ?? {},
  ...(x.colorCode != null ? { colorCode: x.colorCode } : {}),
});

/** A bundle's site's name: its own, else its identity's, else its place. */
export const bundleSiteName = (x: Partial<Site>, k: number) => x.name || (x.expect?.rfss != null ? `Site ${x.expect.rfss}-${x.expect.site}` : `Site ${k + 1}`);

/** Looking for a bundle's system: its sites (`only`: these of them), else its licensed frequencies. */
export function lookForBundle(b: Bundle, name: string, only?: number[]): LookingFor {
  const sys = { type: b.system.type ?? "p25" };
  const sites = b.system.sites.map((x, k) => ({ x, k })).filter(({ k }) => !only || only.includes(k));
  return {
    name,
    type: b.system.type ?? null,
    fcc: Object.keys(b.system.fcc ?? {}).length ? b.system.fcc! : null,
    sites: sites.map(({ x, k }) => lookSite(sys, x, k, bundleSiteName(x, k))).filter((s) => s.freqsHz.length > 0),
    bundle: b,
    system: null,
  };
}

/** Looking for a system set up already: which of its sites can be heard here. */
export function lookForSystem(sys: System): LookingFor {
  return {
    name: sys.name?.trim() || sys.shortName,
    type: sys.type,
    fcc: Object.keys(sys.fcc ?? {}).length ? sys.fcc! : null,
    sites: sys.sites.map((x, k) => lookSite(sys, x, k, siteNameOf(sys, x))).filter((s) => s.freqsHz.length > 0),
    bundle: null,
    system: sys.shortName,
  };
}

/** Each frequency's listen the recorder takes when none is chosen, s (its default_listen_s). */
export const defaultListenS = (type: System["type"] | null) => (type === "dmr" ? 60 : type ? 5 : 2);

/** What surveyStart says to look for: its sites' frequencies, else its licensed ones. */
export function lookForMessage(l: LookingFor, listenS?: number) {
  const freqsHz = l.sites.length ? [] : Object.keys(l.fcc ?? {}).map(Number);
  const sites = l.sites.map(({ label, freqsHz, expect, colorCode }) => ({ label, freqsHz, expect, ...(colorCode != null ? { colorCode } : {}) }));
  return { freqsHz, sites, ...(l.type ? { type: l.type } : {}), ...(listenS ? { listenS } : {}) };
}

/** Every frequency it scans. */
export const lookedFreqs = (l: LookingFor) => [...new Set(l.sites.length ? l.sites.flatMap((s) => s.freqsHz) : Object.keys(l.fcc ?? {}).map(Number))];
