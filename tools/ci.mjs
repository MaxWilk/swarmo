// Everything that must pass before a change lands.
// Run: node tools/ci.mjs
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

const steps = [
  ["cargo", ["fmt", "--all", "--check"], root, "rustfmt"],
  ["cargo", ["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"], root, "clippy"],
  ["cargo", ["test", "--workspace"], root, "cargo test"],
  ["npm", ["run", "typecheck"], join(root, "ui"), "typecheck"],
  ["npm", ["test"], join(root, "ui"), "ui test"],
  ["npm", ["run", "build"], join(root, "ui"), "ui build"],
];

let failed = 0;
for (const [cmd, args, cwd, label] of steps) {
  process.stdout.write(`\n=== ${label} ===\n`);
  const r = spawnSync(cmd, args, { cwd, stdio: "inherit", shell: process.platform === "win32" });
  if (r.status !== 0) {
    failed++;
    process.stdout.write(`--- ${label} FAILED ---\n`);
  }
}

process.stdout.write(
  failed === 0 ? "\nAll checks passed.\n" : `\n${failed} check(s) failed.\n`,
);
process.exit(failed === 0 ? 0 : 1);
