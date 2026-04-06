use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use std::ffi::{CString, c_char};
#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;

#[cfg(target_os = "macos")]
use anyhow::Context;
use anyhow::{Result, bail};
#[cfg(target_os = "macos")]
use libloading::Library;
use serde::Serialize;
#[cfg(target_os = "macos")]
use sha2::{Digest, Sha256};

use crate::context::AppContext;
#[cfg(target_os = "macos")]
use crate::util::{ensure_dir, write_json_file};

#[derive(Debug, Serialize)]
pub(crate) struct OrbitSwiftFormatRequest {
    pub working_directory: PathBuf,
    pub configuration_json: Option<String>,
    pub mode: OrbitSwiftFormatMode,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OrbitSwiftFormatMode {
    Check,
    Write,
}

#[derive(Debug, Serialize)]
pub(crate) struct OrbitSwiftLintRequest {
    pub working_directory: PathBuf,
    pub configuration_json: Option<String>,
    pub files: Vec<PathBuf>,
    pub compiler_invocations: Vec<OrbitSwiftLintCompilerInvocation>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrbitSwiftLintCompilerInvocation {
    pub arguments: Vec<String>,
    pub source_files: Vec<PathBuf>,
}

#[cfg(target_os = "macos")]
mod embedded_swift_tools {
    include!(concat!(env!("OUT_DIR"), "/embedded_swift_tools.rs"));
}

#[cfg(target_os = "macos")]
type SwiftToolEntryPoint = unsafe extern "C" fn(*const c_char) -> i32;

#[cfg(target_os = "macos")]
struct EmbeddedSwiftToolSpec {
    name: &'static str,
    file_name: &'static str,
    bytes: &'static [u8],
    symbol_name: &'static [u8],
    available: bool,
    unavailable_reason: &'static str,
}

#[cfg(target_os = "macos")]
const ORBIT_SWIFTLINT_TOOL: EmbeddedSwiftToolSpec = EmbeddedSwiftToolSpec {
    name: "orbit-swiftlint",
    file_name: embedded_swift_tools::ORBIT_SWIFTLINT_FFI_FILE_NAME,
    bytes: embedded_swift_tools::ORBIT_SWIFTLINT_FFI_BYTES,
    symbol_name: b"orbit_swiftlint_run_request\0",
    available: embedded_swift_tools::ORBIT_SWIFTLINT_FFI_AVAILABLE,
    unavailable_reason: embedded_swift_tools::ORBIT_SWIFTLINT_FFI_UNAVAILABLE_REASON,
};

#[cfg(target_os = "macos")]
const ORBIT_SWIFT_FORMAT_TOOL: EmbeddedSwiftToolSpec = EmbeddedSwiftToolSpec {
    name: "orbit-swift-format",
    file_name: embedded_swift_tools::ORBIT_SWIFTFORMAT_FFI_FILE_NAME,
    bytes: embedded_swift_tools::ORBIT_SWIFTFORMAT_FFI_BYTES,
    symbol_name: b"orbit_swiftformat_run_request\0",
    available: embedded_swift_tools::ORBIT_SWIFTFORMAT_FFI_AVAILABLE,
    unavailable_reason: embedded_swift_tools::ORBIT_SWIFTFORMAT_FFI_UNAVAILABLE_REASON,
};

#[cfg(target_os = "macos")]
pub(crate) fn run_orbit_swift_format(
    app: &AppContext,
    request_root: &Path,
    request: &OrbitSwiftFormatRequest,
) -> Result<()> {
    ensure_embedded_swift_tool_available(&ORBIT_SWIFT_FORMAT_TOOL)?;
    let request_path = request_root.join("orbit-swift-format-request.json");
    write_json_file(&request_path, request)?;
    run_embedded_swift_tool(app, &ORBIT_SWIFT_FORMAT_TOOL, &request_path)
        .with_context(|| "failed to run the Orbit Swift formatter")
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_orbit_swift_format(
    app: &AppContext,
    request_root: &Path,
    request: &OrbitSwiftFormatRequest,
) -> Result<()> {
    let _ = (app, request_root, request);
    bail!("Orbit Swift formatting is supported only on macOS hosts")
}

#[cfg(target_os = "macos")]
pub(crate) fn run_orbit_swiftlint(
    app: &AppContext,
    request_root: &Path,
    request: &OrbitSwiftLintRequest,
) -> Result<()> {
    ensure_embedded_swift_tool_available(&ORBIT_SWIFTLINT_TOOL)?;
    let request_path = request_root.join("orbit-swiftlint-request.json");
    write_json_file(&request_path, request)?;
    run_embedded_swift_tool(app, &ORBIT_SWIFTLINT_TOOL, &request_path)
        .with_context(|| "failed to run the Orbit Swift linter")
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run_orbit_swiftlint(
    app: &AppContext,
    request_root: &Path,
    request: &OrbitSwiftLintRequest,
) -> Result<()> {
    let _ = (app, request_root, request);
    bail!("Orbit Swift linting is supported only on macOS hosts")
}

#[cfg(target_os = "macos")]
fn run_embedded_swift_tool(
    app: &AppContext,
    spec: &EmbeddedSwiftToolSpec,
    request_path: &Path,
) -> Result<()> {
    ensure_embedded_swift_tool_available(spec)?;
    let library_path = ensure_swift_tool_library(app, spec)?;
    let request_path = CString::new(request_path.as_os_str().as_bytes())
        .context("Swift tool request path contained an unexpected NUL byte")?;
    // Keep the library alive while invoking the exported C ABI entrypoint.
    let status = unsafe {
        let library = Library::new(&library_path)
            .with_context(|| format!("failed to load {}", library_path.display()))?;
        let entrypoint = library
            .get::<SwiftToolEntryPoint>(spec.symbol_name)
            .with_context(|| format!("missing FFI symbol for `{}`", spec.name))?;
        entrypoint(request_path.as_ptr())
    };
    if status != 0 {
        bail!(
            "embedded Orbit-managed tool `{}` failed with exit code {}",
            spec.name,
            status
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_embedded_swift_tool_available(spec: &EmbeddedSwiftToolSpec) -> Result<()> {
    if !spec.available {
        bail!("{}", spec.unavailable_reason);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_swift_tool_library(app: &AppContext, spec: &EmbeddedSwiftToolSpec) -> Result<PathBuf> {
    let cache_root = app.global_paths.cache_dir.join("swift-tools").join(format!(
        "{}-{}",
        spec.name,
        embedded_tool_hash(spec.bytes)
    ));
    ensure_dir(&cache_root)?;
    let library_path = cache_root.join(spec.file_name);
    if library_path.exists() {
        return Ok(library_path);
    }

    let temporary_path = cache_root.join(format!("{}.tmp", spec.file_name));
    fs::write(&temporary_path, spec.bytes)
        .with_context(|| format!("failed to write {}", temporary_path.display()))?;
    fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("failed to update {}", temporary_path.display()))?;
    if let Err(error) = fs::rename(&temporary_path, &library_path) {
        if library_path.exists() {
            let _ = fs::remove_file(&temporary_path);
            return Ok(library_path);
        }
        return Err(error).with_context(|| {
            format!(
                "failed to move {} into place at {}",
                temporary_path.display(),
                library_path.display()
            )
        });
    }
    Ok(library_path)
}

#[cfg(target_os = "macos")]
fn embedded_tool_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use std::sync::{Mutex, OnceLock};

    use super::*;
    #[cfg(target_os = "macos")]
    use crate::context::{AppContext, GlobalPaths};

    #[cfg(target_os = "macos")]
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    #[cfg(target_os = "macos")]
    const DEBUG_SWIFT_TOOL_UNAVAILABLE_MESSAGE: &str =
        "Orbit debug builds skip embedded Swift quality tooling";

    #[cfg(target_os = "macos")]
    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    #[cfg(target_os = "macos")]
    fn fixture_app(root: &Path) -> AppContext {
        let data_dir = root.join("data");
        let cache_dir = root.join("cache");
        let schema_dir = root.join("schemas");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&cache_dir).unwrap();
        std::fs::create_dir_all(&schema_dir).unwrap();
        AppContext {
            cwd: root.to_path_buf(),
            interactive: false,
            verbose: false,
            manifest_env: None,
            global_paths: GlobalPaths {
                data_dir: data_dir.clone(),
                cache_dir,
                schema_dir,
                auth_state_path: data_dir.join("auth.json"),
                device_cache_path: data_dir.join("devices.json"),
                keychain_path: data_dir.join("orbit.keychain-db"),
            },
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn embedded_swiftlint_ffi_handles_mock_request() {
        let _guard = env_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("mock.log");
        let app = fixture_app(temp.path());

        unsafe {
            std::env::set_var("MOCK_LOG", &log_path);
        }

        let result = run_orbit_swiftlint(
            &app,
            temp.path(),
            &OrbitSwiftLintRequest {
                working_directory: temp.path().to_path_buf(),
                configuration_json: None,
                files: vec![temp.path().join("Example.swift")],
                compiler_invocations: vec![OrbitSwiftLintCompilerInvocation {
                    arguments: vec!["swiftc".to_owned(), "-sdk".to_owned()],
                    source_files: vec![temp.path().join("Example.swift")],
                }],
            },
        );

        unsafe {
            std::env::remove_var("MOCK_LOG");
        }

        if cfg!(debug_assertions) {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(DEBUG_SWIFT_TOOL_UNAVAILABLE_MESSAGE));
            return;
        }

        result.unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("orbit-swiftlint request:"));
        assert!(log.contains("\"compiler_invocations\""));
        assert!(log.contains("\"swiftc\""));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn embedded_swiftformat_ffi_handles_mock_request() {
        let _guard = env_lock().lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let log_path = temp.path().join("mock.log");
        let app = fixture_app(temp.path());

        unsafe {
            std::env::set_var("MOCK_LOG", &log_path);
        }

        let result = run_orbit_swift_format(
            &app,
            temp.path(),
            &OrbitSwiftFormatRequest {
                working_directory: temp.path().to_path_buf(),
                configuration_json: None,
                mode: OrbitSwiftFormatMode::Check,
                files: vec![temp.path().join("Example.swift")],
            },
        );

        unsafe {
            std::env::remove_var("MOCK_LOG");
        }

        if cfg!(debug_assertions) {
            let error = result.unwrap_err().to_string();
            assert!(error.contains(DEBUG_SWIFT_TOOL_UNAVAILABLE_MESSAGE));
            return;
        }

        result.unwrap();
        let log = std::fs::read_to_string(&log_path).unwrap();
        assert!(log.contains("orbit-swift-format request:"));
        assert!(log.contains("\"mode\": \"check\""));
    }
}
