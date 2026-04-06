use std::fs;

use serde_json::json;

use crate::support::{
    base_command, clear_log, create_build_xcrun_mock, create_git_swift_package_workspace,
    create_home, create_mixed_language_workspace, create_quality_swift_mock,
    create_semver_git_swift_package_workspace, create_signing_workspace,
    create_swift_package_workspace, create_watch_workspace, orbit_cache_dir, read_log,
    run_and_capture,
};

fn quality_tools_are_unavailable_in_debug_build() -> bool {
    cfg!(debug_assertions)
}

fn assert_debug_build_quality_tool_unavailable(output: &std::process::Output) {
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Orbit debug builds skip embedded Swift quality tooling"),
        "{stderr}"
    );
}

#[test]
fn lint_runs_swiftlint_and_semantic_analysis_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    if quality_tools_are_unavailable_in_debug_build() {
        assert_debug_build_quality_tool_unavailable(&output);
        return;
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(!log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("orbit-swiftlint "));
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("\"compiler_invocations\""));
    assert!(log.contains("\"arguments\""));
    assert!(log.contains("\"swiftc\""));
    assert!(log.contains("\"-sdk\""));
    assert!(log.contains("\"ExampleApp\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn lint_platform_flag_limits_semantic_analysis_scope() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "lint",
        "--platform",
        "ios",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(!log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("orbit-swiftlint "));
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("\"swiftc\""));
    assert!(log.contains("\"-sdk\""));
    assert!(log.contains("\"WatchFixture\""));
    assert!(!log.contains("xcrun --sdk watchsimulator --show-sdk-path"));
    assert!(!log.contains("\"WatchApp\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn lint_runs_compiler_backed_c_family_diagnostics_for_mixed_targets() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("orbit-swiftlint "));
    assert!(log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(log.contains("-fsyntax-only"));
    assert!(log.contains("Sources/App/Bridge.m"));
    assert!(!log.contains("Bridge.m.o"));
}

#[test]
fn lint_reuses_cached_semantic_artifact_between_runs() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let manifest_path = workspace.join("orbit.json");

    let mut first = base_command(&workspace, &home, &mock_bin, &log_path);
    first.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_log = read_log(&log_path);
    assert!(first_log.contains("xcrun --find swiftc"));

    clear_log(&log_path);

    let mut second = base_command(&workspace, &home, &mock_bin, &log_path);
    second.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("xcrun --find swiftc"));
}

#[test]
fn lint_reuses_cached_swift_package_outputs_between_runs() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_swift_package_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let manifest_path = workspace.join("orbit.json");

    let mut first = base_command(&workspace, &home, &mock_bin, &log_path);
    first.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let first_log = read_log(&log_path);
    assert!(first_log.contains("swift package --package-path"));
    assert!(first_log.contains("xcrun --sdk iphonesimulator swiftc"));

    clear_log(&log_path);

    let mut second = base_command(&workspace, &home, &mock_bin, &log_path);
    second.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("swift package --package-path"));
    assert!(!second_log.contains("xcrun --sdk iphonesimulator swiftc"));
    assert!(!workspace.join(".orbit").exists());
}

#[test]
fn lint_resolves_git_swift_package_dependencies_from_pinned_revisions() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let (workspace, fixture) = create_git_swift_package_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let manifest_path = workspace.join("orbit.json");

    let mut first = base_command(&workspace, &home, &mock_bin, &log_path);
    first.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let first_output = run_and_capture(&mut first);
    assert!(
        first_output.status.success(),
        "{}",
        String::from_utf8_lossy(&first_output.stderr)
    );

    let cache_root = orbit_cache_dir(&home)
        .join("git-swift-packages")
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("checkout").exists())
        .expect("expected a cached git Swift package checkout");
    let cached_head = std::process::Command::new("git")
        .args(["-C"])
        .arg(cache_root.join("checkout"))
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(cached_head.status.success());
    assert_eq!(
        String::from_utf8(cached_head.stdout).unwrap().trim(),
        fixture.initial_revision
    );

    clear_log(&log_path);

    let mut second = base_command(&workspace, &home, &mock_bin, &log_path);
    second.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let second_output = run_and_capture(&mut second);
    assert!(
        second_output.status.success(),
        "{}",
        String::from_utf8_lossy(&second_output.stderr)
    );

    let second_log = read_log(&log_path);
    assert!(!second_log.contains("swift package --package-path"));
    assert!(!second_log.contains("xcrun --sdk iphonesimulator swiftc"));
}

#[test]
fn lint_accepts_semver_pinned_git_swift_package_dependencies() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let (workspace, fixture) = create_semver_git_swift_package_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let manifest_path = workspace.join("orbit.json");
    assert!(!workspace.join(".orbit/orbit.lock").exists());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let cache_root = orbit_cache_dir(&home)
        .join("git-swift-packages")
        .read_dir()
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("checkout").exists())
        .expect("expected a cached git Swift package checkout");
    let cached_head = std::process::Command::new("git")
        .args(["-C"])
        .arg(cache_root.join("checkout"))
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(cached_head.status.success());
    assert_eq!(
        String::from_utf8(cached_head.stdout).unwrap().trim(),
        fixture.initial_revision
    );
    let lockfile: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.join(".orbit/orbit.lock")).unwrap()).unwrap();
    assert_eq!(
        lockfile["dependencies"]["OrbitPkg"]["revision"].as_str(),
        Some(fixture.initial_revision.as_str())
    );
}

#[test]
fn lint_recreates_internal_lockfile_for_versioned_git_swift_package_dependencies() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let (workspace, fixture) = create_semver_git_swift_package_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    let manifest_path = workspace.join("orbit.json");
    fs::create_dir_all(workspace.join(".orbit")).unwrap();
    fs::write(workspace.join(".orbit/orbit.lock"), b"stale").unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lockfile: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.join(".orbit/orbit.lock")).unwrap()).unwrap();
    assert_eq!(
        lockfile["dependencies"]["OrbitPkg"]["revision"].as_str(),
        Some(fixture.initial_revision.as_str())
    );
}

#[test]
fn format_defaults_to_read_only_check() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "format",
    ]);
    let output = run_and_capture(&mut command);
    if quality_tools_are_unavailable_in_debug_build() {
        assert_debug_build_quality_tool_unavailable(&output);
        return;
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(!log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("orbit-swift-format "));
    assert!(log.contains("\"mode\": \"check\""));
    assert!(!log.contains("\"mode\": \"write\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn format_uses_orbit_default_four_space_indentation() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "format",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("\"configuration_json\""));
    assert!(log.contains("\\\"indentation\\\":{\\\"spaces\\\":4}"));
}

#[test]
fn format_write_runs_swift_format_in_place() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "format",
        "--write",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(!log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("orbit-swift-format "));
    assert!(log.contains("\"mode\": \"write\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn lint_reads_orbit_json_rules_and_ignore_globs() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(workspace.join("Sources/App/Generated")).unwrap();
    fs::write(
        workspace.join("Sources/App/Generated/Ignored.swift"),
        "import Foundation\nlet ignored = 1\n",
    )
    .unwrap();

    let manifest_path = workspace.join("orbit.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["quality"] = json!({
        "lint": {
            "ignore": ["**/Generated/**"],
            "rules": {
                "unused_import": "error",
                "trailing_whitespace": ["warn", { "ignores_empty_lines": true }]
            }
        }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("\"configuration_json\""));
    assert!(log.contains("\\\"unused_import\\\":\\\"error\\\""));
    assert!(
        log.contains(
            "\\\"trailing_whitespace\\\":[\\\"warn\\\",{\\\"ignores_empty_lines\\\":true}]"
        )
    );
    assert!(log.contains("Sources/App/App.swift"));
    assert!(!log.contains("Sources/App/Generated/Ignored.swift"));
}

#[test]
fn format_reads_editorconfig_rules_and_ignore_globs_from_orbit_json() {
    if quality_tools_are_unavailable_in_debug_build() {
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(workspace.join("Sources/App/Generated")).unwrap();
    fs::write(
        workspace.join("Sources/App/Generated/Ignored.swift"),
        "import Foundation\nlet ignored = 1\n",
    )
    .unwrap();
    fs::write(
        workspace.join(".editorconfig"),
        "root = true\n\n[*.swift]\nindent_style = space\nindent_size = 4\ntab_width = 4\nmax_line_length = 120\n",
    )
    .unwrap();

    let manifest_path = workspace.join("orbit.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["quality"] = json!({
        "format": {
            "ignore": ["**/Generated/**"],
            "editorconfig": true,
            "rules": {
                "NoAssignmentInExpressions": "off",
                "indentSwitchCaseLabels": true
            }
        }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "format",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("\"configuration_json\""));
    assert!(log.contains("\\\"lineLength\\\":120"));
    assert!(log.contains("\\\"indentation\\\":{\\\"spaces\\\":4}"));
    assert!(log.contains("\\\"tabWidth\\\":4"));
    assert!(log.contains("\\\"rules\\\":{\\\"NoAssignmentInExpressions\\\":false}"));
    assert!(log.contains("\\\"indentSwitchCaseLabels\\\":true"));
    assert!(log.contains("Sources/App/App.swift"));
    assert!(!log.contains("Sources/App/Generated/Ignored.swift"));
}
