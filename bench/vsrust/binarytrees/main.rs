enum Tree { Leaf, Node(Box<Tree>, Box<Tree>) }
fn make(depth: i64) -> Tree {
    if depth == 0 { return Tree::Node(Box::new(Tree::Leaf), Box::new(Tree::Leaf)); }
    Tree::Node(Box::new(make(depth - 1)), Box::new(make(depth - 1)))
}
fn check(t: &Tree) -> i64 { match t { Tree::Leaf => 0, Tree::Node(l, r) => 1 + check(l) + check(r) } }
fn main() {
    let n: i64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let max_depth = n.max(6);
    println!("stretch tree of depth {}\t check: {}", max_depth + 1, check(&make(max_depth + 1)));
    let long_lived = make(max_depth);
    let mut d = 4;
    while d <= max_depth {
        let iters = 1i64 << (max_depth - d + 4);
        let mut total = 0;
        for _ in 0..iters { total += check(&make(d)); }
        println!("{iters}\t trees of depth {d}\t check: {total}");
        d += 2;
    }
    println!("long lived tree of depth {max_depth}\t check: {}", check(&long_lived));
}
