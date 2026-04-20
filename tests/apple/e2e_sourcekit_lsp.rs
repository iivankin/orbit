use std::fs;

use crate::support::{
    base_command, create_build_xcrun_mock, create_home, create_mixed_language_workspace, read_log,
    run_and_capture, sourcekit_lsp_command,
};

#[test]
#[ignore = "manual sourcekit-lsp integration test"]
fn sourcekit_lsp_debug_index_uses_orbi_build_server() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let mut install = base_command(&workspace, &home, &mock_bin, &log_path);
    install.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbi.json").to_str().unwrap(),
        "ide",
        "install-build-server",
    ]);
    let install_output = run_and_capture(&mut install);
    assert!(
        install_output.status.success(),
        "{}",
        String::from_utf8_lossy(&install_output.stderr)
    );

    let mut index = sourcekit_lsp_command(&workspace, &home, &mock_bin, &log_path);
    index.args([
        "--default-workspace-type",
        "buildServer",
        "debug",
        "index",
        "--experimental-feature",
        "output-paths-request",
        "--experimental-feature",
        "sourcekit-options-request",
        "--experimental-feature",
        "synchronize-for-build-system-updates",
        "--project",
        workspace.to_str().unwrap(),
    ]);
    let output = run_and_capture(&mut index);
    assert!(
        output.status.success(),
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("xcrun --find swiftc"));
    assert!(log.contains("xcrun --sdk iphonesimulator swiftc"));
    assert!(log.contains("xcrun --sdk iphonesimulator clang"));
}
