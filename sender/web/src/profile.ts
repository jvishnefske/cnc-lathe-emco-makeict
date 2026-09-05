/**
 * Default machine profile for the EMCO PC Turn 50, from the machine's own
 * manuals and schematics (docs/_machine-facts.md).
 *
 * Serial settings are for a USB-RS485 adapter and are placeholders: the wire
 * protocol of the X300 RS-485 link is NOT yet known. The 9600/7E1 in the
 * binders is the DOS PC's file-transfer COM port, not the machine link.
 */

import type { MachineProfile } from "./core/types";

export const DEFAULT_PROFILE: MachineProfile = {
  name: "EMCO PC Turn 50",
  xMinMm: 0,
  xMaxMm: 48, // usable X travel, radius [MANUAL]
  zMinMm: -228, // usable Z travel 228 mm (275 mechanical) [MANUAL]
  zMaxMm: 0,
  rapidMmMin: 750, // rapid traverse [MANUAL]
  maxFeedMmMin: 750, // working feed range 0–750 (1500 absolute max) [MANUAL]
  accelMmSec2: 100, // UNVERIFIED
  spindleMinRpm: 130,
  spindleMaxRpm: 3000, // via three belt steps: 130–1300 / 200–2000 / 300–3000
  turretStations: 6, // 3 external + 3 internal [MANUAL]
  turretIndexSeconds: 2, // UNVERIFIED
  serial: {
    baud: 9600,
    dataBits: 8,
    stopBits: 1,
    parity: "none",
    flowControl: "none",
  },
  dialect: "plain-gcode",
};

/** Belt steps: spindle range is set by physically moving the belt. */
export const BELT_STEPS = [
  { label: "I · 130–1300 rpm", min: 130, max: 1300 },
  { label: "II · 200–2000 rpm", min: 200, max: 2000 },
  { label: "III · 300–3000 rpm", min: 300, max: 3000 },
] as const;

/** ESX 25 collets take 1.5–14 mm stock, so default to 14 mm bar. */
export const DEFAULT_STOCK = {
  diameterMm: 14,
  lengthMm: 60,
  zFaceMm: 0,
};

/** Threading (G33) is synchronised only up to 400 rpm on this machine. */
export const MAX_THREADING_RPM = 400;
