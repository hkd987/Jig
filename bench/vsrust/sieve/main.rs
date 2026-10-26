fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let mut composite = vec![false; n + 1];
    let (mut count, mut sum) = (0i64, 0i64);
    for i in 2..n + 1 {
        if !composite[i] {
            count += 1; sum += i as i64;
            let mut j = i * i;
            while j <= n { composite[j] = true; j += i; }
        }
    }
    println!("{count} primes, sum {sum}");
}
