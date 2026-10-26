use std::io::BufRead;

fn eval(line: &str) -> Result<i64, String> {
    let mut stack: Vec<i64> = Vec::new();
    for tok in line.split_whitespace() {
        if let Ok(n) = tok.parse::<i64>() {
            stack.push(n);
            continue;
        }
        if !matches!(tok, "+" | "-" | "*" | "/") {
            return Err(format!("bad token {tok}"));
        }
        let (Some(b), Some(a)) = (stack.pop(), stack.pop()) else { return Err("not enough operands".into()) };
        stack.push(match tok {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            _ => {
                if b == 0 {
                    return Err("division by zero".into());
                }
                a / b
            }
        });
    }
    match stack.as_slice() {
        [v] => Ok(*v),
        [] => Err("not enough operands".into()),
        _ => Err("too many operands".into()),
    }
}

fn main() {
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        match eval(&line) {
            Ok(v) => println!("{v}"),
            Err(e) => println!("error: {e}"),
        }
    }
}
