fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let mut xs: Vec<i64> = Vec::with_capacity(n);
    let mut state: i64 = 42;
    for _ in 0..n {
        state = state * 48271 % 2147483647;
        xs.push(state);
    }
    xs.sort();
    let mut checksum = 0i64;
    for x in &xs { checksum = (checksum * 31 + x) % 1000000007; }
    println!("{} {} {} {}", xs[0], xs[xs.len() / 2], xs[xs.len() - 1], checksum);
}
