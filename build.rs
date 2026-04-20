use std::env;
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct SwiftToolSpec {
    command_name: &'static str,
    package_dir: &'static str,
    product: &'static str,
    dylib_name: &'static str,
    embedded_file_name: &'static str,
    rust_bytes_name: &'static str,
    rust_file_name_name: &'static str,
    rust_available_name: &'static str,
    rust_unavailable_reason_name: &'static str,
    prebuilt_path_env_var: &'static str,
}

struct ResolvedSwiftToolLibrary {
    path: PathBuf,
    cleanup_root: Option<PathBuf>,
}

const ORBI_SWIFT_TOOLS_PREBUILT_DIR_ENV: &str = "ORBI_SWIFT_TOOLS_PREBUILT_DIR";
const SWIFT_BUILD_PROGRESS_INTERVAL: Duration = Duration::from_secs(30);

const SWIFT_TOOL_SPECS: &[SwiftToolSpec] = &[
    SwiftToolSpec {
        command_name: "orbi lint",
        package_dir: "tools/orbi-swiftlint",
        product: "OrbiSwiftLintFFI",
        dylib_name: "libOrbiSwiftLintFFI.dylib",
        embedded_file_name: "OrbiSwiftLintFFI.dylib",
        rust_bytes_name: "ORBI_SWIFTLINT_FFI_BYTES",
        rust_file_name_name: "ORBI_SWIFTLINT_FFI_FILE_NAME",
        rust_available_name: "ORBI_SWIFTLINT_FFI_AVAILABLE",
        rust_unavailable_reason_name: "ORBI_SWIFTLINT_FFI_UNAVAILABLE_REASON",
        prebuilt_path_env_var: "ORBI_SWIFTLINT_FFI_PREBUILT_PATH",
    },
    SwiftToolSpec {
        command_name: "orbi format",
        package_dir: "tools/orbi-swift-format",
        product: "OrbiSwiftFormatFFI",
        dylib_name: "libOrbiSwiftFormatFFI.dylib",
        embedded_file_name: "OrbiSwiftFormatFFI.dylib",
        rust_bytes_name: "ORBI_SWIFTFORMAT_FFI_BYTES",
        rust_file_name_name: "ORBI_SWIFTFORMAT_FFI_FILE_NAME",
        rust_available_name: "ORBI_SWIFTFORMAT_FFI_AVAILABLE",
        rust_unavailable_reason_name: "ORBI_SWIFTFORMAT_FFI_UNAVAILABLE_REASON",
        prebuilt_path_env_var: "ORBI_SWIFTFORMAT_FFI_PREBUILT_PATH",
    },
];

fn main() -> Result<(), Box<dyn Error>> {
    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    emit_prebuilt_rerun_hints();
    if target_os != "macos" {
        write_unavailable_swift_tools(
            &out_dir,
            "Orbi Swift quality tooling is supported only on macOS hosts",
        )?;
        return Ok(());
    }

    let profile = env::var("PROFILE")?;
    if profile == "debug" {
        write_unavailable_swift_tools(
            &out_dir,
            "Orbi debug builds skip embedded Swift quality tooling to keep local iteration fast. Build Orbi with `cargo build --release` to use `orbi lint` and `orbi format`.",
        )?;
        return Ok(());
    }

    let target = env::var("TARGET")?;
    let target_dir = cargo_target_dir(&out_dir)?;
    let build_root = target_dir
        .join("swift-tool-build")
        .join(target)
        .join(profile);
    fs::create_dir_all(&build_root)?;

    let mut generated = String::new();
    for spec in SWIFT_TOOL_SPECS {
        let package_dir = manifest_dir.join(spec.package_dir);
        emit_rerun_if_changed(&package_dir)?;
        let resolved_library = resolve_swift_tool_library(
            &manifest_dir,
            &package_dir,
            &build_root.join(spec.product),
            spec,
        )?;
        let embedded_path = out_dir.join(spec.embedded_file_name);
        fs::copy(&resolved_library.path, &embedded_path)?;
        if let Some(cleanup_root) = resolved_library.cleanup_root {
            cleanup_swift_tool_build_root(&cleanup_root, spec)?;
        }
        generated.push_str(&generated_swift_tool_definition(
            spec,
            Some(&embedded_path),
            None,
        ));
    }

    fs::write(out_dir.join("embedded_swift_tools.rs"), generated)?;
    Ok(())
}

fn emit_prebuilt_rerun_hints() {
    println!("cargo:rerun-if-env-changed={ORBI_SWIFT_TOOLS_PREBUILT_DIR_ENV}");
    for spec in SWIFT_TOOL_SPECS {
        println!("cargo:rerun-if-env-changed={}", spec.prebuilt_path_env_var);
    }
}

fn write_unavailable_swift_tools(out_dir: &Path, reason: &str) -> Result<(), Box<dyn Error>> {
    let generated = SWIFT_TOOL_SPECS
        .iter()
        .map(|spec| generated_swift_tool_definition(spec, None, Some(reason)))
        .collect::<String>();
    fs::write(out_dir.join("embedded_swift_tools.rs"), generated)?;
    Ok(())
}

fn generated_swift_tool_definition(
    spec: &SwiftToolSpec,
    embedded_path: Option<&Path>,
    unavailable_reason: Option<&str>,
) -> String {
    let available = embedded_path.is_some();
    let bytes = embedded_path.map_or_else(
        || "&[]".to_owned(),
        |path| format!("include_bytes!(r#\"{}\"#)", path.display()),
    );
    let unavailable_reason = unavailable_reason.unwrap_or("");
    format!(
        "pub(crate) const {}: &[u8] = {bytes};\n\
         pub(crate) const {}: &str = {:?};\n\
         pub(crate) const {}: bool = {};\n\
         pub(crate) const {}: &str = {:?};\n",
        spec.rust_bytes_name,
        spec.rust_file_name_name,
        spec.dylib_name,
        spec.rust_available_name,
        available,
        spec.rust_unavailable_reason_name,
        unavailable_reason,
    )
}

fn cargo_target_dir(out_dir: &Path) -> Result<PathBuf, Box<dyn Error>> {
    out_dir
        .ancestors()
        .nth(4)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "failed to derive Cargo target dir from {}",
                out_dir.display()
            )
            .into()
        })
}

fn build_swift_tool_library(
    package_dir: &Path,
    build_root: &Path,
    spec: &SwiftToolSpec,
) -> Result<PathBuf, Box<dyn Error>> {
    let scratch_path = build_root.join("scratch");
    let cache_path = build_root.join("dependency-cache");
    fs::create_dir_all(&scratch_path)?;
    fs::create_dir_all(&cache_path)?;

    let mut build_command = base_swift_build_command(package_dir, &scratch_path, &cache_path, spec);
    run_command(&mut build_command, spec)?;

    let mut show_bin_command =
        base_swift_build_command(package_dir, &scratch_path, &cache_path, spec);
    show_bin_command.arg("--show-bin-path");
    let bin_dir = command_output(&mut show_bin_command)?;
    let dylib_path = PathBuf::from(bin_dir.trim()).join(spec.dylib_name);
    if !dylib_path.exists() {
        return Err(format!(
            "swift build reported `{}` but {} was not found at {}",
            spec.product,
            spec.dylib_name,
            dylib_path.display()
        )
        .into());
    }
    Ok(dylib_path)
}

fn resolve_swift_tool_library(
    manifest_dir: &Path,
    package_dir: &Path,
    build_root: &Path,
    spec: &SwiftToolSpec,
) -> Result<ResolvedSwiftToolLibrary, Box<dyn Error>> {
    if let Some(prebuilt_path) = resolve_prebuilt_swift_tool_library(manifest_dir, spec)? {
        println!("cargo:rerun-if-changed={}", prebuilt_path.display());
        return Ok(ResolvedSwiftToolLibrary {
            path: prebuilt_path,
            cleanup_root: None,
        });
    }

    Ok(ResolvedSwiftToolLibrary {
        path: build_swift_tool_library(package_dir, build_root, spec)?,
        cleanup_root: Some(build_root.to_path_buf()),
    })
}

fn cleanup_swift_tool_build_root(
    build_root: &Path,
    spec: &SwiftToolSpec,
) -> Result<(), Box<dyn Error>> {
    emit_swift_build_status(
        &format!(
            "cleaning SwiftPM build artifacts for `{}` at {}.",
            spec.product,
            build_root.display()
        ),
        false,
    );
    match fs::remove_dir_all(build_root) {
        Ok(()) => {
            emit_swift_build_status(
                &format!("cleaned SwiftPM build artifacts for `{}`.", spec.product),
                false,
            );
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!(
            "failed to clean SwiftPM build artifacts for `{}` at {}: {err}",
            spec.product,
            build_root.display()
        )
        .into()),
    }
}

fn resolve_prebuilt_swift_tool_library(
    manifest_dir: &Path,
    spec: &SwiftToolSpec,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    if let Some(path) = env::var_os(spec.prebuilt_path_env_var) {
        let path = resolve_env_path(manifest_dir, PathBuf::from(path));
        return Ok(Some(validate_prebuilt_swift_tool_library(&path, spec)?));
    }

    if let Some(dir) = env::var_os(ORBI_SWIFT_TOOLS_PREBUILT_DIR_ENV) {
        let path = resolve_env_path(manifest_dir, PathBuf::from(dir)).join(spec.dylib_name);
        return Ok(Some(validate_prebuilt_swift_tool_library(&path, spec)?));
    }

    Ok(None)
}

fn validate_prebuilt_swift_tool_library(
    path: &Path,
    spec: &SwiftToolSpec,
) -> Result<PathBuf, Box<dyn Error>> {
    if !path.is_file() {
        return Err(format!(
            "expected a prebuilt library for `{}` at {}",
            spec.command_name,
            path.display()
        )
        .into());
    }
    Ok(path.to_path_buf())
}

fn resolve_env_path(manifest_dir: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        manifest_dir.join(path)
    }
}

fn base_swift_build_command(
    package_dir: &Path,
    scratch_path: &Path,
    cache_path: &Path,
    spec: &SwiftToolSpec,
) -> Command {
    let mut command = Command::new("swift");
    command
        .arg("build")
        .arg("--disable-keychain")
        .arg("--package-path")
        .arg(package_dir)
        .arg("--scratch-path")
        .arg(scratch_path)
        .arg("--cache-path")
        .arg(cache_path)
        .arg("--configuration")
        .arg("release")
        .arg("--product")
        .arg(spec.product);
    command
}

fn run_command(command: &mut Command, spec: &SwiftToolSpec) -> Result<(), Box<dyn Error>> {
    let debug = debug_command(command);
    emit_swift_build_status(
        &format!(
            "building embedded Swift tool `{}` for `{}`. First release build can take several minutes while SwiftPM compiles dependencies like SwiftSyntax.",
            spec.product, spec.command_name
        ),
        true,
    );

    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|err| format!("failed to start `{debug}`: {err}"))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        format!(
            "failed to capture stdout for `{}` while building `{}`",
            debug, spec.product
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        format!(
            "failed to capture stderr for `{}` while building `{}`",
            debug, spec.product
        )
    })?;
    let stdout_reader = thread::spawn(move || read_and_forward(stdout));
    let stderr_reader = thread::spawn(move || read_and_forward(stderr));

    let started = Instant::now();
    let mut next_progress = SWIFT_BUILD_PROGRESS_INTERVAL;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }

        let elapsed = started.elapsed();
        if elapsed >= next_progress {
            emit_swift_build_status(
                &format!(
                    "still building embedded Swift tool `{}` after {}s.",
                    spec.product,
                    elapsed.as_secs()
                ),
                false,
            );
            next_progress += SWIFT_BUILD_PROGRESS_INTERVAL;
        }
        thread::sleep(Duration::from_secs(1));
    };

    let stdout = join_reader(stdout_reader, "stdout", &debug)?;
    let stderr = join_reader(stderr_reader, "stderr", &debug)?;
    if !status.success() {
        return Err(format!(
            "`{debug}` failed with {}\nstdout:\n{}\nstderr:\n{}",
            status,
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        )
        .into());
    }

    emit_swift_build_status(
        &format!(
            "finished embedded Swift tool `{}` in {}s.",
            spec.product,
            started.elapsed().as_secs()
        ),
        true,
    );
    Ok(())
}

fn emit_swift_build_status(message: &str, emit_cargo_warning: bool) {
    // Cargo buffers build-script stdout/stderr until the script exits. Writing
    // directly to the controlling terminal keeps long SwiftPM builds visible.
    let line = format!("orbi build: {message}\n");
    let wrote_to_terminal = write_to_terminal(line.as_bytes()).is_ok();
    if emit_cargo_warning && !wrote_to_terminal {
        println!("cargo:warning=orbi build: {message}");
    }
}

fn write_to_terminal(bytes: &[u8]) -> io::Result<()> {
    let mut terminal = fs::OpenOptions::new().write(true).open("/dev/tty")?;
    terminal.write_all(bytes)?;
    terminal.flush()
}

fn read_and_forward<R: Read>(mut reader: R) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    let mut stderr = io::stderr().lock();
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..bytes_read]);
        let _ = stderr.write_all(&buffer[..bytes_read]);
        let _ = stderr.flush();
    }
    Ok(output)
}

fn join_reader(
    reader: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream_name: &str,
    debug: &str,
) -> Result<Vec<u8>, Box<dyn Error>> {
    reader
        .join()
        .map_err(|_| format!("reader thread panicked while capturing {stream_name} for `{debug}`"))?
        .map_err(|err| format!("failed to read {stream_name} for `{debug}`: {err}").into())
}

fn command_output(command: &mut Command) -> Result<String, Box<dyn Error>> {
    let debug = debug_command(command);
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "`{debug}` failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn debug_command(command: &Command) -> String {
    let program = command.get_program().to_string_lossy();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ");
    if args.is_empty() {
        program.into_owned()
    } else {
        format!("{program} {args}")
    }
}

fn emit_rerun_if_changed(root: &Path) -> Result<(), Box<dyn Error>> {
    if root.is_file() {
        println!("cargo:rerun-if-changed={}", root.display());
        return Ok(());
    }

    let mut entries = vec![root.to_path_buf()];
    while let Some(path) = entries.pop() {
        if path.is_dir() {
            for entry in fs::read_dir(&path)? {
                entries.push(entry?.path());
            }
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(())
}
