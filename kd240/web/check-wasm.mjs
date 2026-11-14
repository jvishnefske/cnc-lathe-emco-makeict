// Instantiates a built screen (its wasm with its own JS glue) under Node, so
// build.sh refuses a bundle that cannot start. It does not draw anything:
// with no document the screen's start() ends in its error path.
//
//   node kd240/web/check-wasm.mjs kd240/web/dist
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { resolve } from "node:path";

const dir = resolve(process.argv[2] ?? "dist");
const bytes = readFileSync(resolve(dir, "emco_egui_bg.wasm"));
const mod = await WebAssembly.compile(bytes);
const { default: init } = await import(pathToFileURL(resolve(dir, "emco_egui.js")).href);
const out = await init({ module_or_path: bytes });
await new Promise((r) => setTimeout(r, 100));
console.log(
  `check-wasm: ${dir}: compiled (${WebAssembly.Module.imports(mod).length} imports), ` +
    `instantiated (${out.memory.buffer.byteLength / 65536} pages), start() ran`,
);
