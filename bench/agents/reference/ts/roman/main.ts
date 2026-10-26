import { readFileSync } from "node:fs";

const TABLE: [number, string][] = [
  [1000, "M"], [900, "CM"], [500, "D"], [400, "CD"], [100, "C"], [90, "XC"],
  [50, "L"], [40, "XL"], [10, "X"], [9, "IX"], [5, "V"], [4, "IV"], [1, "I"],
];
const VALUES: Record<string, number> = { I: 1, V: 5, X: 10, L: 50, C: 100, D: 500, M: 1000 };

function toRoman(n: number): string {
  let s = "";
  for (const [v, r] of TABLE) {
    while (n >= v) {
      s += r;
      n -= v;
    }
  }
  return s;
}

function fromRoman(s: string): number | null {
  let total = 0;
  for (let i = 0; i < s.length; i++) {
    const v = VALUES[s[i]];
    if (v === undefined) return null;
    const next = i + 1 < s.length ? VALUES[s[i + 1]] : undefined;
    if (next !== undefined && v < next) total -= v;
    else total += v;
  }
  // Standard form only: converting back must give the same text.
  return total >= 1 && total <= 3999 && toRoman(total) === s ? total : null;
}

let out = "";
const lines = readFileSync(0, "utf8").split("\n");
if (lines[lines.length - 1] === "") lines.pop();
for (const raw of lines) {
  const line = raw.trim();
  if (/^-?\d+$/.test(line)) {
    const n = Number(line);
    out += n >= 1 && n <= 3999 ? toRoman(n) + "\n" : "invalid\n";
  } else {
    const n = line === "" ? null : fromRoman(line);
    out += n === null ? "invalid\n" : `${n}\n`;
  }
}
process.stdout.write(out);
