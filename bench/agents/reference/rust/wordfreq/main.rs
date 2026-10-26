use std::collections::HashMap;
use std::io::Read;

fn main() {
    let mut text = String::new();
    std::io::stdin().read_to_string(&mut text).unwrap();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for word in text.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| !w.is_empty()) {
        *counts.entry(word.to_ascii_lowercase()).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (w, n) in v.into_iter().take(3) {
        println!("{w} {n}");
    }
}
