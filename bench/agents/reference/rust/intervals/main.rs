use std::io::BufRead;

fn main() {
    let mut v: Vec<(i64, i64)> = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let n: Vec<i64> = line.split_whitespace().map(|x| x.parse().unwrap()).collect();
        if n.len() == 2 {
            v.push((n[0], n[1]));
        }
    }
    v.sort();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    for (a, b) in out {
        println!("{a} {b}");
    }
}
