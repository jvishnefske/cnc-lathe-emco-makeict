// node --test test/  — the UI's stream logic and plot math, against the
// same vendored RxJS bundle the page loads.
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { client, commandResults, connectedOf, feed, logLine, logOf, stateOf, view } from "../streams.js";
import { fitView, gridPitchMm, tessellateArc, worldToScreen, screenToWorld, zoomAt } from "../toolpath.js";

const rx = createRequire(import.meta.url)("../vendor/rxjs-7.8.2.umd.min.js");

class FakeEventSource {
  constructor(url) {
    this.url = url;
    this.listeners = {};
    this.closed = false;
    FakeEventSource.last = this;
  }
  addEventListener(type, fn) {
    (this.listeners[type] ??= []).push(fn);
  }
  emit(type, data) {
    for (const fn of this.listeners[type] ?? []) fn({ data: typeof data === "string" ? data : JSON.stringify(data) });
  }
  close() {
    this.closed = true;
  }
}

const snapshot = (run, extra = {}) => ({
  session: {
    run,
    currentLine: -1,
    alarms: [],
    dro: { xRadiusMm: 1, zMm: 2, homed: true },
    ...extra,
  },
  sim: { xRadiusMm: 1, zMm: 2 },
  transport: "virtual",
});

test("the feed splits into state and connection, and closes the EventSource on unsubscribe", () => {
  const f$ = feed(rx, (u) => new FakeEventSource(u));
  const states = [];
  const conn = [];
  const s1 = stateOf(rx, f$).subscribe((s) => states.push(s.session.run));
  const s2 = connectedOf(rx, f$).subscribe((c) => conn.push(c));
  const es = FakeEventSource.last;
  assert.equal(es.url, "/api/events");
  es.onopen();
  es.emit("state", snapshot("idle"));
  es.emit("state", snapshot("running"));
  es.onerror();
  es.onopen();
  assert.deepEqual(states, ["idle", "running"]);
  assert.deepEqual(conn, [false, true, false, true]);
  s1.unsubscribe();
  s2.unsubscribe();
  assert.equal(es.closed, true);
});

test("a malformed event does not kill the stream", () => {
  const f$ = feed(rx, (u) => new FakeEventSource(u));
  const seen = [];
  const sub = f$.subscribe((e) => seen.push(e.type));
  FakeEventSource.last.emit("state", "{not json");
  FakeEventSource.last.emit("state", snapshot("idle"));
  assert.deepEqual(seen, ["bad", "state"]);
  sub.unsubscribe();
});

test("the log keeps the last lines, hides bytes unless asked, and reports lag", () => {
  const f$ = feed(rx, (u) => new FakeEventSource(u));
  const raw$ = new rx.BehaviorSubject(false);
  let last = [];
  const sub = logOf(rx, f$, raw$, 3).subscribe((l) => (last = l));
  const es = FakeEventSource.last;
  es.emit("core", { kind: "sent", line: 0, text: "G00 X1", atMs: 1000 });
  es.emit("core", { kind: "raw", dir: "tx", bytes: [0x47, 0x0d], atMs: 1001 });
  assert.equal(last.length, 1);
  raw$.next(true);
  es.emit("core", { kind: "raw", dir: "rx", bytes: [0x6f, 0x6b], atMs: 1002 });
  es.emit("lagged", "7");
  es.emit("core", { kind: "acked", line: 0, atMs: 1003 });
  assert.equal(last.length, 3, "capped");
  assert.match(last[0], /rx 6f 6b/);
  assert.match(last[1], /7 events missed/);
  assert.match(last[2], /✓ L1/);
  sub.unsubscribe();
});

test("log lines are 1-based and alarms are explicit", () => {
  assert.match(logLine({ kind: "sent", line: 4, text: "G01 Z-1", atMs: 0 }), /→ L5  G01 Z-1/);
  assert.match(logLine({ kind: "alarm", code: "ESTOP", message: "emergency stop", atMs: 0 }), /ALARM ESTOP/);
  assert.equal(logLine({ kind: "nonsense" }), null);
});

test("the view enables only what the run state allows", () => {
  const prog = { name: "p", errors: 0 };
  const off = view(snapshot("idle"), prog, false);
  assert.equal(off.run, "disconnected");
  assert.ok(Object.entries(off.enabled).every(([, v]) => !v), "nothing works without the link");

  const idle = view(snapshot("idle"), prog, true).enabled;
  assert.ok(idle.run && idle.home && idle.jog && idle.mdi && idle.load);
  assert.ok(!idle.hold && !idle.resume && !idle.abort);
  assert.equal(view(snapshot("idle"), null, true).enabled.run, false, "nothing loaded");
  assert.equal(view(snapshot("idle"), { errors: 2 }, true).enabled.run, false, "errors in the program");

  const running = view(snapshot("running"), prog, true).enabled;
  assert.ok(running.hold && running.abort && !running.run && !running.jog && !running.load && !running.home);
  const hold = view(snapshot("hold"), prog, true).enabled;
  assert.ok(hold.resume && hold.abort && !hold.hold);
  const homing = view(snapshot("homing"), prog, true).enabled;
  assert.ok(homing.abort && !homing.run && !homing.home);
  const alarm = view(snapshot("alarm"), prog, true).enabled;
  assert.ok(alarm.home && !alarm.run && !alarm.abort);
});

test("the view reports lost position when the prediction and the simulator disagree", () => {
  const s = snapshot("idle");
  assert.equal(view(s, null, true).drift, 0);
  s.sim = { xRadiusMm: 4, zMm: 6 };
  assert.equal(view(s, null, true).drift, 5);
});

test("commands go out in order, one at a time, and a failure does not end the stream", async () => {
  const intents$ = new rx.Subject();
  const order = [];
  const delays = { a: 30, b: 0, c: 5 };
  const request = (i) =>
    new Promise((resolve, reject) =>
      setTimeout(() => {
        order.push(i.label);
        if (i.label === "b") reject(new Error("boom"));
        else resolve({ ok: i.label !== "c", status: i.label === "c" ? 409 : 202, data: null, error: "busy" });
      }, delays[i.label]),
    );
  const results = [];
  const done = new Promise((res) =>
    commandResults(rx, intents$, request).subscribe((r) => {
      results.push(r);
      if (results.length === 3) res();
    }),
  );
  intents$.next({ label: "a" });
  intents$.next({ label: "b" });
  intents$.next({ label: "c" });
  await done;
  assert.deepEqual(order, ["a", "b", "c"], "a slow first command is not overtaken");
  assert.deepEqual(results.map((r) => [r.intent.label, r.ok, r.error]), [
    ["a", true, null],
    ["b", false, "boom"],
    ["c", false, "busy"],
  ]);
});

test("the client sends the token only with changes and surfaces the server's error", async () => {
  const calls = [];
  const fakeFetch = async (path, init) => {
    calls.push([path, init]);
    if (path === "/bad") return { ok: false, status: 401, statusText: "Unauthorized", text: async () => '{"error":"missing or wrong bearer token"}' };
    return { ok: true, status: 200, text: async () => '{"x":1}' };
  };
  const request = client(fakeFetch, () => "tok");
  const r = await request({ path: "/api/state" });
  assert.deepEqual(r.data, { x: 1 });
  assert.equal(calls[0][1].headers.Authorization, undefined);
  await request({ method: "POST", path: "/api/command", body: { kind: "run" } });
  assert.equal(calls[1][1].headers.Authorization, "Bearer tok");
  assert.equal(calls[1][1].body, '{"kind":"run"}');
  await request({ method: "PUT", path: "/api/programs/p", text: "M30\n" });
  assert.equal(calls[2][1].headers["Content-Type"], "text/plain; charset=utf-8");
  const bad = await request({ method: "POST", path: "/bad" });
  assert.equal(bad.ok, false);
  assert.equal(bad.error, "missing or wrong bearer token");
});

test("plot math: fit, round trips, zoom about the cursor, arcs and grid", () => {
  const v = fitView({ xMin: 0, xMax: 10, zMin: -50, zMax: 0 }, 1000, 500);
  assert.equal(v.zCenter, -25);
  const p = worldToScreen(-50, 10, v, 1000, 500);
  const back = screenToWorld(p.sx, p.sy, v, 1000, 500);
  assert.ok(Math.abs(back.z + 50) < 1e-9 && Math.abs(back.x - 10) < 1e-9);
  const z = zoomAt(v, 2, 100, 100, 1000, 500);
  const before = screenToWorld(100, 100, v, 1000, 500);
  const after = screenToWorld(100, 100, z, 1000, 500);
  assert.ok(Math.abs(before.z - after.z) < 1e-9 && Math.abs(before.x - after.x) < 1e-9);
  const arc = { kind: "arc", x0: 10, z0: -20, x1: 15, z1: -25, cx: 10, cz: -25, ccw: false };
  const pts = tessellateArc(arc, 10);
  assert.deepEqual(pts[0], { z: -20, x: 10 });
  assert.deepEqual(pts[pts.length - 1], { z: -25, x: 15 });
  for (const q of pts) assert.ok(Math.abs(Math.hypot(q.x - 10, q.z + 25) - 5) < 1e-9);
  assert.equal(gridPitchMm(8), 10);
  assert.equal(gridPitchMm(80), 1);
});
