// client.js's LivePlayer (docs/api/client.js), the live-audio player of the
// dashboard and of every interface, on a stand-in AudioContext: which call it
// follows, skip, where its sound goes, pan and gain, stop. `npm test`.
import assert from "node:assert/strict";
import { test } from "node:test";
import { LivePlayer } from "../../docs/api/client.js";

class Param {
  value = 0;
  set: number[] = [];
  setTargetAtTime(v: number) {
    this.set.push(v);
    this.value = v;
  }
}

class Node {
  to: Node[] = [];
  gain = new Param();
  pan = new Param();
  constructor(readonly context: Ctx) {}
  connect(n: Node) {
    this.to.push(n);
    return n;
  }
  disconnect() {
    this.to = [];
  }
}

class Source extends Node {
  buffer: { duration: number; length: number } | null = null;
  at = -1;
  stopped = false;
  onended: (() => void) | null = null;
  start(t: number) {
    this.at = t;
    this.context.sources.push(this);
  }
  stop() {
    this.stopped = true;
  }
}

class Ctx {
  state = "running";
  currentTime = 10;
  sampleRate = 48000;
  closed = false;
  sources: Source[] = [];
  destination = new Node(this);
  createGain = () => new Node(this);
  createStereoPanner = () => new Node(this);
  createBufferSource = () => new Source(this);
  createBuffer = (_ch: number, length: number, rate: number) => ({ length, duration: length / rate, getChannelData: () => new Float32Array(length) });
  resume = () => Promise.resolve();
  close = () => {
    this.closed = true;
  };
}

/** 20 ms of a call's audio. */
const chunk = (callId: number, talkgroup = 100 + callId) => ({ system: 0, callId, talkgroup, samples: new Float32Array(160) });

/** A player on its own stand-in output: (player, the context, the node it plays into, the calls it said it plays). */
function setup(opts: Record<string, unknown> = {}) {
  const ctx = new Ctx();
  const out = new Node(ctx);
  const heard: (number | null)[] = [];
  const player = new LivePlayer({ output: out as never, onNowPlaying: (c) => heard.push(c?.callId ?? null), ...opts });
  void player.resume();
  return { player, ctx, out, heard };
}

test("LivePlayer: plays into its output through a gain and a panner, at the output's rate", () => {
  const { player, ctx, out } = setup({ gain: 3, pan: -0.5 });
  player.play(chunk(1));
  const src = ctx.sources[0];
  const [g] = src.to;
  const [p] = g.to;
  assert.equal(g.gain.value, 3);
  assert.equal(p.pan.value, -0.5);
  assert.deepEqual(p.to, [out], "the panner goes to the output it was given");
  // 160 samples at 8 kHz, resampled to 48 kHz (less the filter's few samples of delay).
  assert.ok(Math.abs(src.buffer!.length - 960) <= 48, `${src.buffer!.length} samples`);
  assert.ok(src.at >= ctx.currentTime + 0.3, "after a gap it starts a little ahead");
  player.stop();
});

test("LivePlayer: follows one call until it's quiet, then the next", () => {
  const { player, ctx, heard } = setup();
  player.play(chunk(1));
  player.play(chunk(2));
  assert.equal(ctx.sources.length, 1, "a call overlapping the one playing is skipped");
  assert.equal(player.nowPlaying?.callId, 1);
  ctx.currentTime += 3; // played out, and quiet longer than quietS
  player.play(chunk(2));
  assert.equal(player.nowPlaying?.callId, 2);
  assert.deepEqual(heard, [1, 2]);
  player.stop();
  assert.deepEqual(heard, [1, 2, null]);
});

test("LivePlayer: skip drops the call playing, what's queued of it and the rest of it", () => {
  const { player, ctx, heard } = setup();
  player.play(chunk(1));
  player.play(chunk(1));
  player.skip();
  assert.ok(ctx.sources.every((s) => s.stopped), "what was queued stops");
  assert.equal(player.nowPlaying, null);
  player.play(chunk(1));
  assert.equal(ctx.sources.length, 2, "the rest of the skipped call isn't played");
  player.play(chunk(2));
  assert.deepEqual(heard, [1, null, 2], "the next call heard plays at once");
  player.stop();
});

test("LivePlayer: pan and gain change while playing, gliding there; pan stays in -1…1", () => {
  const { player, ctx } = setup();
  player.play(chunk(1));
  const g = ctx.sources[0].to[0];
  const p = g.to[0];
  player.pan = 3;
  player.gain = 0;
  assert.equal(player.pan, 1);
  assert.deepEqual(p.pan.set, [1], "set by setTargetAtTime: a jump clicks");
  assert.deepEqual(g.gain.set, [0]);
  player.pan = -7;
  assert.equal(player.pan, -1);
  player.stop();
  player.pan = 0.25; // stopped: kept for the next resume()
  void player.resume();
  player.play(chunk(1));
  assert.equal(ctx.sources.at(-1)!.to[0].to[0].pan.value, 0.25);
  player.stop();
});

test("LivePlayer: stop lets go of its part of the output, which stays open for the others", () => {
  const { player, ctx, out } = setup();
  player.play(chunk(1));
  const g = ctx.sources[0].to[0];
  const p = g.to[0];
  player.stop();
  assert.deepEqual([g.to, p.to], [[], []]);
  assert.ok(ctx.sources[0].stopped);
  assert.equal(ctx.closed, false);
  assert.equal(out.context, ctx);
});

test("LivePlayer: a backlog over 3 s is dropped, and playing starts again a little ahead", () => {
  const { player, ctx } = setup({ followCall: false });
  for (let i = 0; i < 160; i++) player.play(chunk(1)); // 3.2 s queued at once
  const late = ctx.sources.filter((s) => s.stopped).length;
  assert.ok(late > 100, `${late} chunks dropped`);
  assert.ok(ctx.sources.at(-1)!.at < ctx.currentTime + 1);
  player.stop();
});

test("LivePlayer: nothing is queued while the browser holds the output suspended", () => {
  const { player, ctx } = setup();
  ctx.state = "suspended";
  player.play(chunk(1));
  assert.equal(ctx.sources.length, 0);
  assert.equal(player.nowPlaying, null);
  player.stop();
});
