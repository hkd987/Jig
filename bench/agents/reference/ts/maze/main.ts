import { readFileSync } from "node:fs";

const grid = readFileSync(0, "utf8").split("\n").filter((l) => l.length > 0);
const find = (c: string): [number, number] => {
  for (let r = 0; r < grid.length; r++) {
    const col = grid[r].indexOf(c);
    if (col >= 0) return [r, col];
  }
  return [0, 0];
};
const [sr, sc] = find("S");
const [er, ec] = find("E");
const dist = grid.map((row) => new Array<number>(row.length).fill(-1));
dist[sr][sc] = 0;
const queue: [number, number][] = [[sr, sc]];
let answer = "no path";
for (let head = 0; head < queue.length; head++) {
  const [r, c] = queue[head];
  if (r === er && c === ec) {
    answer = String(dist[r][c]);
    break;
  }
  for (const [nr, nc] of [[r - 1, c], [r + 1, c], [r, c - 1], [r, c + 1]]) {
    if (nr >= 0 && nr < grid.length && nc >= 0 && nc < grid[nr].length && grid[nr][nc] !== "#" && dist[nr][nc] < 0) {
      dist[nr][nc] = dist[r][c] + 1;
      queue.push([nr, nc]);
    }
  }
}
process.stdout.write(answer + "\n");
