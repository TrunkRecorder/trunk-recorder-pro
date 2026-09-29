// System types by config name. Add "smartnet", "dmr", "conventional", … here.
import type { ProtocolDriver } from "./types.ts";
import { p25Driver } from "./p25/index.ts";
import type { P25Modulation } from "./p25/controlDecoder.ts";

export interface DriverOptions {
  modulation?: P25Modulation;
}

export function driverFor(type: string, opts: DriverOptions = {}): ProtocolDriver {
  switch (type) {
    case "p25":
      return p25Driver(opts.modulation ?? "auto");
    default:
      throw new Error(`System type "${type}" is not supported yet (only "p25").`);
  }
}
