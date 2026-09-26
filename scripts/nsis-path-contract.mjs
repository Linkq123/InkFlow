import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { spawnSync } from "node:child_process";

const root = resolve(import.meta.dirname, "..");
const hooks = readFileSync(resolve(root, "src-tauri/windows/hooks.nsh"), "utf8");
const bundle = JSON.parse(readFileSync(resolve(root, "src-tauri/tauri.bundle.conf.json"), "utf8"));
assert.equal(bundle.bundle.resources["windows/path-helper.ps1"], "path-helper.ps1");
const commands = hooks.split(/\r?\n/).filter(line => line.includes("nsExec::ExecToStack"));
assert.equal(commands.length, 2);
for (const [index, action] of ["Install", "Remove"].entries()) {
  const command = commands[index];
  assert.ok(command.includes('-File "$INSTDIR\\path-helper.ps1"'));
  assert.ok(command.includes(`-Action ${action}`));
  assert.ok(!command.includes("-Command"));
  // Include a long Windows installation directory in the runtime stack budget.
  assert.ok(command.replace("$INSTDIR", "x".repeat(260)).replace("$SYSDIR", "C:\\Windows\\System32").length < 1024);
}
assert.match(hooks, /\$\{Else\}\s+\$\{IfNot\} \$\{Silent\}\s+MessageBox MB_OK\|MB_ICONEXCLAMATION "\$\(InkFlowPathRemovalFailed\)"/);
if (process.platform === "win32") {
  const result = spawnSync("powershell.exe", ["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", resolve(import.meta.dirname, "nsis-path-contract.ps1")], { encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr || result.error?.message);
  process.stdout.write(result.stdout);
}
console.log("NSIS PATH hook contract passed.");
