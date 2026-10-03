// call-log.mjs — a program, not a page: append every recorded call to a CSV
// file, and optionally POST it as JSON to a webhook. Built on client.js
// (keep it beside this file's folder: ../client.js). Node 22 or later.
//
//   node call-log.mjs                                  # localhost:8080 → calls.csv
//   node call-log.mjs --server pi.local:8080 --csv /var/log/calls.csv
//   node call-log.mjs --webhook https://example.com/hook --talkgroups 101,202
//
// Programs aren't browsers: no Origin, so the recorder lets them in without
// server.allowedOrigins. (It still has to be reachable: --bind 0.0.0.0 for
// another machine, on a network you trust.)

import { appendFileSync, existsSync } from "node:fs";
import { parseArgs } from "node:util";
import { TrunkClient } from "../client.js";

const { values: opt } = parseArgs({
  options: {
    server: { type: "string", default: "localhost:8080" },
    csv: { type: "string", default: "calls.csv" },
    webhook: { type: "string" },
    talkgroups: { type: "string" },
  },
});
const only = opt.talkgroups ? new Set(opt.talkgroups.split(",").map(Number)) : null;

const columns = ["start", "system", "talkgroup", "talkgroup_tag", "seconds", "freq_mhz", "emergency", "encrypted", "units", "snr_db", "audio_url"];
const cell = (v) => (/[",\n]/.test(String(v)) ? `"${String(v).replaceAll('"', '""')}"` : String(v));
if (!existsSync(opt.csv)) appendFileSync(opt.csv, columns.join(",") + "\n");

const rec = new TrunkClient({ server: opt.server });
rec.on("connection", (up) => console.log(up ? `Connected to ${opt.server} (Trunk Recorder Pro ${rec.version}); recorder ${rec.phase.phase}` : "Disconnected; reconnecting…"));

rec.on("concluded", async ({ entry }) => {
  const r = entry.record;
  if (only && !only.has(r.talkgroup)) return;
  const row = {
    start: new Date(r.start_time * 1000).toISOString(),
    system: r.short_name ?? "",
    talkgroup: r.talkgroup,
    talkgroup_tag: r.talkgroup_tag,
    seconds: r.call_length,
    freq_mhz: (r.freq / 1e6).toFixed(5),
    emergency: r.emergency,
    encrypted: r.encrypted,
    units: r.srcList.map((s) => s.tag || s.src).join(" "),
    snr_db: r.snr ?? "",
    audio_url: rec.callUrl(entry, "wav"),
  };
  appendFileSync(opt.csv, columns.map((c) => cell(row[c])).join(",") + "\n");
  console.log(`${row.start}  ${row.system}  ${row.talkgroup_tag || row.talkgroup}  ${row.seconds}s`);
  if (opt.webhook) {
    try {
      const res = await fetch(opt.webhook, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ ...row, record: r }) });
      if (!res.ok) console.error(`webhook: ${res.status} ${res.statusText}`);
    } catch (e) {
      console.error(`webhook: ${e.message}`);
    }
  }
});
