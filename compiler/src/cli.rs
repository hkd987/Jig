//! Command-line entry point.

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("explain") => {
            let Some(code) = args.get(1).and_then(|c| crate::diag::Code::parse(c)) else {
                eprintln!("usage: jig explain <code>");
                return 2;
            };
            let info = code.info().unwrap();
            println!("{} ({}): {}\n\n{}", code.name(), info.slug, info.summary, info.explain);
            0
        }
        _ => {
            eprintln!("usage: jig <command>");
            2
        }
    }
}
