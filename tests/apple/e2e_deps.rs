use std::fs;

use crate::support::{
    base_command, create_git_swift_package_workspace, create_home,
    create_semver_git_swift_package_workspace, read_log, run_and_capture,
};

#[test]
fn deps_update_refreshes_git_dependency_revisions_in_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let (workspace, fixture) = create_git_swift_package_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    let manifest_path = workspace.join("orbit.json");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "deps",
        "update",
        "OrbitPkg",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(
        manifest["dependencies"]["OrbitPkg"]["git"].as_str(),
        Some(fixture.remote_url.as_str())
    );
    assert_eq!(
        manifest["dependencies"]["OrbitPkg"]["revision"].as_str(),
        Some(fixture.latest_revision.as_str())
    );
    assert!(!workspace.join(".orbit/orbit.lock").exists());

    let log = read_log(&log_path);
    assert!(log.is_empty());
}

#[test]
fn deps_update_uses_semver_requirement_and_keeps_dependency_pinned() {
    let temp = tempfile::tempdir().unwrap();
    let (workspace, fixture) = create_semver_git_swift_package_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    let manifest_path = workspace.join("orbit.json");

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "deps",
        "update",
        "OrbitPkg",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(
        manifest["dependencies"]["OrbitPkg"]["git"].as_str(),
        Some(fixture.remote_url.as_str())
    );
    assert_eq!(
        manifest["dependencies"]["OrbitPkg"]["version"].as_str(),
        Some("1.2.0")
    );
    assert!(
        manifest["dependencies"]["OrbitPkg"]
            .as_object()
            .unwrap()
            .get("revision")
            .is_none()
    );
    let lockfile: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.join(".orbit/orbit.lock")).unwrap()).unwrap();
    assert_eq!(
        lockfile["dependencies"]["OrbitPkg"]["revision"].as_str(),
        Some(fixture.matching_revision.as_str())
    );
    assert_ne!(
        lockfile["dependencies"]["OrbitPkg"]["revision"].as_str(),
        Some(fixture.non_matching_revision.as_str())
    );

    let log = read_log(&log_path);
    assert!(log.is_empty());
}
