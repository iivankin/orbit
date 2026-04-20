use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::apple::build::external::{ExternalLinkInputs, apply_external_compile_inputs};
use crate::apple::build::toolchain::Toolchain;
use crate::manifest::ProfileManifest;
use crate::util::{ensure_parent_dir, read_json_file_if_exists, write_json_file};

const CLANG_OBJECT_CACHE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClangSourceLanguage {
    C,
    ObjectiveC,
    ObjectiveCpp,
    Cpp,
}

#[derive(Debug, Clone)]
pub(crate) struct ClangInvocation {
    pub args: Vec<OsString>,
    pub source_file: PathBuf,
    pub output_path: PathBuf,
    pub depfile_path: Option<PathBuf>,
    pub language: ClangSourceLanguage,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClangCompilePlan<'a> {
    pub source_file: &'a Path,
    pub output_path: &'a Path,
    pub depfile_path: Option<&'a Path>,
    pub language: ClangSourceLanguage,
    pub external_link_inputs: &'a ExternalLinkInputs,
    pub index_store_path: Option<&'a Path>,
}

#[derive(Debug, Default)]
pub(crate) struct ClangCompileSummary {
    pub object_files: Vec<PathBuf>,
    pub compiled_count: usize,
    pub reused_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ClangObjectCacheInfo {
    version: u32,
    command_fingerprint: String,
    dependencies: Vec<TrackedPathFingerprint>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TrackedPathFingerprint {
    path: PathBuf,
    state: TrackedPathState,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
enum TrackedPathState {
    Missing,
    Directory,
    Symlink {
        target: PathBuf,
    },
    File {
        size_bytes: u64,
        modified_unix_seconds: u64,
        modified_subsec_nanos: u32,
    },
}

impl ClangSourceLanguage {
    pub(crate) fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "c" => Some(Self::C),
            "m" => Some(Self::ObjectiveC),
            "mm" => Some(Self::ObjectiveCpp),
            "cpp" | "cc" | "cxx" => Some(Self::Cpp),
            _ => None,
        }
    }

    pub(crate) fn language_id(self) -> &'static str {
        match self {
            Self::C => "c",
            Self::ObjectiveC => "objective-c",
            Self::ObjectiveCpp => "objective-cpp",
            Self::Cpp => "cpp",
        }
    }

    fn uses_cpp_driver(self) -> bool {
        matches!(self, Self::ObjectiveCpp | Self::Cpp)
    }
}

impl ClangInvocation {
    pub(crate) fn command(&self, toolchain: &Toolchain) -> Command {
        let mut command = toolchain.clang(self.language.uses_cpp_driver());
        command.args(&self.args);
        command
    }
}

pub(crate) fn target_clang_invocation(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    plan: ClangCompilePlan<'_>,
) -> Result<ClangInvocation> {
    ensure_parent_dir(plan.output_path)?;
    if let Some(depfile_path) = plan.depfile_path {
        ensure_parent_dir(depfile_path)?;
    }
    let mut args = vec![
        "-target".into(),
        toolchain.target_triple.clone().into(),
        "-isysroot".into(),
        toolchain.sdk_path.as_os_str().to_os_string(),
    ];
    if let Some(index_store_path) = plan.index_store_path {
        ensure_parent_dir(&index_store_path.join("placeholder"))?;
        args.push("-index-store-path".into());
        args.push(index_store_path.as_os_str().to_os_string());
    }
    apply_external_compile_inputs(&mut args, plan.external_link_inputs);
    if let Some(depfile_path) = plan.depfile_path {
        args.push("-MMD".into());
        args.push("-MF".into());
        args.push(depfile_path.as_os_str().to_os_string());
    }
    args.push("-c".into());
    args.push(plan.source_file.as_os_str().to_os_string());
    args.push("-o".into());
    args.push(plan.output_path.as_os_str().to_os_string());
    if profile.is_debug() {
        args.push("-g".into());
    } else {
        args.push("-O2".into());
    }

    Ok(ClangInvocation {
        args,
        source_file: plan.source_file.to_path_buf(),
        output_path: plan.output_path.to_path_buf(),
        depfile_path: plan.depfile_path.map(Path::to_path_buf),
        language: plan.language,
    })
}

pub(crate) fn object_file_name(source: &Path) -> Result<String> {
    source
        .file_name()
        .and_then(OsStr::to_str)
        .map(|value| format!("{value}.o"))
        .context("failed to derive object file name")
}

pub(crate) fn object_depfile_path(output_path: &Path) -> PathBuf {
    object_path_with_suffix(output_path, "d")
}

pub(crate) fn cached_object_can_be_reused(
    toolchain: &Toolchain,
    invocation: &ClangInvocation,
) -> Result<bool> {
    if invocation.depfile_path.is_none() || !invocation.output_path.exists() {
        return Ok(false);
    }

    let Some(cache_info) = read_json_file_if_exists::<ClangObjectCacheInfo>(
        &object_cache_info_path(&invocation.output_path),
    )?
    else {
        return Ok(false);
    };

    if cache_info.version != CLANG_OBJECT_CACHE_VERSION
        || cache_info.command_fingerprint != object_command_fingerprint(toolchain, invocation)
    {
        return Ok(false);
    }

    for dependency in &cache_info.dependencies {
        if tracked_path_state(&dependency.path)? != dependency.state {
            return Ok(false);
        }
    }

    Ok(true)
}

pub(crate) fn write_object_cache(
    toolchain: &Toolchain,
    invocation: &ClangInvocation,
) -> Result<()> {
    let Some(depfile_path) = &invocation.depfile_path else {
        return Ok(());
    };
    let Some(mut dependency_paths) = load_depfile_dependencies(depfile_path)
        .with_context(|| format!("failed to load depfile {}", depfile_path.display()))?
    else {
        return Ok(());
    };

    if !dependency_paths.contains(&invocation.source_file) {
        dependency_paths.push(invocation.source_file.clone());
    }
    dependency_paths.sort();
    dependency_paths.dedup();

    let dependencies = dependency_paths
        .into_iter()
        .map(|path| {
            Ok(TrackedPathFingerprint {
                state: tracked_path_state(&path)?,
                path,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    write_json_file(
        &object_cache_info_path(&invocation.output_path),
        &ClangObjectCacheInfo {
            version: CLANG_OBJECT_CACHE_VERSION,
            command_fingerprint: object_command_fingerprint(toolchain, invocation),
            dependencies,
        },
    )
}

fn load_depfile_dependencies(depfile_path: &Path) -> Result<Option<Vec<PathBuf>>> {
    if !depfile_path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(depfile_path)
        .with_context(|| format!("failed to read {}", depfile_path.display()))?;
    let cwd = std::env::current_dir().context("failed to resolve depfile working directory")?;
    Ok(Some(
        parse_depfile_dependencies(&contents)?
            .into_iter()
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            })
            .collect(),
    ))
}

fn parse_depfile_dependencies(contents: &str) -> Result<Vec<PathBuf>> {
    let Some(dependencies) = depfile_dependencies_segment(contents) else {
        anyhow::bail!("depfile did not contain a target separator");
    };

    let mut paths = Vec::new();
    let mut token = String::new();
    let mut chars = dependencies.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\\' => match chars.next() {
                Some('\n') => {}
                Some('\r') => {
                    if matches!(chars.peek(), Some('\n')) {
                        chars.next();
                    }
                }
                Some(escaped) => token.push(escaped),
                None => token.push('\\'),
            },
            character if character.is_whitespace() => {
                if !token.is_empty() {
                    paths.push(PathBuf::from(std::mem::take(&mut token)));
                }
            }
            _ => token.push(character),
        }
    }

    if !token.is_empty() {
        paths.push(PathBuf::from(token));
    }

    Ok(paths)
}

fn depfile_dependencies_segment(contents: &str) -> Option<&str> {
    let mut escaped = false;
    for (index, character) in contents.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            ':' => return Some(&contents[index + character.len_utf8()..]),
            _ => {}
        }
    }
    None
}

fn object_command_fingerprint(toolchain: &Toolchain, invocation: &ClangInvocation) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CLANG_OBJECT_CACHE_VERSION.to_le_bytes());
    hasher.update(toolchain.platform.to_string().as_bytes());
    hasher.update(toolchain.destination.as_str().as_bytes());
    hasher.update(toolchain.sdk_name.as_bytes());
    hasher.update(toolchain.sdk_path.to_string_lossy().as_bytes());
    hasher.update(toolchain.deployment_target.as_bytes());
    hasher.update(toolchain.architecture.as_bytes());
    hasher.update(toolchain.target_triple.as_bytes());
    if let Some(selected_xcode) = &toolchain.selected_xcode {
        hasher.update(selected_xcode.version.as_bytes());
        hasher.update(selected_xcode.build_version.as_bytes());
        hasher.update(selected_xcode.developer_dir.to_string_lossy().as_bytes());
    } else {
        hasher.update(b"system-xcode");
    }
    if let Ok(cwd) = std::env::current_dir() {
        hasher.update(cwd.to_string_lossy().as_bytes());
    }
    hasher.update(invocation.language.language_id().as_bytes());
    for argument in &invocation.args {
        hasher.update([0]);
        hasher.update(argument.to_string_lossy().as_bytes());
    }
    hex_digest(hasher.finalize())
}

fn object_cache_info_path(output_path: &Path) -> PathBuf {
    object_path_with_suffix(output_path, "cache.json")
}

fn object_path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    match path.extension().and_then(OsStr::to_str) {
        Some(extension) => path.with_extension(format!("{extension}.{suffix}")),
        None => path.with_extension(suffix),
    }
}

fn tracked_path_state(path: &Path) -> Result<TrackedPathState> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(TrackedPathState::Missing),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to stat {}", path.display()));
        }
    };

    if metadata.file_type().is_symlink() {
        return Ok(TrackedPathState::Symlink {
            target: fs::read_link(path)
                .with_context(|| format!("failed to read symlink {}", path.display()))?,
        });
    }
    if metadata.is_dir() {
        return Ok(TrackedPathState::Directory);
    }

    let modified = metadata
        .modified()
        .with_context(|| format!("failed to read mtime for {}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .with_context(|| format!("mtime for {} was before UNIX_EPOCH", path.display()))?;
    Ok(TrackedPathState::File {
        size_bytes: metadata.len(),
        modified_unix_seconds: modified.as_secs(),
        modified_subsec_nanos: modified.subsec_nanos(),
    })
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::Duration;

    use super::{
        ClangCompilePlan, ClangSourceLanguage, cached_object_can_be_reused, object_depfile_path,
        object_file_name, parse_depfile_dependencies, target_clang_invocation, write_object_cache,
    };
    use crate::apple::build::external::ExternalLinkInputs;
    use crate::apple::build::toolchain::{DestinationKind, Toolchain};
    use crate::manifest::{ApplePlatform, BuildConfiguration, DistributionKind, ProfileManifest};

    fn fixture_toolchain() -> Toolchain {
        Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Simulator,
            sdk_name: "iphonesimulator".to_owned(),
            sdk_path: PathBuf::from("/Applications/Xcode.app/SDKs/iPhoneSimulator.sdk"),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0-simulator".to_owned(),
            selected_xcode: None,
        }
    }

    #[test]
    fn clang_invocation_captures_include_paths_and_index_store() {
        let depfile_path =
            object_depfile_path(Path::new(".orbi/build/App/intermediates/Bridge.m.o"));
        let invocation = target_clang_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
            ClangCompilePlan {
                source_file: Path::new("Sources/App/Bridge.m"),
                output_path: Path::new(".orbi/build/App/intermediates/Bridge.m.o"),
                depfile_path: Some(depfile_path.as_path()),
                language: ClangSourceLanguage::ObjectiveC,
                external_link_inputs: &ExternalLinkInputs {
                    module_search_paths: vec![PathBuf::from("/tmp/include")],
                    framework_search_paths: vec![PathBuf::from("/tmp/frameworks")],
                    ..ExternalLinkInputs::default()
                },
                index_store_path: Some(Path::new(".orbi/ide/index/store")),
            },
        )
        .unwrap();
        let args = invocation
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.windows(2).any(|pair| pair == ["-I", "/tmp/include"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-F", "/tmp/frameworks"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-index-store-path", ".orbi/ide/index/store"])
        );
        assert!(args.iter().any(|arg| arg == "-MMD"));
        assert!(
            args.windows(2)
                .any(|pair| { pair == ["-MF", ".orbi/build/App/intermediates/Bridge.m.o.d",] })
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", ".orbi/build/App/intermediates/Bridge.m.o"])
        );
    }

    #[test]
    fn object_file_name_uses_source_file_name() {
        assert_eq!(
            object_file_name(Path::new("Sources/App/Bridge.m")).unwrap(),
            "Bridge.m.o"
        );
    }

    #[test]
    fn depfile_parser_handles_escaped_paths_and_continuations() {
        let dependencies = parse_depfile_dependencies(
            "/tmp/build/Bridge.m.o: /tmp/project/Sources/App/Bridge.m /tmp/project/Headers/Bridge\\ Header.h \\\n /tmp/project/Headers/Sub\\:Header.h\n",
        )
        .unwrap();

        assert_eq!(
            dependencies,
            vec![
                PathBuf::from("/tmp/project/Sources/App/Bridge.m"),
                PathBuf::from("/tmp/project/Headers/Bridge Header.h"),
                PathBuf::from("/tmp/project/Headers/Sub:Header.h"),
            ]
        );
    }

    #[test]
    fn object_cache_invalidates_when_dependency_changes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("Bridge.m");
        let header = temp.path().join("Bridge.h");
        let output_path = temp.path().join("Bridge.m.o");
        let depfile_path = object_depfile_path(&output_path);
        fs::write(
            &source,
            "#import \"Bridge.h\"\nint orbi_add(int a, int b) { return a + b; }\n",
        )
        .unwrap();
        fs::write(&header, "int orbi_add(int a, int b);\n").unwrap();
        fs::write(&output_path, "object").unwrap();
        fs::write(
            &depfile_path,
            format!(
                "{}: {} {}\n",
                output_path.display(),
                source.display(),
                header.display()
            ),
        )
        .unwrap();

        let invocation = target_clang_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
            ClangCompilePlan {
                source_file: &source,
                output_path: &output_path,
                depfile_path: Some(depfile_path.as_path()),
                language: ClangSourceLanguage::ObjectiveC,
                external_link_inputs: &ExternalLinkInputs::default(),
                index_store_path: None,
            },
        )
        .unwrap();

        write_object_cache(&fixture_toolchain(), &invocation).unwrap();
        assert!(cached_object_can_be_reused(&fixture_toolchain(), &invocation).unwrap());

        thread::sleep(Duration::from_millis(10));
        fs::write(
            &header,
            "int orbi_add(int a, int b);\nint orbi_subtract(int a, int b);\n",
        )
        .unwrap();

        assert!(!cached_object_can_be_reused(&fixture_toolchain(), &invocation).unwrap());
    }
}
