import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";

const desktop = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const workspace = path.resolve(desktop, "../..");
const names = ["nsbctl", "nsb-mcp"];

function run(command, args, capture = false) {
  const result = spawnSync(command, args, {
    cwd: workspace, shell: false, windowsHide: true,
    stdio: capture ? ["ignore", "pipe", "inherit"] : "inherit", encoding: "utf8",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed (${result.signal || result.status})`);
  return result.stdout?.trim();
}

function option(args, names) {
  for (let i = 0; i < args.length; i++) {
    for (const name of names) {
      if (args[i] === name) {
        if (!args[i + 1] || args[i + 1].startsWith("-")) throw new Error(`${name} requires a value`);
        return args[i + 1];
      }
      if (args[i].startsWith(`${name}=`)) return args[i].slice(name.length + 1);
    }
  }
}

export function toolBuildPlan(args, host) {
  const separator = args.indexOf("--");
  const tauriArgs = separator < 0 ? args : args.slice(0, separator);
  const target = option(tauriArgs, ["--target", "-t"]) || host;
  if (!/^[a-z0-9_]+(?:-[a-z0-9_]+){2,}$/.test(target)) throw new Error(`Unsupported target: ${target}`);
  const profile = option(args, ["--profile"]) || (tauriArgs.some(arg => arg === "--debug" || arg === "-d") ? "dev" : "release");
  if (!/^[a-zA-Z0-9_-]+$/.test(profile)) throw new Error(`Invalid profile: ${profile}`);
  return { target, profile, directory: profile === "dev" ? "debug" : profile,
    targets: target === "universal-apple-darwin" ? ["aarch64-apple-darwin", "x86_64-apple-darwin"] : [target] };
}

function prepareTools(args) {
  const host = run("rustc", ["--print", "host-tuple"], true);
  const plan = toolBuildPlan(args, host);
  if (plan.targets.length > 1 && process.platform !== "darwin") throw new Error("Universal macOS tools must be built on macOS");
  const metadata = JSON.parse(run("cargo", ["metadata", "--format-version", "1", "--no-deps", "--locked"], true));
  const destination = path.join(desktop, "src-tauri", "binaries");
  fs.mkdirSync(destination, { recursive: true });
  for (const target of plan.targets) {
    run("cargo", ["build", "--locked", "-p", "nsb-core", "--bin", "nsbctl", "--bin", "nsb-mcp", "--target", target, "--profile", plan.profile]);
  }
  for (const name of names) {
    const extension = plan.target.includes("windows") ? ".exe" : "";
    const output = path.join(destination, `${name}-${plan.target}${extension}`);
    const sources = plan.targets.map(target => path.join(metadata.target_directory, target, plan.directory, `${name}${extension}`));
    if (sources.some(source => !fs.statSync(source).isFile() || fs.statSync(source).size === 0)) throw new Error(`Missing tool binary: ${name}`);
    if (sources.length === 1) fs.copyFileSync(sources[0], output);
    else run("lipo", ["-create", ...sources, "-output", output]);
    if (!extension) fs.chmodSync(output, 0o755);
  }
  process.stdout.write(`Prepared ${names.join(" / ")} for ${plan.target} (${plan.profile})\n`);
}

export function withToolConfig(args) {
  const separator = args.indexOf("--");
  const at = separator < 0 ? args.length : separator;
  const config = JSON.stringify({ bundle: { externalBin: names.map(name => `binaries/${name}`) } });
  return [...args.slice(0, at), "--config", config, ...args.slice(at)];
}

function main(args) {
  if (args[0] === "prepare-tools") { prepareTools(args.slice(1)); return; }
  const bundle = ["build", "bundle"].includes(args[0]) && !args.some(arg => ["--help", "-h", "--version", "-V"].includes(arg));
  if (bundle) { prepareTools(args.slice(1)); args = withToolConfig(args); }
  const require = createRequire(import.meta.url);
  const cliPackage = require.resolve("@tauri-apps/cli/package.json");
  const cli = path.resolve(path.dirname(cliPackage), JSON.parse(fs.readFileSync(cliPackage, "utf8")).bin.tauri);
  const result = spawnSync(process.execPath, [cli, ...args], { cwd: desktop, stdio: "inherit", shell: false, windowsHide: true });
  if (result.error) throw result.error;
  if (result.signal) throw new Error(`Tauri stopped: ${result.signal}`);
  process.exitCode = result.status ?? 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(process.argv.slice(2)); }
  catch (error) { process.stderr.write(`${error.message}\n`); process.exitCode = 1; }
}
