use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::apple::build::external::{ExternalLinkInputs, apply_external_compile_inputs};
use crate::apple::build::toolchain::Toolchain;
use crate::manifest::ProfileManifest;
use crate::util::ensure_parent_dir;

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
    pub language: ClangSourceLanguage,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ClangCompilePlan<'a> {
    pub source_file: &'a Path,
    pub output_path: &'a Path,
    pub language: ClangSourceLanguage,
    pub external_link_inputs: &'a ExternalLinkInputs,
    pub index_store_path: Option<&'a Path>,
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

#[cfg(test)]
mod tests {
    use super::{ClangCompilePlan, ClangSourceLanguage, object_file_name, target_clang_invocation};
    use crate::apple::build::external::ExternalLinkInputs;
    use crate::apple::build::toolchain::{DestinationKind, Toolchain};
    use crate::manifest::{ApplePlatform, BuildConfiguration, DistributionKind, ProfileManifest};
    use std::path::{Path, PathBuf};

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
        let invocation = target_clang_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
            ClangCompilePlan {
                source_file: Path::new("Sources/App/Bridge.m"),
                output_path: Path::new(".orbit/build/App/intermediates/Bridge.m.o"),
                language: ClangSourceLanguage::ObjectiveC,
                external_link_inputs: &ExternalLinkInputs {
                    module_search_paths: vec![PathBuf::from("/tmp/include")],
                    framework_search_paths: vec![PathBuf::from("/tmp/frameworks")],
                    ..ExternalLinkInputs::default()
                },
                index_store_path: Some(Path::new(".orbit/ide/index/store")),
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
                .any(|pair| pair == ["-index-store-path", ".orbit/ide/index/store"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-o", ".orbit/build/App/intermediates/Bridge.m.o"])
        );
    }

    #[test]
    fn object_file_name_uses_source_file_name() {
        assert_eq!(
            object_file_name(Path::new("Sources/App/Bridge.m")).unwrap(),
            "Bridge.m.o"
        );
    }
}
