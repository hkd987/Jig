//! Traceability over the bank example's scenarios.

use jig::sema::load::Sources;
use jig::tools::trace::{run, Query};
use std::path::Path;

#[test]
fn trace_answers_the_three_questions() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/bank");
    let prog = jig::sema::analyze(&Sources::Disk, &dir);
    assert!(!prog.diags.has_errors());
    let (text, _) = run(&prog, &Query::Function("transfer".into()));
    assert!(text.contains("Payments: A payment moves money"), "{text}");
    let (text, _) = run(&prog, &Query::Uncovered);
    assert!(text.contains("TxError.str") && !text.contains("transfer"), "{text}");
    let (text, json) = run(&prog, &Query::Scenario("overdraft".into()));
    assert!(text.contains("then_refused"), "{text}");
    let dies = json.get("dies_without_it").and_then(|d| d.as_arr()).map(|d| d.len());
    assert_eq!(dies, Some(1));
}
