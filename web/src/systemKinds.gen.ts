// Written from the recorder's table (trunk-app config.rs, SystemKind::info): don't edit.
// `TRP_WRITE=1 cargo test -p trunk-app the_webs_system_kinds_are_these` writes it again.

/** What each kind of system is, beside its name. */
export const SYSTEM_KINDS = {
  p25: { label: "P25", watched: false, siteLock: ["nac", "wacn", "sysId", "rfss", "site"] },
  smartnet: { label: "SmartNet", watched: false, siteLock: ["sysId", "site"] },
  dmr: { label: "DMR", watched: true, siteLock: [] },
  nxdn: { label: "NXDN", watched: true, siteLock: ["sysId", "site"] },
  edacs: { label: "EDACS", watched: true, siteLock: ["site"] },
} as const;

export type SystemKind = keyof typeof SYSTEM_KINDS;
