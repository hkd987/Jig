fn a(i: i64, j: i64) -> f64 { 1.0 / (((i + j) * (i + j + 1) / 2 + i + 1) as f64) }
fn mul_av(v: &[f64], out: &mut [f64]) {
    let n = v.len();
    for i in 0..n { let mut s = 0.0; for j in 0..n { s += a(i as i64, j as i64) * v[j]; } out[i] = s; }
}
fn mul_atv(v: &[f64], out: &mut [f64]) {
    let n = v.len();
    for i in 0..n { let mut s = 0.0; for j in 0..n { s += a(j as i64, i as i64) * v[j]; } out[i] = s; }
}
fn mul_atav(v: &[f64], out: &mut [f64], tmp: &mut [f64]) { mul_av(v, tmp); mul_atv(tmp, out); }
fn main() {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(100);
    let mut u = vec![1.0; n]; let mut v = vec![0.0; n]; let mut tmp = vec![0.0; n];
    for _ in 0..10 { mul_atav(&u, &mut v, &mut tmp); mul_atav(&v, &mut u, &mut tmp); }
    let (mut vbv, mut vv) = (0.0, 0.0);
    for i in 0..n { vbv += u[i] * v[i]; vv += v[i] * v[i]; }
    println!("{:?}", (vbv / vv).sqrt());
}
