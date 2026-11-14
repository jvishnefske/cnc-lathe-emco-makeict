// The page: DOM in, streams.js in the middle, DOM out. Every user action is
// an intent object; intents go to the server in order and come back as
// results for the toast line and for the few views that depend on them.
import { client, commandResults, connectedOf, feed, logOf, polled, stateOf, view } from "./streams.js";
import { ToolpathView } from "./toolpath.js";

const rx = globalThis.rxjs;
const { BehaviorSubject, Subject, combineLatest, fromEvent, merge, from, map, filter, startWith, switchMap,
  shareReplay, distinctUntilChanged, tap } = rx;
const $ = (id) => document.getElementById(id);

// ------------------------------------------------------------- the token
const TOKEN_KEY = "emco-kd240-token";
let token = "";
try {
  token = localStorage.getItem(TOKEN_KEY) || "";
} catch {
  /* private window: the token lives only in this page */
}
$("token").value = token;
fromEvent($("token"), "change").subscribe(() => {
  token = $("token").value.trim();
  try {
    localStorage.setItem(TOKEN_KEY, token);
  } catch {
    /* ignore */
  }
});
const request = client((...a) => fetch(...a), () => token);

// ------------------------------------------------------------- streams
const feed$ = feed(rx, (url) => new EventSource(url));
const state$ = stateOf(rx, feed$);
const connected$ = connectedOf(rx, feed$).pipe(shareReplay({ bufferSize: 1, refCount: true }));
const loaded$ = new BehaviorSubject(null);
const refreshPrograms$ = new Subject();
const refreshCaptures$ = new Subject();
const raw$ = fromEvent($("raw"), "change").pipe(
  map((e) => e.target.checked),
  startWith(false),
  shareReplay({ bufferSize: 1, refCount: true }),
);
const view$ = combineLatest([state$.pipe(startWith(null)), loaded$, connected$]).pipe(
  map(([s, l, c]) => view(s, l, c)),
  shareReplay({ bufferSize: 1, refCount: true }),
);

from(request({ path: "/api/program" })).subscribe((r) => r.ok && r.data && loaded$.next(r.data));

// ------------------------------------------------------------- intents
const click = (el, make) => fromEvent(el, "click").pipe(map(make));
const command = (body, label) => ({ method: "POST", path: "/api/command", body, label: label ?? body.kind });

const cmdButtons = [...document.querySelectorAll("[data-cmd]")];
const faultButtons = [...document.querySelectorAll("[data-fault]")];
const jogButtons = [...document.querySelectorAll("[data-jog]")];

const programName = () => $("name").value.trim();
const intents$ = merge(
  ...cmdButtons.map((b) => click(b, () => command({ kind: b.dataset.cmd }))),
  fromEvent($("single"), "change").pipe(map((e) => command({ kind: "singleBlock", on: e.target.checked }))),
  fromEvent($("dry"), "change").pipe(map((e) => command({ kind: "dryRun", on: e.target.checked }))),
  ...jogButtons.map((b) =>
    click(b, () => {
      const [axis, sign] = [b.dataset.jog[0], b.dataset.jog[1] === "+" ? 1 : -1];
      return command(
        { kind: "jog", axis, deltaMm: sign * Number($("jog-step").value), feedMmMin: Number($("jog-feed").value) },
        `jog ${b.dataset.jog}`,
      );
    }),
  ),
  fromEvent($("mdi-form"), "submit").pipe(
    tap((e) => e.preventDefault()),
    map(() => $("mdi").value.trim()),
    filter((text) => text.length > 0),
    map((text) => command({ kind: "mdi", text }, `MDI ${text}`)),
  ),
  ...faultButtons.map((b) =>
    click(b, () => ({ method: "POST", path: "/api/sim/fault", body: { kind: b.dataset.fault }, label: `fault ${b.dataset.fault}` })),
  ),
  click($("save"), () => ({ method: "PUT", path: `/api/programs/${encodeURIComponent(programName())}`, text: $("editor").value, label: "save", tag: "save" })),
  click($("load"), () => ({ method: "POST", path: `/api/programs/${encodeURIComponent(programName())}/load`, label: "load", tag: "load" })),
  click($("delete"), () => ({ method: "DELETE", path: `/api/programs/${encodeURIComponent(programName())}`, label: "delete", tag: "delete" })),
  click($("cap-start"), () => ({
    method: "POST",
    path: "/api/capture/start",
    body: { baud: Number($("cap-baud").value), framing: $("cap-framing").value },
    label: "capture start",
    tag: "capture",
  })),
  click($("cap-stop"), () => ({ method: "POST", path: "/api/capture/stop", label: "capture stop", tag: "capture" })),
);

const results$ = commandResults(rx, intents$, request).pipe(shareReplay({ bufferSize: 1, refCount: true }));

results$.subscribe((r) => {
  toast(r.ok ? `${r.intent.label}: ok` : `${r.intent.label}: ${r.error}`, !r.ok);
  switch (r.intent.tag) {
    case "save":
    case "delete":
      refreshPrograms$.next();
      break;
    case "load":
      // A program with errors comes back 422 with its diagnostics.
      if (r.data && r.data.diagnostics) showDiagnostics(r.data.diagnostics);
      if (r.ok) loaded$.next(r.data);
      break;
    case "capture":
      refreshCaptures$.next();
      break;
  }
});

// ------------------------------------------------------------- programs
const programs$ = polled(rx, request, { path: "/api/programs" }, 15000, refreshPrograms$);
programs$.subscribe((list) => {
  const sel = $("programs");
  const keep = sel.value;
  sel.replaceChildren(new Option("— stored programs —", ""), ...(list || []).map((p) => new Option(`${p.name} (${p.bytes} B)`, p.name)));
  sel.value = (list || []).some((p) => p.name === keep) ? keep : "";
});
fromEvent($("programs"), "change")
  .pipe(
    map((e) => e.target.value),
    filter((name) => name !== ""),
    switchMap((name) => from(request({ path: `/api/programs/${encodeURIComponent(name)}` })).pipe(map((r) => [name, r]))),
  )
  .subscribe(([name, r]) => {
    if (!r.ok) return toast(`open ${name}: ${r.error}`, true);
    $("name").value = name;
    $("editor").value = typeof r.data === "string" ? r.data : String(r.data ?? "");
  });
fromEvent($("file"), "change")
  .pipe(
    map((e) => e.target.files?.[0]),
    filter(Boolean),
    switchMap((f) => from(f.text()).pipe(map((text) => [f.name, text]))),
  )
  .subscribe(([fname, text]) => {
    $("name").value = safeName(fname.replace(/\.[^.]*$/, ""));
    $("editor").value = text;
  });

/** The store's name rule: letters, digits, . _ -, starting alphanumeric. */
function safeName(s) {
  const cleaned = s.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/\.\.+/g, ".").replace(/^[^A-Za-z0-9]+/, "");
  return cleaned.slice(0, 64) || "program";
}

function showDiagnostics(diags) {
  $("diagnostics").replaceChildren(
    ...diags.map((d) => {
      const li = document.createElement("li");
      li.className = d.severity;
      li.textContent = `L${d.line + 1}:${d.column + 1} ${d.severity}: ${d.message}`;
      return li;
    }),
  );
}

// ------------------------------------------------------------- loaded program
const plot = new ToolpathView($("plot"));
loaded$.subscribe((doc) => {
  $("loaded-name").textContent = doc ? doc.name : "—";
  $("listing").replaceChildren(
    ...(doc?.lines || []).map((text) => {
      const li = document.createElement("li");
      li.textContent = text || " ";
      return li;
    }),
  );
  if (doc) showDiagnostics(doc.diagnostics || []);
  plot.setBackplot(doc ? doc.backplot : null);
});

// ------------------------------------------------------------- machine state
const fixed = (v, n = 3) => (typeof v === "number" ? v.toFixed(n) : "—");
state$.subscribe((snap) => {
  const s = snap.session;
  $("st-tool").textContent = s.tool ? `T${s.tool}` : "unknown";
  $("st-spindle").textContent = s.spindleOn ? `${Math.round(s.spindleRpm)} rpm` : "off";
  $("st-feed").textContent = `${Math.round(s.feedMmMin)} mm/min`;
  $("st-modal").textContent = s.modal.join(" ");
  $("dro-xd").textContent = fixed(s.dro.xDiameterMm);
  $("dro-xr").textContent = fixed(s.dro.xRadiusMm);
  $("dro-z").textContent = fixed(s.dro.zMm);
  $("dro-work").textContent = `${fixed(s.dro.xWorkMm * 2)} / ${fixed(s.dro.zWorkMm)}`;
  $("dro-homed").textContent = s.dro.homed ? "yes" : "NO — home before running";
  $("single").checked = s.singleBlock;
  $("dry").checked = s.dryRun;
  $("alarms").replaceChildren(
    ...s.alarms.map((a) => {
      const li = document.createElement("li");
      li.textContent = `${a.code}: ${a.message}`;
      return li;
    }),
  );
  plot.update(s.dro, snap.sim, s.currentLine, s.run === "running" || s.run === "hold");
});

connected$.subscribe((up) => {
  $("conn").textContent = up ? "connected" : "disconnected";
  $("conn").className = `badge ${up ? "on" : "off"}`;
});

view$.subscribe((v) => {
  $("run").textContent = v.run;
  $("run").className = `run ${v.run}`;
  $("st-line").textContent = v.currentLine >= 0 ? `L${v.currentLine + 1}` : "—";
  for (const b of cmdButtons) b.disabled = !v.enabled[b.dataset.cmd];
  for (const b of jogButtons) b.disabled = !v.enabled.jog;
  for (const b of faultButtons) b.disabled = !v.enabled.fault;
  $("mdi").disabled = !v.enabled.mdi;
  $("load").disabled = !v.enabled.load;
  $("drift-row").hidden = v.drift < 0.01;
  $("dro-drift").textContent = `${v.drift.toFixed(3)} mm`;
});

view$
  .pipe(
    map((v) => v.currentLine),
    distinctUntilChanged(),
  )
  .subscribe((line) => {
    const items = $("listing").children;
    for (const li of items) li.classList.remove("current");
    const cur = items[line];
    if (cur) {
      cur.classList.add("current");
      cur.scrollIntoView({ block: "nearest" });
    }
  });

// ------------------------------------------------------------- log
logOf(rx, feed$, raw$).subscribe((lines) => {
  const el = $("log");
  const atEnd = el.scrollTop + el.clientHeight >= el.scrollHeight - 4;
  el.textContent = lines.join("\n");
  if (atEnd) el.scrollTop = el.scrollHeight;
});

// ------------------------------------------------------------- capture
polled(rx, request, { path: "/api/capture" }, 1000, refreshCaptures$).subscribe((c) => {
  if (!c) return;
  const sel = $("cap-baud");
  if (sel.options.length === 0) {
    sel.replaceChildren(...c.baudRates.map((b) => new Option(String(b), String(b))));
    sel.value = "9600";
  }
  const s = c.status;
  $("cap-start").disabled = s.running || !s.port;
  $("cap-stop").disabled = !s.running;
  $("cap-status").textContent = !s.port
    ? "no RS-485 port configured (start the service with --rs485 /dev/tty…)"
    : s.running
      ? `capturing on ${s.port} at ${s.baud} ${s.framing}: ${s.rxBytes} bytes, ${s.errorBytes} with UART errors → ${s.file}`
      : `idle on ${s.port}${s.error ? ` — last error: ${s.error}` : ""}`;
});
polled(rx, request, { path: "/api/captures" }, 5000, refreshCaptures$).subscribe((list) => {
  $("captures").replaceChildren(
    ...(list || []).map((c) => {
      const li = document.createElement("li");
      const a = document.createElement("a");
      a.href = `/api/captures/${encodeURIComponent(c.name)}`;
      a.textContent = c.name;
      li.append(a, ` ${c.bytes} B`);
      return li;
    }),
  );
});

// ------------------------------------------------------------- toast + render loop
let toastTimer = 0;
function toast(text, bad) {
  const el = $("toast");
  el.textContent = text;
  el.className = `toast show${bad ? " bad" : ""}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.className = "toast"), bad ? 5000 : 1500);
}

(function frame() {
  plot.render();
  requestAnimationFrame(frame);
})();
