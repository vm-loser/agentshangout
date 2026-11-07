//! RE8 to RE12: the operators' Reports panel, run from tests/review_ui.cjs.
#[test]
fn review_ui() {
    let output =
        std::process::Command::new("node").arg("tests/review_ui.cjs").output().expect("node runs");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
