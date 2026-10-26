use std::io::BufRead;

const TABLE: [(i64, &str); 13] = [
    (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
    (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
];

fn to_roman(mut n: i64) -> String {
    let mut s = String::new();
    for (v, r) in TABLE {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

fn from_roman(s: &str) -> Option<i64> {
    let value = |c: char| match c {
        'I' => Some(1), 'V' => Some(5), 'X' => Some(10), 'L' => Some(50),
        'C' => Some(100), 'D' => Some(500), 'M' => Some(1000), _ => None,
    };
    let mut total = 0;
    let cs: Vec<char> = s.chars().collect();
    for i in 0..cs.len() {
        let v = value(cs[i])?;
        if i + 1 < cs.len() && v < value(cs[i + 1])? {
            total -= v;
        } else {
            total += v;
        }
    }
    // Standard form only: converting back must give the same text.
    (total >= 1 && total <= 3999 && to_roman(total) == s).then_some(total)
}

fn main() {
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let line = line.trim();
        if let Ok(n) = line.parse::<i64>() {
            if (1..=3999).contains(&n) {
                println!("{}", to_roman(n));
            } else {
                println!("invalid");
            }
        } else {
            match from_roman(line) {
                Some(n) if !line.is_empty() => println!("{n}"),
                _ => println!("invalid"),
            }
        }
    }
}
