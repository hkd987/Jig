use std::collections::HashMap;
fn word(seed: i64) -> String {
    let syl = ["ka", "lo", "mi", "ne", "ru", "ta", "vo", "zi"];
    let mut w = String::new();
    let mut s = seed;
    for _ in 0..1 + seed % 3 { w.push_str(syl[(s % 8) as usize]); s /= 8; }
    w
}
fn main() {
    let n: i64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let mut state: i64 = 12345;
    let mut text = String::new();
    for _ in 0..n {
        state = (state * 1103515245 + 12345) % 2147483648;
        text.push_str(&word(state / 65536 % 4096));
        text.push(' ');
    }
    let mut counts: HashMap<&str, i64> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for w in text.split(' ') {
        if !w.is_empty() {
            let c = counts.entry(w).or_insert_with(|| { order.push(w); 0 });
            *c += 1;
        }
    }
    let mut es: Vec<(&str, i64)> = order.iter().map(|w| (*w, counts[w])).collect();
    es.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    println!("{} distinct words", counts.len());
    for (w, c) in es.iter().take(10) { println!("{w} {c}"); }
}
