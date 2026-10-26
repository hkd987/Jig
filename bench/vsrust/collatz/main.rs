fn steps(start: i64) -> i64 {
    let (mut n, mut s) = (start, 0);
    while n != 1 { if n % 2 == 0 { n /= 2 } else { n = 3 * n + 1 } s += 1; }
    s
}
fn main() {
    let limit: i64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let (mut best, mut best_steps) = (1, 0);
    for i in 1..limit { let s = steps(i); if s > best_steps { best = i; best_steps = s; } }
    println!("{best} takes {best_steps} steps");
}
