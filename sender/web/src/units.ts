/** mm <-> inch conversion and DRO formatting. The core is metric; inch is display-only. */

export const MM_PER_INCH = 25.4;

export function mmToInch(mm: number): number {
  return mm / MM_PER_INCH;
}

export function inchToMm(inch: number): number {
  return inch * MM_PER_INCH;
}

/** Fixed-width metric readout, e.g. "-123.456". */
export function formatMm(mm: number, digits = 3): string {
  const v = Object.is(mm, -0) ? 0 : mm;
  return v.toFixed(digits);
}

/** Inch readout at tenths (0.0001") like an imperial DRO. */
export function formatInch(mm: number, digits = 4): string {
  const v = mmToInch(Object.is(mm, -0) ? 0 : mm);
  return v.toFixed(digits);
}

export function formatFeed(mmMin: number): string {
  return `${mmMin.toFixed(0)} mm/min (${mmToInch(mmMin).toFixed(1)} in/min)`;
}
