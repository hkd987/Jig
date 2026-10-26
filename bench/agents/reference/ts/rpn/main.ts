import { readFileSync } from "node:fs";

function evaluate(line: string): bigint | string {
  const stack: bigint[] = [];
  for (const tok of line.split(/\s+/).filter((t) => t.length > 0)) {
    if (/^-?\d+$/.test(tok)) {
      stack.push(BigInt(tok));
      continue;
    }
    if (!["+", "-", "*", "/"].includes(tok)) return `bad token ${tok}`;
    if (stack.length < 2) return "not enough operands";
    const b = stack.pop()!;
    const a = stack.pop()!;
    if (tok === "+") stack.push(a + b);
    else if (tok === "-") stack.push(a - b);
    else if (tok === "*") stack.push(a * b);
    else {
      if (b === 0n) return "division by zero";
      stack.push(a / b);
    }
  }
  if (stack.length === 0) return "not enough operands";
  if (stack.length > 1) return "too many operands";
  return stack[0];
}

let out = "";
for (const line of readFileSync(0, "utf8").split("\n")) {
  if (line.trim() === "") continue;
  const r = evaluate(line);
  out += typeof r === "string" ? `error: ${r}\n` : `${r}\n`;
}
process.stdout.write(out);
