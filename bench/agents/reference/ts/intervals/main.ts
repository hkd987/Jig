import { readFileSync } from "node:fs";

const v: [number, number][] = [];
for (const line of readFileSync(0, "utf8").split("\n")) {
  const n = line.trim().split(/\s+/).filter((x) => x.length > 0).map(Number);
  if (n.length === 2) v.push([n[0], n[1]]);
}
v.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
const out: [number, number][] = [];
for (const [a, b] of v) {
  const last = out[out.length - 1];
  if (last && a <= last[1]) last[1] = Math.max(last[1], b);
  else out.push([a, b]);
}
process.stdout.write(out.map(([a, b]) => `${a} ${b}\n`).join(""));
