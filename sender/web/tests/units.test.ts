import { describe, expect, it } from "vitest";
import { formatFeed, formatInch, formatMm, inchToMm, mmToInch } from "../src/units";

describe("unit conversion", () => {
  it("converts mm to inch and back", () => {
    expect(mmToInch(25.4)).toBeCloseTo(1, 12);
    expect(inchToMm(1)).toBeCloseTo(25.4, 12);
    expect(inchToMm(mmToInch(12.345))).toBeCloseTo(12.345, 12);
  });
});

describe("DRO formatting", () => {
  it("formats mm at microns", () => {
    expect(formatMm(12.3456)).toBe("12.346");
    expect(formatMm(-0.001)).toBe("-0.001");
  });

  it("never shows negative zero", () => {
    expect(formatMm(-0)).toBe("0.000");
    expect(formatInch(-0)).toBe("0.0000");
  });

  it("formats inch at tenths", () => {
    expect(formatInch(25.4)).toBe("1.0000");
    expect(formatInch(12.7)).toBe("0.5000");
  });

  it("formats feed with both units", () => {
    expect(formatFeed(750)).toBe("750 mm/min (29.5 in/min)");
  });
});
