mod config;
mod tooling;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::apple::analysis::{
    SemanticCompilationArtifact, SemanticCompilerInvocation, build_semantic_compilation_artifact,
    collect_project_swift_files, load_cached_analysis_project,
};
use crate::apple::runtime::apple_platform_from_cli;
use crate::cli::{FormatArgs, LintArgs};
use crate::context::{AppContext, ProjectContext};
use crate::util::{command_output_allow_failure, print_success};
use config::{format_configuration_json, format_ignore_matcher, lint_quality_config};
use tooling::{
    OrbitSwiftFormatMode, OrbitSwiftFormatRequest, OrbitSwiftLintCompilerInvocation,
    OrbitSwiftLintRequest, run_orbit_swift_format, run_orbit_swiftlint,
};

pub fn lint_project(
    app: &AppContext,
    args: &LintArgs,
    requested_manifest: Option<&Path>,
) -> Result<()> {
    let analysis_project = load_cached_analysis_project(app, requested_manifest)?;
    let project = &analysis_project.project;
    let lint_quality = lint_quality_config(project)?;
    let include_source = |path: &Path| {
        !lint_quality
            .ignore_matcher()
            .is_some_and(|matcher| matcher.is_ignored(path))
    };
    let swift_files = collect_project_swift_files(project, &include_source)?;
    let artifact = build_semantic_compilation_artifact(
        project,
        args.platform.map(apple_platform_from_cli),
        &include_source,
    )?;
    let swift_compiler_invocations = artifact
        .invocations
        .iter()
        .filter(|invocation| invocation.language == "swift")
        .map(|invocation| OrbitSwiftLintCompilerInvocation {
            arguments: invocation.arguments.clone(),
            source_files: invocation.source_files.clone(),
        })
        .collect::<Vec<_>>();
    let c_family_invocations = artifact
        .invocations
        .iter()
        .filter(|invocation| is_c_family_semantic_invocation(invocation))
        .collect::<Vec<_>>();
    if swift_files.is_empty() && c_family_invocations.is_empty() {
        print_success("No Swift or C-family sources found.");
        return Ok(());
    }
    if !swift_files.is_empty() {
        run_orbit_swiftlint(
            app,
            project.project_paths.orbit_dir.as_path(),
            &OrbitSwiftLintRequest {
                working_directory: project.root.clone(),
                configuration_json: lint_quality.configuration_json,
                files: swift_files.clone(),
                compiler_invocations: swift_compiler_invocations.clone(),
            },
        )?;
    }
    if !c_family_invocations.is_empty() {
        run_c_family_semantic_lint(&artifact)?;
    }
    print_success(format!(
        "Lint completed for {} Swift file(s) and {} C-family file(s) across {} platform(s), using {} Swift semantic command(s) and {} C-family compiler check(s).",
        swift_files.len(),
        c_family_source_count(&artifact),
        artifact.platforms.len(),
        swift_compiler_invocations.len(),
        c_family_invocations.len()
    ));

    Ok(())
}

pub fn format_project(
    app: &AppContext,
    args: &FormatArgs,
    requested_manifest: Option<&Path>,
) -> Result<()> {
    let analysis_project = load_cached_analysis_project(app, requested_manifest)?;
    let project = &analysis_project.project;
    let ignore_matcher = format_ignore_matcher(project)?;
    let include_source = |path: &Path| {
        !ignore_matcher
            .as_ref()
            .is_some_and(|matcher| matcher.is_ignored(path))
    };
    let swift_files = collect_project_swift_files(project, &include_source)?;
    run_swift_format(
        app,
        project,
        if args.write {
            SwiftFormatMode::FormatWrite
        } else {
            SwiftFormatMode::FormatCheck
        },
        &swift_files,
        format_configuration_json(project, &swift_files)?,
    )
}

#[derive(Debug, Clone, Copy)]
enum SwiftFormatMode {
    FormatCheck,
    FormatWrite,
}

fn run_swift_format(
    app: &AppContext,
    project: &ProjectContext,
    mode: SwiftFormatMode,
    swift_files: &[PathBuf],
    configuration_json: Option<String>,
) -> Result<()> {
    if swift_files.is_empty() {
        print_success("No Swift sources found.");
        return Ok(());
    }

    run_orbit_swift_format(
        app,
        project.project_paths.orbit_dir.as_path(),
        &OrbitSwiftFormatRequest {
            working_directory: project.root.clone(),
            configuration_json,
            mode: match mode {
                SwiftFormatMode::FormatCheck => OrbitSwiftFormatMode::Check,
                SwiftFormatMode::FormatWrite => OrbitSwiftFormatMode::Write,
            },
            files: swift_files.to_vec(),
        },
    )?;

    match mode {
        SwiftFormatMode::FormatCheck => print_success(format!(
            "Formatting is clean for {} Swift file(s).",
            swift_files.len()
        )),
        SwiftFormatMode::FormatWrite => {
            print_success(format!("Formatted {} Swift file(s).", swift_files.len()))
        }
    }
    Ok(())
}

fn run_c_family_semantic_lint(artifact: &SemanticCompilationArtifact) -> Result<()> {
    for invocation in artifact
        .invocations
        .iter()
        .filter(|invocation| is_c_family_semantic_invocation(invocation))
    {
        let (compiler, arguments) = invocation
            .arguments
            .split_first()
            .context("C-family semantic invocation was missing a compiler executable")?;
        let mut command = Command::new("xcrun");
        command.current_dir(&invocation.working_directory);
        command.args(["--sdk", invocation.sdk_name.as_str(), compiler.as_str()]);
        command.args(syntax_only_clang_arguments(arguments));
        let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
        if !success {
            let source = invocation
                .source_files
                .first()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| invocation.target.clone());
            bail!(
                "semantic {} diagnostics failed for `{}`\nstdout:\n{}\nstderr:\n{}",
                invocation.language,
                source,
                stdout,
                stderr
            );
        }
    }
    Ok(())
}

fn syntax_only_clang_arguments(arguments: &[String]) -> Vec<String> {
    let mut transformed = Vec::with_capacity(arguments.len() + 1);
    transformed.push("-fsyntax-only".to_owned());
    let mut skip_next = false;
    for argument in arguments {
        if skip_next {
            skip_next = false;
            continue;
        }
        match argument.as_str() {
            "-c" => continue,
            "-o" | "-index-store-path" => {
                skip_next = true;
            }
            _ => transformed.push(argument.clone()),
        }
    }
    transformed
}

fn is_c_family_semantic_invocation(invocation: &SemanticCompilerInvocation) -> bool {
    matches!(
        invocation.language.as_str(),
        "c" | "objective-c" | "cpp" | "objective-cpp"
    )
}

fn c_family_source_count(artifact: &SemanticCompilationArtifact) -> usize {
    artifact
        .invocations
        .iter()
        .filter(|invocation| is_c_family_semantic_invocation(invocation))
        .flat_map(|invocation| invocation.source_files.iter())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}
