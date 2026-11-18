// The browser sender's fallback profile against the one source of the
// machine's numbers: cnc/model's PhysicalConfig::pc_turn_50, as its test
// writes it to cnc/model/fixtures/pc_turn_50.json.

import { describe, expect, it } from "vitest";
import fixture from "../../../cnc/model/fixtures/pc_turn_50.json";
import { DEFAULT_PROFILE } from "../src/profile";

describe("DEFAULT_PROFILE", () => {
  it("has exactly the core's physical numbers", () => {
    const { link: _link, dialect: _dialect, ...physical } = DEFAULT_PROFILE;
    expect(physical).toEqual(fixture);
  });
});
