use std::collections::HashMap;
use std::io::BufRead;

fn main() {
    let mut accounts: HashMap<String, i64> = HashMap::new();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let w: Vec<&str> = line.split_whitespace().collect();
        let amount = |s: &str| s.parse::<i64>().ok();
        match w.as_slice() {
            ["open", name, a] => match amount(a) {
                Some(a) if a >= 0 => {
                    if accounts.contains_key(*name) {
                        println!("error: account {name} exists");
                    } else {
                        accounts.insert(name.to_string(), a);
                    }
                }
                _ => println!("error: bad command"),
            },
            ["transfer", from, to, a] => match amount(a) {
                Some(a) if a > 0 && from != to => {
                    if !accounts.contains_key(*from) {
                        println!("error: no account {from}");
                    } else if !accounts.contains_key(*to) {
                        println!("error: no account {to}");
                    } else if accounts[*from] < a {
                        println!("error: insufficient funds");
                    } else {
                        *accounts.get_mut(*from).unwrap() -= a;
                        *accounts.get_mut(*to).unwrap() += a;
                    }
                }
                _ => println!("error: bad command"),
            },
            ["show", name] => match accounts.get(*name) {
                Some(b) => println!("{name} {b}"),
                None => println!("error: no account {name}"),
            },
            [] => {}
            _ => println!("error: bad command"),
        }
    }
    println!("total {}", accounts.values().sum::<i64>());
}
