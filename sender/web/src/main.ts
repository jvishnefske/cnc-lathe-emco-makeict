/**
 * Entry point: builds the layout, loads a core (WASM if available, mock as
 * fallback or on ?core=mock), wires transports to the session, and runs the
 * main loop:
 *
 *   requestAnimationFrame
 *     → drain transport bytes
 *     → session.tick(performance.now(), rx)
 *     → write step.tx to the transport
 *     → render session.state()
 */

import "./styles.css";
import { loadCore, type CoreChoice } from "./core";
import type { ProgramApi, SessionApi, VirtualMachineApi } from "./core/api";
import type { Backplot, Command, StockSpec } from "./core/types";
import { DEFAULT_PROFILE, DEFAULT_STOCK } from "./profile";
import { DEFAULT_SERIAL_SETTINGS, type Transport } from "./transport/transport";
import { LoopbackTransport } from "./transport/loopback";
import { WebSerialTransport } from "./transport/webSerial";
import { ConnectionBar, type TransportChoice } from "./ui/connectionBar";
import { ConsolePane } from "./ui/consolePane";
import { ControlsPanel } from "./ui/controls";
import { DroPanel } from "./ui/dro";
import { ProgramPane } from "./ui/programPane";
import { StatePanel } from "./ui/statePanel";
import { ToolpathView } from "./ui/toolpath";
import { WebcamPane } from "./ui/webcam";

const DEMO_NAME = "demo-part.nc";
const DEMO_GCODE = `( EMCO PC Turn 50 demo part — 14 mm bar in the ESX 25 collet )
( diameter mode, mm, absolute )
G21 G90 G7
T0101 ( station 1 — external turning )
M3 S1200
G0 X16 Z2
G0 X14.5 Z0
G1 X0 F80          ( face to centre )
G0 X14.5 Z1
G0 X12 Z1
G1 Z-28 F150       ( rough to 12 )
G0 X15
G0 Z1
G0 X10
G1 Z-15 F120       ( rough to 10 )
G0 X15
G0 Z1
G0 X9 Z1
G1 X10 Z-0.5 F100  ( chamfer )
G1 Z-14 F100
G3 X12 Z-15 R1     ( corner radius )
G1 Z-28 F120
G0 X15
G0 Z2
M5
M30
`;

async function start(): Promise<void> {
  const app = document.getElementById("app")!;
  app.textContent = "";

  // ---------------------------------------------------------------- core
  const params = new URLSearchParams(location.search);
  const choice = (params.get("core") ?? "auto") as CoreChoice;
  const { core, note: coreNote } = await loadCore(choice);

  const profile = DEFAULT_PROFILE;
  const session: SessionApi = core.createSession(profile);

  // ---------------------------------------------------------------- state
  let transport: Transport | null = null;
  let webSerial: WebSerialTransport | null = null;
  let loopbackVm: VirtualMachineApi | null = null;
  let program: ProgramApi | null = null;
  let backplot: Backplot | null = null;
  const stock: StockSpec = { ...DEFAULT_STOCK };

  // ---------------------------------------------------------------- panels
  const consolePane = new ConsolePane();

  const bar = new ConnectionBar(DEFAULT_SERIAL_SETTINGS, {
    onConnect: async (kind: TransportChoice, settings) => {
      if (kind === "loopback") {
        loopbackVm = core.createVirtualMachine(profile);
        transport = new LoopbackTransport(loopbackVm);
      } else {
        webSerial ??= new WebSerialTransport();
        transport = webSerial;
      }
      transport.onDisconnect = () => {
        // Cable pulled / device gone.
        session.setConnected?.(false);
        bar.setConnected(false);
        consolePane.note(performance.now(), "link lost (transport disconnected)");
        transport = null;
      };
      await transport.connect(settings);
      session.setConnected?.(true);
      consolePane.note(
        performance.now(),
        kind === "loopback"
          ? "connected to SIMULATED machine (in-process VirtualMachine)"
          : `connected via Web Serial — ${settings.baud} baud ${settings.dataBits}` +
            `${settings.parity[0]!.toUpperCase()}${settings.stopBits}, flow ${settings.flowControl}`,
      );
    },
    onDisconnect: async () => {
      const t = transport;
      transport = null;
      if (t) {
        t.onDisconnect = undefined;
        await t.disconnect();
      }
      session.setConnected?.(false);
      consolePane.note(performance.now(), "disconnected");
    },
    onPickPort: async () => {
      webSerial ??= new WebSerialTransport();
      await webSerial.pickPort();
      return webSerial.portLabel;
    },
    onInjectFault: (kind) => {
      loopbackVm?.injectFault(kind as Parameters<VirtualMachineApi["injectFault"]>[0]);
      consolePane.note(performance.now(), `fault injected: ${kind} (training)`);
    },
  });
  bar.setCoreNote(coreNote);

  const dro = new DroPanel();
  const statePanel = new StatePanel();
  const controls = new ControlsPanel({
    onCommand: (cmd: Command) => {
      session.command(cmd);
      if (cmd.kind === "abort") {
        consolePane.note(
          performance.now(),
          "E-STOP (software) — advisory only; the real stop is the mains switch",
        );
      }
      if (cmd.kind === "run") toolpath.resetProgress();
    },
  });
  const webcam = new WebcamPane();

  const applyProgramText = (name: string, text: string): void => {
    program = core.parseProgram(text);
    const lines: string[] = [];
    for (let i = 0; i < program.lineCount(); i++) lines.push(program.line(i));
    programPane.setProgram(name, lines, program.diagnostics());
    backplot = program.backplot(stock);
    toolpath.setBackplot(backplot, stock);
    session.loadProgram(program);
    consolePane.note(
      performance.now(),
      `program loaded: ${name} (${lines.length} lines, ` +
        `${program.diagnostics().length} diagnostics)`,
    );
  };

  const programPane = new ProgramPane(stock, {
    onProgramText: applyProgramText,
    onLoadDemo: () => applyProgramText(DEMO_NAME, DEMO_GCODE),
    onStockChange: (diameterMm, lengthMm) => {
      stock.diameterMm = diameterMm;
      stock.lengthMm = lengthMm;
      backplot = program ? program.backplot(stock) : null;
      toolpath.setBackplot(backplot, stock);
    },
  });

  // ---------------------------------------------------------------- layout
  const main = document.createElement("div");
  main.className = "main-grid";

  const areaProg = document.createElement("div");
  areaProg.className = "area-prog";
  areaProg.appendChild(programPane.el);

  const areaPlot = document.createElement("div");
  areaPlot.className = "area-plot";
  const plotPanel = document.createElement("section");
  plotPanel.className = "panel plot-panel";
  const canvas = document.createElement("canvas");
  canvas.className = "plot-canvas";
  plotPanel.appendChild(canvas);
  areaPlot.appendChild(plotPanel);

  const plotTools = document.createElement("div");
  plotTools.className = "plot-tools";
  const fitBtn = document.createElement("button");
  fitBtn.className = "btn btn-small";
  fitBtn.textContent = "Fit";
  plotTools.appendChild(fitBtn);
  areaPlot.appendChild(plotTools);

  const plotHint = document.createElement("div");
  plotHint.className = "plot-hint";
  plotHint.textContent = "drag = pan · wheel = zoom · double-click = fit · ←chuck  Z−|Z+ tailstock→";
  areaPlot.appendChild(plotHint);

  // "NOT REFERENCED" banner — the machine loses its reference at every
  // power-off (no absolute position memory, no limit switches). Homing must
  // be the obvious first action after connecting.
  const refBanner = document.createElement("div");
  refBanner.className = "ref-banner";
  refBanner.hidden = true;
  refBanner.innerHTML = `
    <span class="ref-title">NOT REFERENCED</span>
    <span class="ref-text">The machine has no absolute position memory and no limit
    switches — the reference is lost at every power-off, and the displayed position is
    a guess until you home. Home first, or the next move can crash the machine.</span>`;
  const bannerHome = document.createElement("button");
  bannerHome.className = "btn btn-primary btn-home";
  bannerHome.textContent = "⌂ Home now";
  bannerHome.addEventListener("click", () => session.command({ kind: "home" }));
  refBanner.appendChild(bannerHome);
  areaPlot.appendChild(refBanner);

  const areaSide = document.createElement("div");
  areaSide.className = "area-side";
  areaSide.append(dro.el, controls.el, statePanel.el, webcam.el);

  const areaCons = document.createElement("div");
  areaCons.className = "area-cons";
  areaCons.appendChild(consolePane.el);

  main.append(areaProg, areaPlot, areaSide, areaCons);
  app.append(bar.el, main);

  const toolpath = new ToolpathView(canvas);
  fitBtn.addEventListener("click", () => toolpath.fit());
  toolpath.setBackplot(null, stock);

  // ---------------------------------------------------------------- loop
  const EMPTY = new Uint8Array(0);
  const frame = (): void => {
    const now = performance.now();

    // 1. drain transport bytes
    const rx = transport?.connected ? transport.drain(now) : EMPTY;

    // 2. pump the session
    const step = session.tick(now, rx);

    // 3. write outgoing bytes
    if (step.tx.length > 0 && transport?.connected) transport.send(step.tx);

    // 4. events → console
    for (const ev of step.events) {
      switch (ev.kind) {
        case "raw":
          consolePane.push({ atMs: ev.atMs, kind: ev.dir, bytes: ev.bytes });
          break;
        case "alarm":
          consolePane.note(ev.atMs, `ALARM ${ev.code}: ${ev.message}`);
          break;
        case "state":
          consolePane.note(ev.atMs, `state ${ev.from} → ${ev.to}`);
          break;
        default:
          break; // sent/acked are visible as raw tx/rx already
      }
    }
    consolePane.flush();

    // 5. render state
    const s = session.state();
    dro.update(s.dro);
    statePanel.update(s);
    programPane.setCurrentLine(s.currentLine);
    controls.update(s, transport?.connected ?? false, program !== null);
    const connected = transport?.connected ?? false;
    const needRef = connected && !s.dro.homed && profile.requiresReferenceAfterPowerOn;
    if (refBanner.hidden === needRef) refBanner.hidden = !needRef;
    toolpath.update(s.dro, s.currentLine, s.run === "running");
    toolpath.render();

    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
}

void start().catch((err: unknown) => {
  const app = document.getElementById("app");
  if (app) {
    app.textContent =
      "Failed to start: " + (err instanceof Error ? err.message : String(err));
  }
});
