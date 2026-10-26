import { readFileSync } from "node:fs";

const text: string = readFileSync(0, "utf8");
const counts = new Map<string, number>();
for (const w of text.split(/[^A-Za-z]+/).filter((w) => w.length > 0)) {
  const k = w.toLowerCase();
  counts.set(k, (counts.get(k) ?? 0) + 1);
}
const sorted = [...counts.entries()].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
let out = "";
for (const [w, n] of sorted.slice(0, 3)) {
  out += `${w} ${n}\n`;
}
process.stdout.write(out);
