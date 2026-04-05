#[path = "../../support/command_fixtures.rs"]
mod command_fixtures;

mod asc_mock;
mod crypto;
mod live;
pub mod notary_mock;
pub mod submit_mock;
mod tool_mocks;
mod ui_helpers;
mod workspaces;

pub(crate) use self::asc_mock::read_http_request;
#[allow(unused_imports)]
pub use self::asc_mock::{AscMockServer, spawn_asc_mock};
pub use self::command_fixtures::{
    base_command, clear_log, create_home, latest_receipt_path, orbit_bin, orbit_cache_dir,
    orbit_data_dir, read_log, run_and_capture, sourcekit_lsp_command, write_executable,
};
pub use self::crypto::{create_api_key, create_p12};
#[allow(unused_imports)]
pub use self::live::{
    LiveAppleConfig, LiveCleanupGuard, create_live_workspace, create_live_workspace_with_manifest,
    live_command, remote_capabilities_for_bundle_id, require_live_apple_config,
};
pub use self::tool_mocks::{
    create_build_xcrun_mock, create_codesign_mock, create_ditto_mock, create_idb_mock,
    create_lldb_attach_mock, create_passthrough_mock, create_quality_swift_mock,
    create_security_mock, create_submit_swinfo_mock, create_sw_vers_mock,
    create_testing_swift_mock, create_watch_xcrun_mock, create_xcodebuild_mock,
};
pub use self::ui_helpers::{
    create_fake_xcode_bundle, create_runtime_download_xcodebuild_mock,
    create_runtime_installing_xcrun_mock, format_failure_output, latest_ui_report_path,
    set_manifest_platforms, set_manifest_xcode,
};
pub use self::workspaces::{
    create_asset_resource_workspace, create_git_swift_package_workspace,
    create_macos_universal_workspace, create_mixed_language_workspace, create_resource_workspace,
    create_semver_git_swift_package_workspace, create_signing_workspace,
    create_swift_package_workspace, create_testing_workspace, create_ui_testing_workspace,
    create_watch_workspace, create_xcframework_workspace,
};
