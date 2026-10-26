use std::collections::VecDeque;
use std::io::Read;

fn main() {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text).unwrap();
    let grid: Vec<Vec<u8>> = text.lines().filter(|l| !l.is_empty()).map(|l| l.bytes().collect()).collect();
    let find = |c: u8| {
        for (r, row) in grid.iter().enumerate() {
            if let Some(col) = row.iter().position(|&x| x == c) {
                return (r, col);
            }
        }
        (0, 0)
    };
    let (start, end) = (find(b'S'), find(b'E'));
    let mut dist = vec![vec![usize::MAX; grid[0].len()]; grid.len()];
    let mut q = VecDeque::new();
    dist[start.0][start.1] = 0;
    q.push_back(start);
    while let Some((r, c)) = q.pop_front() {
        if (r, c) == end {
            println!("{}", dist[r][c]);
            return;
        }
        let moves = [(r.wrapping_sub(1), c), (r + 1, c), (r, c.wrapping_sub(1)), (r, c + 1)];
        for (nr, nc) in moves {
            if nr < grid.len() && nc < grid[nr].len() && grid[nr][nc] != b'#' && dist[nr][nc] == usize::MAX {
                dist[nr][nc] = dist[r][c] + 1;
                q.push_back((nr, nc));
            }
        }
    }
    println!("no path");
}
