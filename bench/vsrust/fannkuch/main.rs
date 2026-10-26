fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(7);
    let mut perm = vec![0usize; n];
    let mut perm1: Vec<usize> = (0..n).collect();
    let mut count = vec![0usize; n];
    let (mut max_flips, mut checksum, mut perm_count) = (0i64, 0i64, 0i64);
    let mut r = n;
    loop {
        while r != 1 { count[r - 1] = r; r -= 1; }
        perm.copy_from_slice(&perm1);
        let mut flips = 0i64;
        let mut k = perm[0];
        while k != 0 {
            let (mut i, mut j) = (0, k);
            while i < j { perm.swap(i, j); i += 1; j -= 1; }
            flips += 1;
            k = perm[0];
        }
        if flips > max_flips { max_flips = flips; }
        if perm_count % 2 == 0 { checksum += flips; } else { checksum -= flips; }
        let mut done = true;
        while r < n {
            let p0 = perm1[0];
            for i in 0..r { perm1[i] = perm1[i + 1]; }
            perm1[r] = p0;
            count[r] -= 1;
            if count[r] > 0 { done = false; break; }
            r += 1;
        }
        if done { break; }
        perm_count += 1;
    }
    println!("{checksum}");
    println!("Pfannkuchen({n}) = {max_flips}");
}
