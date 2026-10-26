//! Property tests: generated inputs, `requires` as a filter, shrinking to a minimal failure.

use jig::sema::load::Sources;
use std::path::Path;

#[test]
fn property_tests_generate_filter_and_shrink() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/props");
    let mut prog = jig::sema::analyze(&Sources::Disk, &dir);
    assert!(!prog.diags.has_errors());
    let results = jig::runner::run_tests(&mut prog, None);
    let by_name = |n: &str| results.iter().find(|r| r.name == n).unwrap_or_else(|| panic!("no test {n}"));
    for ok in ["add commutes", "no requires", "lists", "reverse twice"] {
        assert!(by_name(ok).passed, "{ok}: {}", by_name(ok).message);
    }
    let clamp = by_name("clamp is at most 50");
    assert!(!clamp.passed);
    assert!(clamp.message.ends_with("with inputs: x = 51"), "{}", clamp.message);
    // Deterministic: the same run gives the same report.
    let again = jig::runner::run_tests(&mut prog, None);
    assert_eq!(
        again.iter().map(|r| r.message.clone()).collect::<Vec<_>>(),
        results.iter().map(|r| r.message.clone()).collect::<Vec<_>>()
    );
}
