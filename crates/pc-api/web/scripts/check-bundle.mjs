import { readFileSync, readdirSync } from "node:fs";
import { gzipSync } from "node:zlib";
const files = readdirSync("dist/static");
const bytes = files.reduce(
  (n, file) => n + gzipSync(readFileSync(`dist/static/${file}`)).length,
  0,
);
console.log(`Static bundle: ${(bytes / 1024).toFixed(1)} KiB gzip / 400 KiB`);
if (bytes > 400 * 1024)
  throw new Error("Bundle exceeds FRONTEND-SPEC.md limit");
