import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = fileURLToPath(new URL("../", import.meta.url));
const contract = JSON.parse(await readFile(path.join(root, "docs/native-command-contract.json"), "utf8"));
if (!Array.isArray(contract.commands) || contract.commands.length !== 328 || new Set(contract.commands).size !== 328) {
  throw new Error("The v1.0.4 command contract must retain all 328 commands");
}
const names = [...contract.commands].sort();
const implemented = new Set();
for (const file of await readdir(path.join(root, "internal/core"))) {
  if (!file.endsWith(".go")) continue;
  const source = await readFile(path.join(root, "internal/core", file), "utf8");
  for (const match of source.matchAll(/c\.handle\("([a-z_]+)"/g)) implemented.add(match[1]);
}
const bridge = await readFile(path.join(root, "internal/desktop/bridge.go"), "utf8");
for (const match of bridge.matchAll(/case "([a-z_]+)":/g)) implemented.add(match[1]);
const registered = names.filter((name) => implemented.has(name));
const pending = names.filter((name) => !implemented.has(name));
const report = { legacyCommands: names.length, goRegistered: registered.length, pendingCount: pending.length, registered, pending };
console.log(JSON.stringify(report, null, 2));
// Presence is necessary, not proof of behavioural parity. Manual validation is
// still required for commands with side effects, platform ownership or recovery.
if (process.argv.includes("--require-complete") && pending.length) process.exitCode = 1;
