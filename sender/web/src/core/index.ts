/** Runtime selection between the real WASM core and the TypeScript mock. */

import type { CoreApi } from "./api";
import { MockCore } from "./mockCore";
import { loadWasmCore } from "./wasmCore";

export type CoreChoice = "auto" | "wasm" | "mock";

export async function loadCore(choice: CoreChoice): Promise<{ core: CoreApi; note: string }> {
  if (choice === "mock") {
    return { core: new MockCore(), note: "mock core (selected)" };
  }
  try {
    const core = await loadWasmCore();
    return { core, note: "wasm core" };
  } catch (err) {
    if (choice === "wasm") throw err;
    return {
      core: new MockCore(),
      note: "mock core (wasm build not found — run wasm-pack in sender/core for the real one)",
    };
  }
}
