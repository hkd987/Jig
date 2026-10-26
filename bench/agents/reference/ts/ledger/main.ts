import { readFileSync } from "node:fs";

const accounts = new Map<string, bigint>();
const amount = (s: string): bigint | null => (/^-?\d+$/.test(s) ? BigInt(s) : null);
const INT_MAX = 9223372036854775807n;
let out = "";
for (const line of readFileSync(0, "utf8").split("\n")) {
  const w = line.trim().split(/\s+/).filter((x) => x.length > 0);
  if (w.length === 0) continue;
  if (w[0] === "open" && w.length === 3) {
    const a = amount(w[2]);
    if (a === null || a < 0n || a > INT_MAX) out += "error: bad command\n";
    else if (accounts.has(w[1])) out += `error: account ${w[1]} exists\n`;
    else accounts.set(w[1], a);
  } else if (w[0] === "transfer" && w.length === 4) {
    const [, from, to] = w;
    const a = amount(w[3]);
    if (a === null || a <= 0n || a > INT_MAX || from === to) out += "error: bad command\n";
    else if (!accounts.has(from)) out += `error: no account ${from}\n`;
    else if (!accounts.has(to)) out += `error: no account ${to}\n`;
    else if (accounts.get(from)! < a) out += "error: insufficient funds\n";
    else {
      accounts.set(from, accounts.get(from)! - a);
      accounts.set(to, accounts.get(to)! + a);
    }
  } else if (w[0] === "show" && w.length === 2) {
    const b = accounts.get(w[1]);
    out += b === undefined ? `error: no account ${w[1]}\n` : `${w[1]} ${b}\n`;
  } else {
    out += "error: bad command\n";
  }
}
let total = 0n;
for (const b of accounts.values()) total += b;
out += `total ${total}\n`;
process.stdout.write(out);
