import { mkdtempSync, realpathSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { spawn } from "node:child_process";
const root = realpathSync(mkdtempSync(join(tmpdir(), "pc-web-e2e-")));
mkdirSync(join(root, "quarantine"));
mkdirSync(join(root, "archive"));
mkdirSync(join(root, "output"));
const child = spawn(
  resolve("../../../target/debug/photo-cleanup"),
  [
    "--db",
    join(root, "test.db"),
    "serve",
    "--bind",
    `127.0.0.1:${process.env.PC_TEST_PORT || 18086}`,
    "--quarantine",
    join(root, "quarantine"),
  ],
  { stdio: "inherit" },
);
let closing = false;
const close = () => {
  if (closing) return;
  closing = true;
  child.kill("SIGTERM");
};
process.on("SIGTERM", close);
process.on("SIGINT", close);
child.on("exit", (code) => {
  rmSync(root, { recursive: true, force: true });
  process.exit(code || 0);
});
