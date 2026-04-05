use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

use super::artifacts::remove_existing_path;
use super::compile::prefixed_compile_message;
use super::info_plist::needs_info_plist;
use super::resources::should_process_resources;
use super::{
    BuiltTarget, ProductLayout, bundle_frameworks_root, bundle_metadata_root, bundle_resources_root,
};
use crate::apple::build::external::{ExternalLinkInputs, PackageBuildOutput};
use crate::apple::build::toolchain::Toolchain;
use crate::apple::signing::{PackageSigningMaterial, SigningMaterial};
use crate::context::ProjectContext;
use crate::manifest::{
    ApplePlatform, DistributionKind, ExtensionManifest, IosTargetManifest, ProfileManifest,
    PushManifest, SwiftPackageDependency, TargetKind, TargetManifest, XcframeworkDependency,
};
use crate::util::{print_success, read_json_file_if_exists, resolve_path, write_json_file};

const ARTIFACT_CACHE_VERSION: u32 = 1;
const BUNDLE_CONTENT_CACHE_VERSION: u32 = 1;
const CODE_BUILD_CACHE_VERSION: u32 = 1;
const EMBEDDED_DEPENDENCY_CACHE_VERSION: u32 = 1;
const MERGED_TARGET_CACHE_VERSION: u32 = 1;
const SIGNING_CACHE_VERSION: u32 = 1;
const TARGET_BUILD_CACHE_VERSION: u32 = 2;

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CodeBuildCacheInfo {
    version: u32,
    fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct TargetBuildCacheInfo {
    version: u32,
    fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct SigningCacheInfo {
    version: u32,
    fingerprint: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ArtifactCacheInfo {
    version: u32,
    fingerprint: String,
    artifact_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct EmbeddedDependencyCacheInfo {
    version: u32,
    fingerprint: String,
    outputs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct MergedTargetCacheInfo {
    version: u32,
    fingerprint: String,
}

#[derive(Serialize)]
struct CodePhaseTargetFingerprint<'a> {
    name: &'a str,
    kind: TargetKind,
    platforms: &'a [ApplePlatform],
    sources: &'a [PathBuf],
    frameworks: &'a [String],
    weak_frameworks: &'a [String],
    system_libraries: &'a [String],
    xcframeworks: &'a [XcframeworkDependency],
    swift_packages: &'a [SwiftPackageDependency],
}

#[derive(Serialize)]
struct BundlePhaseTargetFingerprint<'a> {
    name: &'a str,
    kind: TargetKind,
    bundle_id: &'a str,
    display_name: &'a Option<String>,
    build_number: &'a Option<String>,
    resources: &'a [PathBuf],
    info_plist: &'a std::collections::BTreeMap<String, serde_json::Value>,
    ios: &'a Option<IosTargetManifest>,
    push: &'a Option<PushManifest>,
    extension: &'a Option<ExtensionManifest>,
}

pub(super) fn compute_code_phase_fingerprint(
    project: &ProjectContext,
    toolchain: &Toolchain,
    profile: &ProfileManifest,
    target: &TargetManifest,
    index_store_path: Option<&Path>,
    package_outputs: &[PackageBuildOutput],
    external_link_inputs: &ExternalLinkInputs,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(CODE_BUILD_CACHE_VERSION.to_le_bytes());
    hasher.update(serde_json::to_vec(&CodePhaseTargetFingerprint {
        name: &target.name,
        kind: target.kind,
        platforms: &target.platforms,
        sources: &target.sources,
        frameworks: &target.frameworks,
        weak_frameworks: &target.weak_frameworks,
        system_libraries: &target.system_libraries,
        xcframeworks: &target.xcframeworks,
        swift_packages: &target.swift_packages,
    })?);
    hasher.update(profile.variant_name().as_bytes());
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
    if let Some(index_store_path) = index_store_path {
        hasher.update(index_store_path.to_string_lossy().as_bytes());
    } else {
        hasher.update(b"no-index-store");
    }

    hash_source_roots(&mut hasher, project, target)?;
    hash_package_outputs(&mut hasher, package_outputs)?;
    hash_external_link_inputs(&mut hasher, external_link_inputs)?;

    Ok(hex_digest(hasher.finalize()))
}

pub(super) fn compute_bundle_phase_fingerprint(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    external_link_inputs: &ExternalLinkInputs,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(TARGET_BUILD_CACHE_VERSION.to_le_bytes());
    hasher.update(serde_json::to_vec(&BundlePhaseTargetFingerprint {
        name: &target.name,
        kind: target.kind,
        bundle_id: &target.bundle_id,
        display_name: &target.display_name,
        build_number: &target.build_number,
        resources: &target.resources,
        info_plist: &target.info_plist,
        ios: &target.ios,
        push: &target.push,
        extension: &target.extension,
    })?);
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

    hash_resource_roots(&mut hasher, project, target)?;
    hash_embedded_payloads(&mut hasher, external_link_inputs)?;

    Ok(hex_digest(hasher.finalize()))
}

pub(super) fn cached_code_phase_can_be_reused(
    target_dir: &Path,
    product: &ProductLayout,
    fingerprint: &str,
) -> Result<bool> {
    let Some(cache_info) =
        read_json_file_if_exists::<CodeBuildCacheInfo>(&code_cache_info_path(target_dir))?
    else {
        return Ok(false);
    };
    if cache_info.version != CODE_BUILD_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(false);
    }
    Ok(code_phase_outputs_exist(product))
}

pub(super) fn cached_bundle_phase_can_be_reused(
    target_dir: &Path,
    target: &TargetManifest,
    toolchain: &Toolchain,
    product: &ProductLayout,
    external_link_inputs: &ExternalLinkInputs,
    fingerprint: &str,
) -> Result<bool> {
    let Some(cache_info) =
        read_json_file_if_exists::<TargetBuildCacheInfo>(&cache_info_path(target_dir))?
    else {
        return Ok(false);
    };
    if cache_info.version != TARGET_BUILD_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(false);
    }
    Ok(target_build_outputs_exist(
        target,
        toolchain,
        product,
        external_link_inputs,
    ))
}

pub(super) fn write_code_phase_cache(target_dir: &Path, fingerprint: &str) -> Result<()> {
    write_json_file(
        &code_cache_info_path(target_dir),
        &CodeBuildCacheInfo {
            version: CODE_BUILD_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
        },
    )
}

pub(super) fn write_bundle_phase_cache(target_dir: &Path, fingerprint: &str) -> Result<()> {
    write_json_file(
        &bundle_cache_info_path(target_dir),
        &TargetBuildCacheInfo {
            version: TARGET_BUILD_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
        },
    )
}

pub(super) fn print_target_build_cache_hit(target_name: &str, log_prefix: Option<&str>) {
    print_success(prefixed_compile_message(
        log_prefix,
        format!("Reused cached build outputs for target `{target_name}`."),
    ));
}

pub(super) fn compute_embedded_dependency_fingerprint(
    platform: ApplePlatform,
    root_target: &TargetManifest,
    built_target: &BuiltTarget,
    outputs: &[PathBuf],
    dependency_fingerprints: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(EMBEDDED_DEPENDENCY_CACHE_VERSION.to_le_bytes());
    hasher.update(platform.to_string().as_bytes());
    hasher.update(root_target.name.as_bytes());
    hasher.update(format!("{:?}", root_target.kind).as_bytes());
    hasher.update(built_target.bundle_phase_fingerprint.as_bytes());
    for output in outputs {
        hasher.update(output.to_string_lossy().as_bytes());
    }
    for dependency in dependency_fingerprints {
        hasher.update(dependency.as_bytes());
    }
    hex_digest(hasher.finalize())
}

pub(super) fn cached_embedded_dependencies_can_be_reused(
    target_dir: &Path,
    bundle_root: &Path,
    outputs: &[PathBuf],
    fingerprint: &str,
) -> Result<bool> {
    let Some(cache_info) = read_json_file_if_exists::<EmbeddedDependencyCacheInfo>(
        &embedded_dependency_cache_info_path(target_dir),
    )?
    else {
        return Ok(false);
    };
    if cache_info.version != EMBEDDED_DEPENDENCY_CACHE_VERSION
        || cache_info.fingerprint != fingerprint
        || cache_info.outputs != outputs
    {
        return Ok(false);
    }

    Ok(outputs
        .iter()
        .all(|output| bundle_root.join(output).exists()))
}

pub(super) fn clear_embedded_dependency_outputs(
    target_dir: &Path,
    bundle_root: &Path,
    current_outputs: &[PathBuf],
) -> Result<()> {
    let mut outputs_to_remove = current_outputs.to_vec();
    if let Some(cache_info) = read_json_file_if_exists::<EmbeddedDependencyCacheInfo>(
        &embedded_dependency_cache_info_path(target_dir),
    )? {
        outputs_to_remove.extend(cache_info.outputs);
    }
    outputs_to_remove.sort();
    outputs_to_remove.dedup();
    for output in outputs_to_remove {
        remove_existing_path(&bundle_root.join(output))?;
    }
    Ok(())
}

pub(super) fn write_embedded_dependency_cache(
    target_dir: &Path,
    fingerprint: &str,
    outputs: &[PathBuf],
) -> Result<()> {
    write_json_file(
        &embedded_dependency_cache_info_path(target_dir),
        &EmbeddedDependencyCacheInfo {
            version: EMBEDDED_DEPENDENCY_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
            outputs: outputs.to_vec(),
        },
    )
}

pub(super) fn compute_universal_merge_fingerprint(
    target: &TargetManifest,
    primary_target: &BuiltTarget,
    secondary_target: &BuiltTarget,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(MERGED_TARGET_CACHE_VERSION.to_le_bytes());
    hasher.update(target.name.as_bytes());
    hasher.update(format!("{:?}", target.kind).as_bytes());
    hasher.update(primary_target.code_phase_fingerprint.as_bytes());
    hasher.update(primary_target.bundle_phase_fingerprint.as_bytes());
    hasher.update(secondary_target.code_phase_fingerprint.as_bytes());
    hasher.update(secondary_target.bundle_phase_fingerprint.as_bytes());
    hex_digest(hasher.finalize())
}

pub(super) fn cached_universal_merge_can_be_reused(
    target_dir: &Path,
    product: &ProductLayout,
    expected_outputs: &[PathBuf],
    fingerprint: &str,
) -> Result<bool> {
    let Some(cache_info) = read_json_file_if_exists::<MergedTargetCacheInfo>(
        &merged_target_cache_info_path(target_dir),
    )?
    else {
        return Ok(false);
    };
    if cache_info.version != MERGED_TARGET_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(false);
    }
    Ok(code_phase_outputs_exist(product) && expected_outputs.iter().all(|output| output.exists()))
}

pub(super) fn write_universal_merge_cache(target_dir: &Path, fingerprint: &str) -> Result<()> {
    write_json_file(
        &merged_target_cache_info_path(target_dir),
        &MergedTargetCacheInfo {
            version: MERGED_TARGET_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
        },
    )
}

pub(super) fn compute_bundle_content_fingerprint(
    target: &TargetManifest,
    built_target: &BuiltTarget,
    dependency_fingerprints: &[String],
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(BUNDLE_CONTENT_CACHE_VERSION.to_le_bytes());
    hasher.update(target.name.as_bytes());
    hasher.update(format!("{:?}", target.kind).as_bytes());
    hasher.update(built_target.code_phase_fingerprint.as_bytes());
    hasher.update(built_target.bundle_phase_fingerprint.as_bytes());
    for dependency in dependency_fingerprints {
        hasher.update(dependency.as_bytes());
    }
    Ok(hex_digest(hasher.finalize()))
}

pub(super) fn compute_signing_fingerprint(
    platform: ApplePlatform,
    distribution: DistributionKind,
    target: &TargetManifest,
    built_target: &BuiltTarget,
    bundle_content_fingerprint: &str,
    material: &SigningMaterial,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(SIGNING_CACHE_VERSION.to_le_bytes());
    hasher.update(platform.to_string().as_bytes());
    hasher.update(distribution.as_str().as_bytes());
    hasher.update(target.name.as_bytes());
    hasher.update(format!("{:?}", target.kind).as_bytes());
    hasher.update(bundle_content_fingerprint.as_bytes());
    hasher.update(built_target.binary_path.to_string_lossy().as_bytes());
    hasher.update(material.signing_identity.as_bytes());
    hasher.update(material.keychain_path.to_string_lossy().as_bytes());
    hash_file_contents(&mut hasher, &material.provisioning_profile_path)?;
    if let Some(entitlements_path) = &material.entitlements_path {
        hash_file_contents(&mut hasher, entitlements_path)?;
    } else {
        hasher.update(b"no-entitlements");
    }
    Ok(hex_digest(hasher.finalize()))
}

pub(super) fn cached_signed_bundle_can_be_reused(
    built_target: &BuiltTarget,
    platform: ApplePlatform,
    fingerprint: &str,
) -> Result<bool> {
    let Some(cache_info) = read_json_file_if_exists::<SigningCacheInfo>(&signing_cache_info_path(
        &built_target.target_dir,
    ))?
    else {
        return Ok(false);
    };
    if cache_info.version != SIGNING_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(false);
    }

    if !built_target.bundle_path.exists() || !built_target.binary_path.exists() {
        return Ok(false);
    }

    Ok(
        embedded_profile_path(&built_target.bundle_path, platform).exists()
            && code_signature_path(
                &built_target.bundle_path,
                built_target.target_kind,
                platform,
            )
            .exists(),
    )
}

pub(super) fn write_signing_cache(target_dir: &Path, fingerprint: &str) -> Result<()> {
    write_json_file(
        &signing_cache_info_path(target_dir),
        &SigningCacheInfo {
            version: SIGNING_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
        },
    )
}

pub(super) fn compute_artifact_fingerprint(
    distribution: DistributionKind,
    signed_bundle_fingerprint: &str,
    package_signing: Option<&PackageSigningMaterial>,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ARTIFACT_CACHE_VERSION.to_le_bytes());
    hasher.update(distribution.as_str().as_bytes());
    hasher.update(signed_bundle_fingerprint.as_bytes());
    if let Some(package_signing) = package_signing {
        hasher.update(package_signing.signing_identity.as_bytes());
        hasher.update(package_signing.keychain_path.to_string_lossy().as_bytes());
    } else {
        hasher.update(b"no-package-signing");
    }
    hex_digest(hasher.finalize())
}

pub(super) fn cached_exported_artifact_path(
    target_dir: &Path,
    desired_artifact_path: &Path,
    fingerprint: &str,
) -> Result<Option<PathBuf>> {
    let Some(cache_info) =
        read_json_file_if_exists::<ArtifactCacheInfo>(&artifact_cache_info_path(target_dir))?
    else {
        return Ok(None);
    };
    if cache_info.version != ARTIFACT_CACHE_VERSION || cache_info.fingerprint != fingerprint {
        return Ok(None);
    }
    if desired_artifact_path.exists() {
        return Ok(Some(desired_artifact_path.to_path_buf()));
    }
    if cache_info.artifact_path.exists() {
        return Ok(Some(cache_info.artifact_path));
    }
    Ok(None)
}

pub(super) fn write_artifact_cache(
    target_dir: &Path,
    fingerprint: &str,
    artifact_path: &Path,
) -> Result<()> {
    write_json_file(
        &artifact_cache_info_path(target_dir),
        &ArtifactCacheInfo {
            version: ARTIFACT_CACHE_VERSION,
            fingerprint: fingerprint.to_owned(),
            artifact_path: artifact_path.to_path_buf(),
        },
    )
}

pub(super) fn combine_fingerprints(values: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(values.len().to_le_bytes());
    for value in values {
        hasher.update(value.as_bytes());
    }
    hex_digest(hasher.finalize())
}

fn cache_info_path(target_dir: &Path) -> PathBuf {
    bundle_cache_info_path(target_dir)
}

fn artifact_cache_info_path(target_dir: &Path) -> PathBuf {
    target_dir.join("artifact-cache.json")
}

fn embedded_dependency_cache_info_path(target_dir: &Path) -> PathBuf {
    target_dir.join("embedded-dependency-cache.json")
}

fn code_cache_info_path(target_dir: &Path) -> PathBuf {
    target_dir.join("code-build-cache.json")
}

fn bundle_cache_info_path(target_dir: &Path) -> PathBuf {
    target_dir.join("build-cache.json")
}

fn merged_target_cache_info_path(target_dir: &Path) -> PathBuf {
    target_dir.join("universal-merge-cache.json")
}

fn signing_cache_info_path(target_dir: &Path) -> PathBuf {
    target_dir.join("signing-cache.json")
}

fn code_phase_outputs_exist(product: &ProductLayout) -> bool {
    if !product.product_path.exists() || !product.binary_path.exists() {
        return false;
    }
    if let Some(module_output_path) = &product.module_output_path
        && !module_output_path.exists()
    {
        return false;
    }
    true
}

fn target_build_outputs_exist(
    target: &TargetManifest,
    toolchain: &Toolchain,
    product: &ProductLayout,
    external_link_inputs: &ExternalLinkInputs,
) -> bool {
    if !product.product_path.exists() || !product.binary_path.exists() {
        return false;
    }
    if let Some(module_output_path) = &product.module_output_path
        && !module_output_path.exists()
    {
        return false;
    }
    if target.kind.is_bundle() && needs_info_plist(target.kind) {
        let info_plist_path =
            bundle_metadata_root(toolchain, target.kind, &product.product_path).join("Info.plist");
        if !info_plist_path.exists() {
            return false;
        }
    }
    if target.kind.is_bundle() && should_process_resources(target) {
        let resources_root = bundle_resources_root(toolchain, target.kind, &product.product_path);
        if !resources_root.exists() {
            return false;
        }
    }
    if target.kind.is_bundle() && !external_link_inputs.embedded_payloads.is_empty() {
        let frameworks_root = bundle_frameworks_root(toolchain, target.kind, &product.product_path);
        if external_link_inputs
            .embedded_payloads
            .iter()
            .any(|payload| {
                payload
                    .file_name()
                    .is_none_or(|file_name| !frameworks_root.join(file_name).exists())
            })
        {
            return false;
        }
    }

    true
}

fn hash_source_roots(
    hasher: &mut Sha256,
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<()> {
    let mut roots = BTreeSet::new();
    for root in project.resolved_manifest.shared_source_roots() {
        roots.insert(resolve_path(&project.root, &root));
    }
    for root in &target.sources {
        roots.insert(resolve_path(&project.root, root));
    }
    for root in roots {
        hash_path_tree(hasher, &root)?;
    }
    Ok(())
}

fn hash_resource_roots(
    hasher: &mut Sha256,
    project: &ProjectContext,
    target: &TargetManifest,
) -> Result<()> {
    for root in &target.resources {
        hash_path_tree(hasher, &resolve_path(&project.root, root))?;
    }
    Ok(())
}

fn hash_package_outputs(hasher: &mut Sha256, package_outputs: &[PackageBuildOutput]) -> Result<()> {
    for package in package_outputs {
        for library in &package.link_libraries {
            hasher.update(library.as_bytes());
        }
        hash_path_tree(hasher, &package.module_dir)?;
        hash_path_tree(hasher, &package.library_dir)?;
    }
    Ok(())
}

fn hash_external_link_inputs(
    hasher: &mut Sha256,
    external_link_inputs: &ExternalLinkInputs,
) -> Result<()> {
    for framework in &external_link_inputs.link_frameworks {
        hasher.update(framework.as_bytes());
    }
    for framework in &external_link_inputs.weak_frameworks {
        hasher.update(framework.as_bytes());
    }
    for library in &external_link_inputs.link_libraries {
        hasher.update(library.as_bytes());
    }

    let mut paths = BTreeSet::new();
    paths.extend(external_link_inputs.module_search_paths.iter().cloned());
    paths.extend(external_link_inputs.framework_search_paths.iter().cloned());
    paths.extend(external_link_inputs.library_search_paths.iter().cloned());
    paths.extend(external_link_inputs.embedded_payloads.iter().cloned());
    for path in paths {
        hash_path_tree(hasher, &path)?;
    }
    Ok(())
}

fn hash_embedded_payloads(
    hasher: &mut Sha256,
    external_link_inputs: &ExternalLinkInputs,
) -> Result<()> {
    for path in &external_link_inputs.embedded_payloads {
        hash_path_tree(hasher, path)?;
    }
    Ok(())
}

fn hash_file_contents(hasher: &mut Sha256, path: &Path) -> Result<()> {
    hasher.update(path.to_string_lossy().as_bytes());
    if !path.exists() {
        hasher.update(b"missing");
        return Ok(());
    }
    hasher.update(b"file");
    hasher.update(
        fs::read(path)
            .with_context(|| format!("failed to read {}", path.display()))?
            .as_slice(),
    );
    Ok(())
}

fn hash_path_tree(hasher: &mut Sha256, path: &Path) -> Result<()> {
    hasher.update(path.to_string_lossy().as_bytes());
    if !path.exists() {
        hasher.update(b"missing");
        return Ok(());
    }

    let mut paths = Vec::new();
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry.with_context(|| format!("failed to walk {}", path.display()))?;
        paths.push(entry.into_path());
    }
    paths.sort();

    for entry_path in paths {
        hash_path_entry(hasher, &entry_path)?;
    }
    Ok(())
}

fn hash_path_entry(hasher: &mut Sha256, path: &Path) -> Result<()> {
    hasher.update(path.to_string_lossy().as_bytes());
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("failed to stat {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        hasher.update(b"symlink");
        hasher.update(
            fs::read_link(path)
                .with_context(|| format!("failed to read symlink {}", path.display()))?
                .to_string_lossy()
                .as_bytes(),
        );
        return Ok(());
    }
    if metadata.is_dir() {
        hasher.update(b"dir");
        return Ok(());
    }

    hasher.update(b"file");
    hasher.update(metadata.len().to_le_bytes());
    let modified = metadata
        .modified()
        .with_context(|| format!("failed to read mtime for {}", path.display()))?
        .duration_since(std::time::UNIX_EPOCH)
        .with_context(|| format!("mtime for {} was before UNIX_EPOCH", path.display()))?;
    hasher.update(modified.as_nanos().to_le_bytes());
    Ok(())
}

fn embedded_profile_path(bundle_path: &Path, platform: ApplePlatform) -> PathBuf {
    if platform == ApplePlatform::Macos {
        bundle_path
            .join("Contents")
            .join("embedded.provisionprofile")
    } else {
        bundle_path.join("embedded.mobileprovision")
    }
}

fn code_signature_path(
    bundle_path: &Path,
    target_kind: TargetKind,
    platform: ApplePlatform,
) -> PathBuf {
    let metadata_root = if platform == ApplePlatform::Macos
        && matches!(
            target_kind,
            TargetKind::App
                | TargetKind::AppExtension
                | TargetKind::WatchApp
                | TargetKind::WatchExtension
                | TargetKind::WidgetExtension
        ) {
        bundle_path.join("Contents")
    } else {
        bundle_path.to_path_buf()
    };
    metadata_root.join("_CodeSignature").join("CodeResources")
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

    use tempfile::tempdir;

    use super::{
        BuiltTarget, ProductLayout, cached_signed_bundle_can_be_reused,
        cached_universal_merge_can_be_reused, write_signing_cache, write_universal_merge_cache,
    };
    use crate::manifest::{ApplePlatform, TargetKind};

    #[test]
    fn signing_cache_requires_code_signature_artifact() {
        let temp = tempdir().unwrap();
        let target_dir = temp.path().join("target");
        let bundle_path = target_dir.join("Example.app");
        let binary_path = bundle_path.join("Example");
        fs::create_dir_all(bundle_path.join("_CodeSignature")).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&binary_path, "binary").unwrap();
        fs::write(bundle_path.join("embedded.mobileprovision"), "profile").unwrap();
        fs::write(
            bundle_path.join("_CodeSignature").join("CodeResources"),
            "signed",
        )
        .unwrap();

        let built_target = BuiltTarget {
            target_name: "Example".to_owned(),
            target_kind: TargetKind::App,
            target_dir: target_dir.clone(),
            bundle_path: bundle_path.clone(),
            binary_path,
            module_output_path: None,
            code_phase_fingerprint: "code".to_owned(),
            bundle_phase_fingerprint: "bundle".to_owned(),
        };
        write_signing_cache(&target_dir, "fingerprint").unwrap();
        assert!(
            cached_signed_bundle_can_be_reused(&built_target, ApplePlatform::Ios, "fingerprint",)
                .unwrap()
        );

        fs::remove_file(bundle_path.join("_CodeSignature").join("CodeResources")).unwrap();
        assert!(
            !cached_signed_bundle_can_be_reused(&built_target, ApplePlatform::Ios, "fingerprint",)
                .unwrap()
        );
    }

    #[test]
    fn universal_merge_cache_requires_all_merged_module_outputs() {
        let temp = tempdir().unwrap();
        let target_dir = temp.path().join("target");
        let product_path = target_dir.join("Example.framework");
        let binary_path = product_path.join("Example");
        let primary_module = product_path
            .join("Modules")
            .join("Example.swiftmodule")
            .join("arm64-apple-macosx14.0.swiftmodule");
        let secondary_module = product_path
            .join("Modules")
            .join("Example.swiftmodule")
            .join("x86_64-apple-macosx14.0.swiftmodule");
        fs::create_dir_all(primary_module.parent().unwrap()).unwrap();
        fs::write(&binary_path, "binary").unwrap();
        fs::write(&primary_module, "primary").unwrap();
        fs::write(&secondary_module, "secondary").unwrap();

        let layout = ProductLayout {
            product_path,
            binary_path,
            module_output_path: Some(primary_module.clone()),
        };
        write_universal_merge_cache(&target_dir, "fingerprint").unwrap();
        let expected_outputs = vec![primary_module.clone(), secondary_module.clone()];
        assert!(
            cached_universal_merge_can_be_reused(
                &target_dir,
                &layout,
                &expected_outputs,
                "fingerprint",
            )
            .unwrap()
        );

        fs::remove_file(&secondary_module).unwrap();
        assert!(
            !cached_universal_merge_can_be_reused(
                &target_dir,
                &layout,
                &expected_outputs,
                "fingerprint",
            )
            .unwrap()
        );
    }
}
