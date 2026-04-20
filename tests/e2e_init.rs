mod support;

use support::orbi_bin;

#[test]
fn init_requires_interactive_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(orbi_bin())
        .current_dir(temp.path())
        .args(["--non-interactive", "init"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!temp.path().join("orbi.json").exists());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`orbi init` requires an interactive terminal"));
}
