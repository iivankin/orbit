use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::apple::build::external::{ExternalLinkInputs, PackageBuildOutput};
use crate::apple::build::toolchain::Toolchain;
use crate::manifest::{ProfileManifest, TargetKind};
use crate::util::ensure_parent_dir;

#[derive(Debug, Clone)]
pub(crate) struct SwiftcInvocation {
    pub args: Vec<OsString>,
    pub source_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SwiftTargetCompilePlan<'a> {
    pub target_kind: TargetKind,
    pub use_nsextension_main: bool,
    pub module_name: &'a str,
    pub product_path: &'a Path,
    pub module_output_path: Option<&'a Path>,
    pub swift_sources: &'a [PathBuf],
    pub package_outputs: &'a [PackageBuildOutput],
    pub external_link_inputs: &'a ExternalLinkInputs,
    pub object_files: &'a [PathBuf],
    pub index_store_path: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SwiftPackageTargetCompilePlan<'a> {
    pub module_name: &'a str,
    pub product_path: &'a Path,
    pub module_output_path: &'a Path,
    pub swift_sources: &'a [PathBuf],
    pub module_search_paths: &'a [PathBuf],
    pub library_search_paths: &'a [PathBuf],
    pub link_libraries: &'a [String],
    pub index_store_path: Option<&'a Path>,
}

impl SwiftcInvocation {
    pub(crate) fn command(&self, toolchain: &Toolchain) -> Command {
        let mut command = toolchain.swiftc();
        command.args(&self.args);
        command
    }
}

pub(crate) fn target_swiftc_invocation(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    plan: SwiftTargetCompilePlan<'_>,
) -> Result<SwiftcInvocation> {
    let mut args = base_swiftc_args(toolchain, profile, plan.module_name);

    match plan.target_kind {
        TargetKind::StaticLibrary => {
            args.push("-emit-library".into());
            args.push("-static".into());
        }
        TargetKind::DynamicLibrary | TargetKind::Framework => {
            args.push("-emit-library".into());
        }
        _ => {}
    }
    if matches!(
        plan.target_kind,
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Framework
    ) {
        args.push("-emit-module".into());
        if let Some(module_output_path) = plan.module_output_path {
            ensure_parent_dir(module_output_path)?;
            args.push("-emit-module-path".into());
            args.push(module_output_path.as_os_str().to_os_string());
        }
    }
    if let Some(index_store_path) = plan.index_store_path {
        ensure_parent_dir(&index_store_path.join("placeholder"))?;
        args.push("-index-store-path".into());
        args.push(index_store_path.as_os_str().to_os_string());
    }
    args.push("-o".into());
    args.push(plan.product_path.as_os_str().to_os_string());
    if plan.use_nsextension_main {
        // Extension bundles do not define `main`; the system loader enters through NSExtensionMain.
        args.push("-Xlinker".into());
        args.push("-e".into());
        args.push("-Xlinker".into());
        args.push("_NSExtensionMain".into());
    }
    for package in plan.package_outputs {
        args.push("-I".into());
        args.push(package.module_dir.as_os_str().to_os_string());
        args.push("-L".into());
        args.push(package.library_dir.as_os_str().to_os_string());
        for library in &package.link_libraries {
            args.push("-l".into());
            args.push(library.into());
        }
    }
    append_external_link_inputs(&mut args, plan.external_link_inputs);
    append_paths(&mut args, plan.object_files);
    append_paths(&mut args, plan.swift_sources);

    Ok(SwiftcInvocation {
        args,
        source_files: plan.swift_sources.to_vec(),
    })
}

pub(crate) fn package_target_swiftc_invocation(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    plan: SwiftPackageTargetCompilePlan<'_>,
) -> Result<SwiftcInvocation> {
    ensure_parent_dir(plan.module_output_path)?;
    let mut args = base_swiftc_args(toolchain, profile, plan.module_name);
    args.push("-emit-library".into());
    args.push("-static".into());
    args.push("-emit-module".into());
    if let Some(index_store_path) = plan.index_store_path {
        ensure_parent_dir(&index_store_path.join("placeholder"))?;
        args.push("-index-store-path".into());
        args.push(index_store_path.as_os_str().to_os_string());
    }
    args.push("-o".into());
    args.push(plan.product_path.as_os_str().to_os_string());
    args.push("-emit-module-path".into());
    args.push(plan.module_output_path.as_os_str().to_os_string());
    append_flagged_paths(&mut args, "-I", plan.module_search_paths);
    append_flagged_paths(&mut args, "-L", plan.library_search_paths);
    for library in plan.link_libraries {
        args.push("-l".into());
        args.push(library.into());
    }
    append_paths(&mut args, plan.swift_sources);

    Ok(SwiftcInvocation {
        args,
        source_files: plan.swift_sources.to_vec(),
    })
}

fn base_swiftc_args(
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    module_name: &str,
) -> Vec<OsString> {
    let mut args = vec![
        "-parse-as-library".into(),
        "-target".into(),
        toolchain.target_triple.clone().into(),
        "-sdk".into(),
        toolchain.sdk_path.as_os_str().to_os_string(),
        "-module-name".into(),
        module_name.into(),
    ];
    if profile.is_debug() {
        args.push("-Onone".into());
        args.push("-g".into());
    } else {
        args.push("-O".into());
    }
    args
}

fn append_external_link_inputs(args: &mut Vec<OsString>, inputs: &ExternalLinkInputs) {
    append_flagged_paths(args, "-I", &inputs.module_search_paths);
    append_flagged_paths(args, "-F", &inputs.framework_search_paths);
    append_flagged_paths(args, "-L", &inputs.library_search_paths);
    for framework in &inputs.link_frameworks {
        args.push("-framework".into());
        args.push(framework.into());
    }
    for framework in &inputs.weak_frameworks {
        args.push("-weak_framework".into());
        args.push(framework.into());
    }
    for library in &inputs.link_libraries {
        args.push("-l".into());
        args.push(library.into());
    }
}

fn append_flagged_paths(args: &mut Vec<OsString>, flag: &str, paths: &[PathBuf]) {
    for path in paths {
        args.push(flag.into());
        args.push(path.as_os_str().to_os_string());
    }
}

fn append_paths(args: &mut Vec<OsString>, paths: &[PathBuf]) {
    for path in paths {
        args.push(path.as_os_str().to_os_string());
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        SwiftPackageTargetCompilePlan, SwiftTargetCompilePlan, package_target_swiftc_invocation,
        target_swiftc_invocation,
    };
    use crate::apple::build::external::{ExternalLinkInputs, PackageBuildOutput};
    use crate::apple::build::toolchain::{DestinationKind, Toolchain};
    use crate::manifest::{
        ApplePlatform, BuildConfiguration, DistributionKind, ProfileManifest, TargetKind,
    };

    fn fixture_toolchain() -> Toolchain {
        Toolchain {
            platform: ApplePlatform::Ios,
            destination: DestinationKind::Simulator,
            sdk_name: "iphonesimulator".to_owned(),
            sdk_path: "/Applications/Xcode.app/SDK".into(),
            deployment_target: "18.0".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "arm64-apple-ios18.0-simulator".to_owned(),
            selected_xcode: None,
        }
    }

    #[test]
    fn target_invocation_captures_extension_link_and_dependencies() {
        let external = ExternalLinkInputs {
            module_search_paths: vec!["/tmp/modules".into()],
            framework_search_paths: vec!["/tmp/frameworks".into()],
            library_search_paths: vec!["/tmp/libs".into()],
            link_frameworks: vec!["Network".to_owned()],
            weak_frameworks: vec!["SwiftUI".to_owned()],
            link_libraries: vec!["sqlite3".to_owned()],
            embedded_payloads: Vec::new(),
        };
        let package_outputs = vec![PackageBuildOutput {
            module_dir: "/tmp/package/modules".into(),
            library_dir: "/tmp/package/libs".into(),
            link_libraries: vec!["PackageCore".to_owned()],
        }];
        let invocation = target_swiftc_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
            SwiftTargetCompilePlan {
                target_kind: TargetKind::AppExtension,
                use_nsextension_main: true,
                module_name: "ShareExtension",
                product_path: "/tmp/ShareExtension.appex/ShareExtension".as_ref(),
                module_output_path: None,
                swift_sources: &["/tmp/Sources/Extension.swift".into()],
                package_outputs: &package_outputs,
                external_link_inputs: &external,
                object_files: &["/tmp/bridge.o".into()],
                index_store_path: Some(Path::new("/tmp/index-store")),
            },
        )
        .unwrap();

        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-module-name", "ShareExtension"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-sdk", "/Applications/Xcode.app/SDK"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-index-store-path", "/tmp/index-store"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-l", "PackageCore"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-framework", "Network"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-weak_framework", "SwiftUI"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-l", "sqlite3"]));
        assert!(args.windows(2).any(|pair| pair == ["-Xlinker", "-e"]));
        assert!(args.contains(&"/tmp/bridge.o".to_owned()));
        assert!(args.contains(&"/tmp/Sources/Extension.swift".to_owned()));
    }

    #[test]
    fn target_invocation_omits_nsextension_main_for_widget_runtime_without_entry() {
        let invocation = target_swiftc_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
            SwiftTargetCompilePlan {
                target_kind: TargetKind::WidgetExtension,
                use_nsextension_main: false,
                module_name: "WidgetExtension",
                product_path: "/tmp/WidgetExtension.appex/WidgetExtension".as_ref(),
                module_output_path: None,
                swift_sources: &["/tmp/Sources/Widget.swift".into()],
                package_outputs: &[],
                external_link_inputs: &ExternalLinkInputs::default(),
                object_files: &[],
                index_store_path: None,
            },
        )
        .unwrap();

        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(!args.windows(2).any(|pair| pair == ["-Xlinker", "-e"]));
        assert!(!args.iter().any(|arg| arg == "_NSExtensionMain"));
    }

    #[test]
    fn target_invocation_omits_nsextension_main_for_extensionkit_runtime() {
        let invocation = target_swiftc_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Debug, DistributionKind::Development),
            SwiftTargetCompilePlan {
                target_kind: TargetKind::AppExtension,
                use_nsextension_main: false,
                module_name: "AppIntentsExtension",
                product_path: "/tmp/AppIntentsExtension.appex/AppIntentsExtension".as_ref(),
                module_output_path: None,
                swift_sources: &["/tmp/Sources/AppIntents.swift".into()],
                package_outputs: &[],
                external_link_inputs: &ExternalLinkInputs::default(),
                object_files: &[],
                index_store_path: None,
            },
        )
        .unwrap();

        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(!args.windows(2).any(|pair| pair == ["-Xlinker", "-e"]));
        assert!(!args.iter().any(|arg| arg == "_NSExtensionMain"));
    }

    #[test]
    fn package_invocation_captures_search_paths_and_module_output() {
        let invocation = package_target_swiftc_invocation(
            &fixture_toolchain(),
            &ProfileManifest::new(BuildConfiguration::Release, DistributionKind::Development),
            SwiftPackageTargetCompilePlan {
                module_name: "OrbiFeature",
                product_path: "/tmp/libOrbiPackage_OrbiFeature.a".as_ref(),
                module_output_path: "/tmp/modules/OrbiFeature.swiftmodule".as_ref(),
                swift_sources: &["/tmp/Package/Sources/OrbiFeature.swift".into()],
                module_search_paths: &["/tmp/modules".into()],
                library_search_paths: &["/tmp/libs".into()],
                link_libraries: &["OrbiPackage_OrbiCore".to_owned()],
                index_store_path: Some(Path::new("/tmp/index-store")),
            },
        )
        .unwrap();

        let args = invocation
            .args
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.contains(&"-emit-library".to_owned()));
        assert!(args.contains(&"-static".to_owned()));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-sdk", "/Applications/Xcode.app/SDK"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-emit-module-path", "/tmp/modules/OrbiFeature.swiftmodule"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-index-store-path", "/tmp/index-store"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-I", "/tmp/modules"]));
        assert!(args.windows(2).any(|pair| pair == ["-L", "/tmp/libs"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-l", "OrbiPackage_OrbiCore"])
        );
    }
}
