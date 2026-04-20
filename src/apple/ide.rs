use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::apple::analysis::{
    SemanticCompilerInvocation, build_cached_semantic_compilation_artifact_with_status,
    load_cached_analysis_project,
};
use crate::apple::runtime::apple_platform_from_cli;
use crate::cli::IdeDumpArgs;
use crate::context::AppContext;
use crate::util::resolve_path;

#[derive(Debug, Serialize)]
struct DumpArgsOutput {
    manifest_path: PathBuf,
    project_root: PathBuf,
    requested_file: Option<PathBuf>,
    platforms: Vec<String>,
    invocations: Vec<SemanticCompilerInvocation>,
}

pub fn dump_args(
    app: &AppContext,
    args: &IdeDumpArgs,
    requested_manifest: Option<&Path>,
) -> Result<()> {
    let analysis_project = load_cached_analysis_project(app, requested_manifest)?;
    let explicit_platform = args.platform.map(apple_platform_from_cli);
    let requested_file = args
        .file
        .as_deref()
        .map(|path| resolve_filter_file(&analysis_project.project.root, path))
        .transpose()?;
    let cached_artifact = build_cached_semantic_compilation_artifact_with_status(
        &analysis_project.project,
        explicit_platform,
    )?;
    eprintln!(
        "orbi > {}",
        cached_artifact.cache_status.message(explicit_platform)
    );
    let mut artifact = cached_artifact.artifact;

    if let Some(filter_file) = requested_file.as_ref() {
        artifact.invocations.retain(|invocation| {
            invocation
                .source_files
                .iter()
                .any(|path| path == filter_file)
        });
        if artifact.invocations.is_empty() {
            bail!(
                "no semantic compilation command matched `{}`",
                filter_file.display()
            );
        }
        let mut platforms = artifact
            .invocations
            .iter()
            .map(|invocation| invocation.platform.clone())
            .collect::<Vec<_>>();
        platforms.sort();
        platforms.dedup();
        artifact.platforms = platforms;
    }

    let output = DumpArgsOutput {
        manifest_path: analysis_project.project.manifest_path.clone(),
        project_root: analysis_project.project.root.clone(),
        requested_file,
        platforms: artifact.platforms,
        invocations: artifact.invocations,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&output).context("failed to serialize compiler arguments")?
    );
    Ok(())
}

fn resolve_filter_file(project_root: &Path, path: &Path) -> Result<PathBuf> {
    let resolved = resolve_path(project_root, path);
    if resolved.exists() {
        resolved
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", resolved.display()))
    } else {
        Ok(resolved)
    }
}
