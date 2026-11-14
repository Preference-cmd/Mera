import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const cargo = process.env.CARGO || "cargo";
const rustc = process.env.RUSTC || "rustc";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(scriptDir, "../..");
const manifestPath = path.join(workspaceRoot, "Cargo.toml");

function detectHostTriple(): string {
  const result = spawnSync(rustc, ["-vV"], { cwd: workspaceRoot, encoding: "utf8" });
  if (result.error) {
    console.error(`failed to run ${rustc} -vV:`, result.error.message);
    process.exit(1);
  }
  if (result.status !== 0) {
    if (result.stderr) {
      process.stderr.write(result.stderr);
    }
    process.exit(result.status ?? 1);
  }
  const hostLine = result.stdout.split(/\r?\n/).find((line) => line.startsWith("host:"));
  if (!hostLine) {
    console.error(`could not parse host triple from ${rustc} -vV output`);
    process.exit(1);
  }
  return hostLine.slice("host:".length).trim();
}

const hostTriple = detectHostTriple();
const targetTriple = process.env.TAURI_ENV_TARGET_TRIPLE || hostTriple;
const isDebug = process.argv.includes("--dev") || process.env.TAURI_ENV_DEBUG === "true";
const profile = isDebug ? "debug" : "release";

const buildArgs = [
  "build",
  "--manifest-path",
  manifestPath,
  "--locked",
  "-p",
  "text-mod",
  ...(isDebug ? [] : ["--release"]),
  ...(targetTriple !== hostTriple ? ["--target", targetTriple] : []),
];

const buildResult = spawnSync(cargo, buildArgs, { cwd: workspaceRoot, stdio: "inherit" });
if (buildResult.error) {
  console.error(`failed to run ${cargo}:`, buildResult.error.message);
  process.exit(1);
}
if (buildResult.status !== 0) {
  process.exit(buildResult.status || 1);
}

const targetRoot = process.env.CARGO_TARGET_DIR
  ? path.resolve(workspaceRoot, process.env.CARGO_TARGET_DIR)
  : path.join(workspaceRoot, "target");
const ext = targetTriple.includes("windows") ? ".exe" : "";
const profileDir =
  targetTriple !== hostTriple
    ? path.join(targetRoot, targetTriple, profile)
    : path.join(targetRoot, profile);
const srcBin = path.join(profileDir, `text-mod${ext}`);

if (!existsSync(srcBin)) {
  console.error(`expected compiled binary at ${srcBin}, but it does not exist`);
  process.exit(1);
}

const binariesDir = path.join(workspaceRoot, "src-tauri", "binaries");
mkdirSync(binariesDir, { recursive: true });
const dstBin = path.join(binariesDir, `text-mod-${targetTriple}${ext}`);

const srcBytes = readFileSync(srcBin);
const needsStage = !existsSync(dstBin) || Buffer.compare(srcBytes, readFileSync(dstBin)) !== 0;

if (needsStage) {
  const tmpBin = path.join(binariesDir, `.text-mod-${targetTriple}${ext}.tmp-${process.pid}`);
  writeFileSync(tmpBin, srcBytes);
  if (!targetTriple.includes("windows") && process.platform !== "win32") {
    chmodSync(tmpBin, 0o755);
  }
  renameSync(tmpBin, dstBin);
}
