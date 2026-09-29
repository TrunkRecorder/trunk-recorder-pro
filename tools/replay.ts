// Headless Trunk Recorder Lite: replay an rtl_sdr capture (unsigned 8-bit IQ)
// through the same channelizer + trunking engine the browser runs, writing each
// call as <base>.wav + <base>.json like Trunk Recorder does.
//
//   npm run replay -- capture.cu8 --center 858300000 --rate 2400000 \
//       --cc 857987500,858987500 [--out calls/] [--talkgroups tg.csv] [--preroll 1]
//
// Capture one with:  rtl_sdr -f 858300000 -s 2400000 -g 38.6 capture.cu8

import { createReadStream, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { performance } from "node:perf_hooks";
import { Channelizer } from "../src/engine/channelizer.ts";
import { driverFor } from "../src/protocols/registry.ts";
import { TrunkEngine, type ChannelPort } from "../src/trunking/trunkEngine.ts";
import { parseTalkgroupCsv } from "../src/trunking/talkgroups.ts";
import { encodeWav } from "../src/recording/wav.ts";

function arg(name: string, dflt?: string): string | undefined {
  const i = process.argv.indexOf(`--${name}`);
  return i >= 0 ? process.argv[i + 1] : dflt;
}

const file = process.argv[2];
if (!file || file.startsWith("--")) {
  console.error("usage: replay <capture.cu8> --center Hz --rate Hz --cc Hz[,Hz…] [--out dir] [--talkgroups csv] [--preroll s] [--modulation auto|fsk4|qpsk]");
  process.exit(2);
}
const centerHz = Number(arg("center"));
const rateHz = Number(arg("rate", "2400000"));
const ccs = (arg("cc") ?? "").split(",").filter(Boolean).map(Number);
const outDir = arg("out", "calls")!;
const prerollS = Number(arg("preroll", "1"));
const verbose = process.argv.includes("--verbose");
mkdirSync(outDir, { recursive: true });

const driver = driverFor("p25", { modulation: (arg("modulation", "auto") as "auto" | "fsk4" | "qpsk") });
const chz = new Channelizer({ fs: rateHz, minOutputRate: driver.minChannelRate, historyS: Math.max(prerollS, 0.1) });
const port: ChannelPort = {
  open: (offsetHz, cutoffHz, preroll, onIq) => chz.addHead(offsetHz, cutoffHz, onIq, preroll).id,
  close: (id) => chz.removeHead(id),
};
const tgFile = arg("talkgroups");
const engine = new TrunkEngine(
  {
    system: { shortName: "replay", type: "p25", controlChannels: ccs, talkgroups: tgFile ? parseTalkgroupCsv(readFileSync(tgFile, "utf8")) : undefined },
    centerHz,
    rateHz,
    channelRate: chz.outputRate,
    prerollS,
    maxRecorders: 32,
    keepSilentCalls: false,
    calls: {},
    epochMsAtZero: Date.now(),
  },
  driver,
  port,
  {
    onMessages: (msgs) => {
      if (verbose) for (const m of msgs) if (m.type !== "unknown") console.log(`${m.timeS.toFixed(2).padStart(7)}s  ${m.meta}`);
    },
    onCallStart: (c) =>
      console.log(`${c.startS.toFixed(2).padStart(7)}s  CALL ${c.id} start TG ${c.talkgroup} ${(c.freqHz / 1e6).toFixed(4)} MHz${c.phase2Tdma ? ` slot ${c.tdmaSlot}` : ""} → ${c.state}${c.reason ? ` (${c.reason})` : ""}`),
    onCallEnd: (c) => console.log(`${engine.status().nowS.toFixed(2).padStart(7)}s  CALL ${c.id} end   TG ${c.talkgroup} srcs [${c.sources.map((s) => s.src).join(",")}]${c.encrypted ? " ENC" : ""}`),
    onConcluded: (c) => {
      writeFileSync(join(outDir, `${c.baseName}.wav`), encodeWav(c.audio, c.audioRate));
      writeFileSync(join(outDir, `${c.baseName}.json`), JSON.stringify(c.record, null, 2));
      console.log(`         wrote ${c.baseName}.wav  (${(c.audio.length / c.audioRate).toFixed(1)} s audio, ${c.record.freqList[0].error_count} bad frames)`);
    },
    onControlChannel: (hz) => console.log(`         control channel ${(hz / 1e6).toFixed(5)} MHz`),
  },
);

engine.start();
const t0 = performance.now();
let bytes = 0;
for await (const chunk of createReadStream(file, { highWaterMark: 1 << 20 })) {
  const u8 = chunk as Buffer;
  chz.pushU8(new Uint8Array(u8.buffer, u8.byteOffset, u8.length & ~1));
  bytes += u8.length;
}
const airS = bytes / 2 / rateHz;
engine.stop();
const ms = performance.now() - t0;
const st = engine.status();
console.log(
  `\n${airS.toFixed(1)} s of air in ${(ms / 1000).toFixed(1)} s (${(airS / (ms / 1000)).toFixed(1)}× real time). ` +
    `CC: ${st.good} good / ${st.bad} bad TSBKs, ${st.modulation ?? "?"}, NAC ${st.identity.nac?.toString(16) ?? "?"} WACN ${st.identity.wacn?.toString(16) ?? "?"} SysID ${st.identity.sysId?.toString(16) ?? "?"}. ` +
    `${st.callsConcluded} call(s) written to ${outDir}/`,
);
