// The UI's data flow, as RxJS streams. No DOM here: every function takes the
// RxJS namespace (`rx`) and plain inputs, so test/streams.test.mjs runs it
// under Node with a fake EventSource and fetch.
//
//   server-sent events ──► feed$ ──┬─► state$       (latest snapshot)
//                                  ├─► connected$   (is the link up?)
//                                  └─► log$         (the session's events)
//   state$ + loaded$ + connected$ ──► view(...)     (what each control may do)
//   intents (clicks) ──► commandResults(...) ──► toasts

/** The SSE feed as one hot stream of `{ type, data }`. */
export function feed(rx, makeEventSource, url = "/api/events") {
  const { Observable, share } = rx;
  return new Observable((sub) => {
    const es = makeEventSource(url);
    const on = (type, parse) =>
      es.addEventListener(type, (e) => {
        try {
          sub.next({ type, data: parse(e.data) });
        } catch (err) {
          sub.next({ type: "bad", data: String(err) });
        }
      });
    on("state", JSON.parse);
    on("core", JSON.parse);
    on("lagged", Number);
    // EventSource reconnects by itself; report the link both ways.
    es.onopen = () => sub.next({ type: "conn", data: true });
    es.onerror = () => sub.next({ type: "conn", data: false });
    return () => es.close();
  }).pipe(share());
}

/** The latest machine snapshot, replayed to late subscribers. */
export function stateOf(rx, feed$) {
  const { filter, map, shareReplay } = rx;
  return feed$.pipe(
    filter((e) => e.type === "state"),
    map((e) => e.data),
    shareReplay({ bufferSize: 1, refCount: true }),
  );
}

/** True while the event link is up. */
export function connectedOf(rx, feed$) {
  const { filter, map, startWith, distinctUntilChanged } = rx;
  return feed$.pipe(
    filter((e) => e.type === "conn" || e.type === "state"),
    map((e) => (e.type === "conn" ? e.data : true)),
    startWith(false),
    distinctUntilChanged(),
  );
}

/** One log line for a session event (or null for events not logged). */
export function logLine(ev, showRaw = false) {
  const t = (ms) => (ms / 1000).toFixed(3).padStart(9);
  switch (ev.kind) {
    case "sent":
      return `${t(ev.atMs)}  → L${ev.line + 1}  ${ev.text}`;
    case "acked":
      return `${t(ev.atMs)}  ✓ L${ev.line + 1}`;
    case "alarm":
      return `${t(ev.atMs)}  ALARM ${ev.code}: ${ev.message}`;
    case "state":
      return `${t(ev.atMs)}  ${ev.from} → ${ev.to}`;
    case "raw":
      if (!showRaw) return null;
      return `${t(ev.atMs)}  ${ev.dir} ${ev.bytes.map((b) => b.toString(16).padStart(2, "0")).join(" ")}`;
    default:
      return null;
  }
}

/** The last `max` log lines; `showRaw$` toggles the byte-level lines. */
export function logOf(rx, feed$, showRaw$, max = 400) {
  const { filter, withLatestFrom, map, scan, startWith } = rx;
  return feed$.pipe(
    filter((e) => e.type === "core" || e.type === "lagged"),
    withLatestFrom(showRaw$),
    map(([e, raw]) => (e.type === "lagged" ? `… ${e.data} events missed (slow link)` : logLine(e.data, raw))),
    filter((line) => line !== null),
    scan((lines, line) => {
      const next = lines.length >= max ? lines.slice(lines.length - max + 1) : lines.slice();
      next.push(line);
      return next;
    }, []),
    startWith([]),
  );
}

const ACTIVE = ["running", "hold", "homing", "toolchange"];

/**
 * What each control may do, from the snapshot, the loaded program and the
 * link. Pure, so the rules are tested rather than scattered over handlers.
 * The core ignores a command its state does not allow anyway (`run` is only
 * taken from idle); this keeps the operator from pressing a dead button.
 */
export function view(snapshot, loaded, connected) {
  const s = snapshot?.session;
  const run = connected && s ? s.run : "disconnected";
  const idle = run === "idle";
  const hasProgram = !!loaded && loaded.errors === 0;
  return {
    run,
    enabled: {
      run: idle && hasProgram,
      hold: run === "running",
      resume: run === "hold",
      abort: ACTIVE.includes(run),
      home: idle || run === "alarm",
      jog: idle,
      mdi: idle,
      load: connected && !ACTIVE.includes(run),
      fault: connected,
    },
    currentLine: s ? s.currentLine : -1,
    alarms: s ? s.alarms : [],
    homed: !!s?.dro?.homed,
    // The predicted DRO and the simulator's truth disagree after a stall:
    // that difference is what the training scenarios are about.
    drift:
      s && snapshot.sim ? Math.hypot(s.dro.xRadiusMm - snapshot.sim.xRadiusMm, s.dro.zMm - snapshot.sim.zMm) : 0,
  };
}

/**
 * Runs each intent's request in order (`concatMap`: commands to a machine
 * must not overtake each other) and turns every outcome into a result for
 * the toast line. A failed request never ends the stream.
 */
export function commandResults(rx, intents$, request) {
  const { concatMap, from, map, catchError, of } = rx;
  return intents$.pipe(
    concatMap((intent) =>
      from(request(intent)).pipe(
        map((res) => ({ intent, ok: res.ok, status: res.status, data: res.data, error: res.ok ? null : res.error })),
        catchError((err) => of({ intent, ok: false, status: 0, data: null, error: String(err?.message ?? err) })),
      ),
    ),
  );
}

/** A fetch wrapper: JSON in and out, the bearer token on every change. */
export function client(fetchFn, token) {
  return async function request({ method = "GET", path, body, text }) {
    const headers = {};
    const t = token();
    if (method !== "GET" && t) headers.Authorization = `Bearer ${t}`;
    let payload;
    if (text !== undefined) {
      headers["Content-Type"] = "text/plain; charset=utf-8";
      payload = text;
    } else if (body !== undefined) {
      headers["Content-Type"] = "application/json";
      payload = JSON.stringify(body);
    }
    const res = await fetchFn(path, { method, headers, body: payload });
    const raw = await res.text();
    let data = null;
    try {
      data = raw ? JSON.parse(raw) : null;
    } catch {
      data = raw;
    }
    const error = res.ok ? null : (data && data.error) || `${res.status} ${res.statusText || ""}`.trim();
    return { ok: res.ok, status: res.status, data, error };
  };
}

/** Polls `request(req)` every `ms` and whenever `refresh$` fires. */
export function polled(rx, request, req, ms, refresh$) {
  const { merge, timer, switchMap, from, map, catchError, of, shareReplay } = rx;
  return merge(timer(0, ms), refresh$).pipe(
    switchMap(() => from(request(req)).pipe(catchError((e) => of({ ok: false, error: String(e) })))),
    map((r) => (r.ok ? r.data : null)),
    shareReplay({ bufferSize: 1, refCount: true }),
  );
}
