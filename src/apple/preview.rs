use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use walkdir::WalkDir;

use crate::apple::analysis::{
    SemanticCompilerInvocation, build_cached_semantic_compilation_artifact_with_status,
    load_cached_analysis_project,
};
use crate::apple::build;
use crate::apple::build::toolchain::DestinationKind;
use crate::apple::runtime::{self, apple_platform_from_cli};
use crate::apple::simulator::{SimulatorDevice, select_simulator_device};
use crate::apple::testing::ui::backend::{MacosBackend, UiBackend};
use crate::apple::xcode::{SelectedXcode, xcrun_command};
use crate::cli::{PreviewListArgs, PreviewShotArgs};
use crate::context::{AppContext, ProjectContext, ProjectPaths};
use crate::manifest::{
    ApplePlatform, ManifestSchema, ResolvedManifest, SwiftPackageDependency, SwiftPackageSource,
    TargetKind, TargetManifest, XcframeworkDependency,
};
use crate::util::{
    combine_command_output, command_output_allow_failure, copy_file, ensure_dir, ensure_parent_dir,
    prompt_select, resolve_path, run_command, run_command_capture, timestamp_slug, write_json_file,
};

const PREVIEW_HELPER_NAME: &str = "__orbi_make_preview_content";
const MACRO_SECTION_DIVIDER: &str = "------------------------------";

#[derive(Debug, Clone)]
struct DiscoveredPreview {
    target_name: String,
    source_file: PathBuf,
    line: usize,
    column: usize,
    name: Option<String>,
    helper_body: String,
    raw_traits: Option<String>,
}

#[derive(Debug, Serialize)]
struct PreviewShotReport {
    platform: String,
    target: String,
    preview_name: Option<String>,
    file: PathBuf,
    line: usize,
    column: usize,
    screenshot_path: PathBuf,
    run_root: PathBuf,
    receipt_path: PathBuf,
    applied_traits: Vec<String>,
    ignored_traits: Vec<String>,
}

#[derive(Debug)]
struct GeneratedPreviewProject {
    project: ProjectContext,
    run_root: PathBuf,
    screenshot_path: PathBuf,
    ignored_traits: Vec<String>,
    applied_traits: Vec<String>,
}

struct PreviewTargetPlan<'a> {
    runtime_target: &'a TargetManifest,
    source_target: &'a TargetManifest,
}

enum PreviewRuntimeBackend {
    Simulator(SimulatorPreviewBackend),
    Macos(Box<MacosBackend>),
}

struct SimulatorPreviewBackend {
    device: SimulatorDevice,
    selected_xcode: Option<SelectedXcode>,
}

pub fn list(
    app: &AppContext,
    args: &PreviewListArgs,
    requested_manifest: Option<&Path>,
) -> Result<()> {
    let project = app.load_project(requested_manifest)?;
    let platform = resolve_preview_platform(&project, args.platform)?;
    let analysis_project = load_cached_analysis_project(app, requested_manifest)?;
    let previews = discover_previews(&project, &analysis_project.project, platform)?;
    if previews.is_empty() {
        println!("No `#Preview` declarations found for platform `{platform}`.");
        return Ok(());
    }

    for preview in &previews {
        println!(
            "{} [{}] {}",
            preview_display_name(preview),
            preview.target_name,
            preview_location_display(&project.root, preview)
        );
    }
    Ok(())
}

pub fn shot(
    app: &AppContext,
    args: &PreviewShotArgs,
    requested_manifest: Option<&Path>,
) -> Result<()> {
    let project = app.load_project(requested_manifest)?;
    let platform = resolve_preview_platform(&project, args.platform)?;
    let analysis_project = load_cached_analysis_project(app, requested_manifest)?;
    let previews = discover_previews(&project, &analysis_project.project, platform)?;
    if previews.is_empty() {
        bail!("no `#Preview` declarations were found for platform `{platform}`");
    }

    let preview = select_preview(&project, previews.as_slice(), args.preview.as_deref())?;
    let generated = generate_preview_project(&project, platform, preview, args.output.as_deref())?;
    let (receipt_path, screenshot_path) = render_preview_shot(
        &generated.project,
        platform,
        &generated.screenshot_path,
        args.delay_ms,
    )?;

    let report = PreviewShotReport {
        platform: platform.to_string(),
        target: preview.target_name.clone(),
        preview_name: preview.name.clone(),
        file: preview.source_file.clone(),
        line: preview.line,
        column: preview.column,
        screenshot_path: screenshot_path.clone(),
        run_root: generated.run_root.clone(),
        receipt_path: receipt_path.clone(),
        applied_traits: generated.applied_traits,
        ignored_traits: generated.ignored_traits,
    };
    let report_path = generated.run_root.join("report.json");
    write_json_file(&report_path, &report)?;

    println!("preview: {}", preview_display_name(preview));
    println!("file: {}", preview_location_display(&project.root, preview));
    if !report.ignored_traits.is_empty() {
        eprintln!(
            "warning: `preview shot` currently ignores some preview traits: {}",
            report.ignored_traits.join(", ")
        );
    }
    println!("screenshot: {}", screenshot_path.display());
    println!("report: {}", report_path.display());
    Ok(())
}

fn resolve_preview_platform(
    project: &ProjectContext,
    requested: Option<crate::cli::TargetPlatform>,
) -> Result<ApplePlatform> {
    let platform = runtime::resolve_platform(
        project,
        requested.map(apple_platform_from_cli),
        "Select a platform to preview",
    )?;
    match platform {
        ApplePlatform::Ios
        | ApplePlatform::Macos
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => Ok(platform),
    }
}

fn discover_previews(
    project: &ProjectContext,
    analysis_project: &ProjectContext,
    platform: ApplePlatform,
) -> Result<Vec<DiscoveredPreview>> {
    let preview_target_names =
        discoverable_preview_target_names(&project.resolved_manifest, platform)?;
    let cached_artifact =
        build_cached_semantic_compilation_artifact_with_status(analysis_project, Some(platform))?;
    let mut previews = Vec::new();
    for invocation in &cached_artifact.artifact.invocations {
        if invocation.language != "swift" || !preview_target_names.contains(&invocation.target) {
            continue;
        }
        previews.extend(discover_invocation_previews(invocation)?);
    }
    previews.sort_by(|left, right| {
        left.source_file
            .cmp(&right.source_file)
            .then(left.line.cmp(&right.line))
            .then(left.column.cmp(&right.column))
    });
    Ok(previews)
}

fn discoverable_preview_target_names(
    manifest: &ResolvedManifest,
    platform: ApplePlatform,
) -> Result<Vec<String>> {
    let app_target = manifest.default_build_target_for_platform(platform)?;
    let mut names = vec![app_target.name.clone()];
    if platform == ApplePlatform::Watchos {
        names.extend(
            manifest
                .topological_targets(&app_target.name)?
                .into_iter()
                .filter(|target| {
                    target.kind == TargetKind::WatchExtension && target.supports_platform(platform)
                })
                .map(|target| target.name.clone()),
        );
    }
    names.sort();
    names.dedup();
    Ok(names)
}

fn discover_invocation_previews(
    invocation: &SemanticCompilerInvocation,
) -> Result<Vec<DiscoveredPreview>> {
    let mut command = preview_macro_expansion_command(invocation)?;
    let (stdout, stderr) = run_command_capture(&mut command)?;
    let output = combine_command_output(&stdout, &stderr);
    let mut previews = Vec::new();
    for section in parse_macro_expansion_sections(&output) {
        if !section.contains("DeveloperToolsSupport.PreviewRegistry") {
            continue;
        }
        previews.push(parse_preview_section(invocation, &section)?);
    }
    Ok(previews)
}

fn preview_macro_expansion_command(invocation: &SemanticCompilerInvocation) -> Result<Command> {
    let swiftc = invocation.toolchain_root.join("usr/bin").join("swiftc");
    let mut command = Command::new(&swiftc);
    command.current_dir(&invocation.working_directory);
    command.args(preview_macro_expansion_arguments(&invocation.arguments));
    Ok(command)
}

fn preview_macro_expansion_arguments(arguments: &[String]) -> Vec<String> {
    let mut expanded = Vec::new();
    let mut index = 0usize;
    while index < arguments.len() {
        let argument = &arguments[index];
        if index == 0 && argument == "swiftc" {
            index += 1;
            continue;
        }
        match argument.as_str() {
            "-o" | "-emit-module-path" => {
                index += 2;
                continue;
            }
            "-emit-library" | "-emit-module" | "-static" => {
                index += 1;
                continue;
            }
            _ => {
                expanded.push(argument.clone());
                index += 1;
            }
        }
    }
    expanded.push("-typecheck".to_owned());
    expanded.push("-dump-macro-expansions".to_owned());
    expanded
}

fn parse_macro_expansion_sections(output: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut lines = output.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.starts_with("@__swiftmacro_") || !line.ends_with(".swift") {
            continue;
        }
        let Some(divider) = lines.next() else {
            break;
        };
        if divider.trim() != MACRO_SECTION_DIVIDER {
            continue;
        }
        let mut code = Vec::new();
        for line in lines.by_ref() {
            if line.trim() == MACRO_SECTION_DIVIDER {
                break;
            }
            code.push(line);
        }
        sections.push(code.join("\n"));
    }
    sections
}

fn parse_preview_section(
    invocation: &SemanticCompilerInvocation,
    section: &str,
) -> Result<DiscoveredPreview> {
    let file_id = parse_string_property(section, "fileID")
        .context("failed to resolve preview `fileID` from macro expansion")?;
    let line = parse_usize_property(section, "line")
        .context("failed to resolve preview line from macro expansion")?;
    let column = parse_usize_property(section, "column")
        .context("failed to resolve preview column from macro expansion")?;
    let make_preview_body = extract_function_body(
        section,
        "static func makePreview() throws -> DeveloperToolsSupport.Preview",
    )?;
    let (args, closure_body) = extract_preview_constructor(&make_preview_body)?;
    let source_file =
        resolve_preview_source_file(invocation.source_files.as_slice(), &file_id, line)?;

    Ok(DiscoveredPreview {
        target_name: invocation.target.clone(),
        source_file,
        line,
        column,
        name: parse_preview_name(&args),
        helper_body: closure_body,
        raw_traits: parse_preview_traits(&args),
    })
}

fn extract_preview_constructor(make_preview_body: &str) -> Result<(String, String)> {
    let preview_call = make_preview_body
        .find("DeveloperToolsSupport.Preview")
        .context("macro expansion did not contain `DeveloperToolsSupport.Preview`")?;
    let constructor_start = preview_call + "DeveloperToolsSupport.Preview".len();
    let rest = &make_preview_body[constructor_start..];
    let significant_offset = rest
        .char_indices()
        .find_map(|(index, character)| (!character.is_whitespace()).then_some(index))
        .context("preview expansion did not contain constructor contents")?;
    let significant_index = constructor_start + significant_offset;
    let next_character = make_preview_body[significant_index..]
        .chars()
        .next()
        .context("preview expansion ended unexpectedly")?;

    let (args, closure_open) = match next_character {
        '(' => {
            let args_close =
                find_matching_delimiter(make_preview_body, significant_index, '(', ')')?;
            let args = make_preview_body[significant_index + 1..args_close]
                .trim()
                .to_owned();
            let closure_open = make_preview_body[args_close + 1..]
                .find('{')
                .map(|index| index + args_close + 1)
                .context("preview expansion did not contain a trailing preview closure")?;
            (args, closure_open)
        }
        '{' => (String::new(), significant_index),
        other => {
            bail!("preview expansion used an unsupported constructor form starting with `{other}`")
        }
    };

    let closure_close = find_matching_delimiter(make_preview_body, closure_open, '{', '}')?;
    let closure_body = trim_common_indent(&make_preview_body[closure_open + 1..closure_close]);
    Ok((args, closure_body.trim().to_owned()))
}

fn parse_string_property(section: &str, property: &str) -> Option<String> {
    let marker = format!("static var {property}: String {{");
    let block = section.find(&marker)?;
    let rest = &section[block + marker.len()..];
    for line in rest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "}" {
            continue;
        }
        return parse_simple_swift_string(trimmed);
    }
    None
}

fn parse_usize_property(section: &str, property: &str) -> Option<usize> {
    let marker = format!("static var {property}: Int {{");
    let block = section.find(&marker)?;
    let rest = &section[block + marker.len()..];
    for line in rest.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "}" {
            continue;
        }
        return trimmed.parse().ok();
    }
    None
}

fn extract_function_body(section: &str, signature: &str) -> Result<String> {
    let start = section
        .find(signature)
        .with_context(|| format!("macro expansion did not contain `{signature}`"))?;
    let body_open = section[start..]
        .find('{')
        .map(|index| index + start)
        .context("function signature did not contain an opening brace")?;
    let body_close = find_matching_delimiter(section, body_open, '{', '}')?;
    Ok(section[body_open + 1..body_close].trim().to_owned())
}

fn find_matching_delimiter(
    text: &str,
    open_index: usize,
    open: char,
    close: char,
) -> Result<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = open_index;
    let mut in_string = false;
    let mut in_multiline_string = false;
    let mut escaped = false;

    while index < bytes.len() {
        if !in_string
            && !in_multiline_string
            && bytes[index] == b'/'
            && index + 1 < bytes.len()
            && bytes[index + 1] == b'/'
        {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }

        if !in_string
            && !in_multiline_string
            && index + 2 < bytes.len()
            && &bytes[index..index + 3] == b"\"\"\""
        {
            in_multiline_string = true;
            index += 3;
            continue;
        }

        if in_multiline_string {
            if index + 2 < bytes.len() && &bytes[index..index + 3] == b"\"\"\"" {
                in_multiline_string = false;
                index += 3;
            } else {
                index += 1;
            }
            continue;
        }

        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }

        if byte == b'"' {
            in_string = true;
            index += 1;
            continue;
        }

        let character = byte as char;
        if character == open {
            depth += 1;
        } else if character == close {
            depth = depth.checked_sub(1).context(
                "encountered an unexpected closing delimiter while parsing preview code",
            )?;
            if depth == 0 {
                return Ok(index);
            }
        }
        index += 1;
    }

    bail!("failed to find a matching `{close}` while parsing preview code")
}

fn parse_simple_swift_string(text: &str) -> Option<String> {
    let stripped = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut value = String::new();
    let mut chars = stripped.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            value.push(character);
            continue;
        }
        match chars.next()? {
            'n' => value.push('\n'),
            'r' => value.push('\r'),
            't' => value.push('\t'),
            '\\' => value.push('\\'),
            '"' => value.push('"'),
            '\'' => value.push('\''),
            '0' => value.push('\0'),
            other => value.push(other),
        }
    }
    Some(value)
}

fn parse_preview_name(arguments: &str) -> Option<String> {
    let trimmed = arguments.trim_start();
    if trimmed.is_empty() || trimmed.starts_with("traits:") || trimmed.starts_with("nil") {
        return None;
    }
    parse_simple_swift_string(read_swift_argument(trimmed).trim())
}

fn parse_preview_traits(arguments: &str) -> Option<String> {
    let marker = "traits:";
    let index = arguments.find(marker)?;
    Some(arguments[index + marker.len()..].trim().to_owned())
}

fn read_swift_argument(arguments: &str) -> &str {
    let mut depth_paren = 0usize;
    let mut depth_brace = 0usize;
    let mut depth_bracket = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in arguments.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '(' => depth_paren += 1,
            ')' => depth_paren = depth_paren.saturating_sub(1),
            '{' => depth_brace += 1,
            '}' => depth_brace = depth_brace.saturating_sub(1),
            '[' => depth_bracket += 1,
            ']' => depth_bracket = depth_bracket.saturating_sub(1),
            ',' if depth_paren == 0 && depth_brace == 0 && depth_bracket == 0 => {
                return &arguments[..index];
            }
            _ => {}
        }
    }
    arguments
}

fn resolve_preview_source_file(
    source_files: &[PathBuf],
    file_id: &str,
    line: usize,
) -> Result<PathBuf> {
    let suffix = file_id
        .split_once('/')
        .map(|(_, suffix)| suffix)
        .unwrap_or(file_id);
    let suffix = suffix.replace('\\', "/");
    let mut candidates = source_files
        .iter()
        .filter(|path| {
            let value = path.to_string_lossy().replace('\\', "/");
            value.ends_with(file_id) || value.ends_with(&suffix)
        })
        .cloned()
        .collect::<Vec<_>>();

    if candidates.len() > 1 {
        candidates.retain(|path| preview_line_matches(path, line).unwrap_or(false));
    }
    match candidates.len() {
        1 => Ok(candidates.remove(0)),
        0 => bail!("could not resolve preview source file for `{file_id}`"),
        _ => bail!("resolved multiple source files for preview `{file_id}`"),
    }
}

fn preview_line_matches(path: &Path, line: usize) -> Result<bool> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read preview source {}", path.display()))?;
    let lines = contents.lines().collect::<Vec<_>>();
    let start = line.saturating_sub(2);
    let end = usize::min(line + 1, lines.len());
    Ok(lines[start..end]
        .iter()
        .any(|candidate| candidate.contains("#Preview")))
}

fn select_preview<'a>(
    project: &ProjectContext,
    previews: &'a [DiscoveredPreview],
    query: Option<&str>,
) -> Result<&'a DiscoveredPreview> {
    if let Some(query) = query {
        let matches = previews
            .iter()
            .filter(|preview| preview_matches_query(project, preview, query))
            .collect::<Vec<_>>();
        return match matches.len() {
            0 => bail!("no preview matched `{query}`"),
            1 => Ok(matches[0]),
            _ if !project.app.interactive => bail!(
                "multiple previews matched `{query}`; run `orbi preview list` to disambiguate"
            ),
            _ => {
                let options = matches
                    .iter()
                    .map(|preview| preview_selection_label(&project.root, preview))
                    .collect::<Vec<_>>();
                let index = prompt_select("Select a preview", &options)?;
                Ok(matches[index])
            }
        };
    }

    match previews.len() {
        0 => bail!("no previews were discovered"),
        1 => Ok(&previews[0]),
        _ if !project.app.interactive => {
            bail!("multiple previews were found; pass a preview name or run interactively")
        }
        _ => {
            let options = previews
                .iter()
                .map(|preview| preview_selection_label(&project.root, preview))
                .collect::<Vec<_>>();
            let index = prompt_select("Select a preview", &options)?;
            Ok(&previews[index])
        }
    }
}

fn preview_matches_query(
    project: &ProjectContext,
    preview: &DiscoveredPreview,
    query: &str,
) -> bool {
    let query = query.to_ascii_lowercase();
    let location = preview_location_display(&project.root, preview).to_ascii_lowercase();
    preview_display_name(preview)
        .to_ascii_lowercase()
        .contains(&query)
        || preview.target_name.to_ascii_lowercase().contains(&query)
        || location.contains(&query)
        || preview
            .name
            .as_ref()
            .is_some_and(|name| name.eq_ignore_ascii_case(query.as_str()))
}

fn preview_selection_label(root: &Path, preview: &DiscoveredPreview) -> String {
    format!(
        "{} [{}] {}",
        preview_display_name(preview),
        preview.target_name,
        preview_location_display(root, preview)
    )
}

fn preview_display_name(preview: &DiscoveredPreview) -> String {
    preview.name.clone().unwrap_or_else(|| {
        format!(
            "<unnamed:{}:{}>",
            preview
                .source_file
                .file_name()
                .unwrap_or_default()
                .to_string_lossy(),
            preview.line
        )
    })
}

fn preview_location_display(root: &Path, preview: &DiscoveredPreview) -> String {
    let display_path = preview
        .source_file
        .strip_prefix(root)
        .unwrap_or(&preview.source_file)
        .display()
        .to_string();
    format!("{display_path}:{}:{}", preview.line, preview.column)
}

fn preview_target_plan<'a>(
    manifest: &'a ResolvedManifest,
    platform: ApplePlatform,
    preview_target_name: &str,
) -> Result<PreviewTargetPlan<'a>> {
    let runtime_target = manifest.default_build_target_for_platform(platform)?;
    let source_target = manifest.resolve_target(Some(preview_target_name))?;

    if !source_target.supports_platform(platform) {
        bail!("target `{preview_target_name}` does not support platform `{platform}`");
    }

    if platform == ApplePlatform::Watchos {
        if source_target.name == runtime_target.name {
            return Ok(PreviewTargetPlan {
                runtime_target,
                source_target,
            });
        }
        if source_target.kind != TargetKind::WatchExtension {
            bail!(
                "`preview shot --platform watchos` supports previews only in `{}` or its WatchExtension dependency",
                runtime_target.name
            );
        }
        let is_runtime_dependency = manifest
            .topological_targets(&runtime_target.name)?
            .into_iter()
            .any(|target| target.name == source_target.name);
        if !is_runtime_dependency {
            bail!(
                "watchOS preview target `{}` is not embedded by runtime target `{}`",
                source_target.name,
                runtime_target.name
            );
        }
        return Ok(PreviewTargetPlan {
            runtime_target,
            source_target,
        });
    }

    if source_target.name != runtime_target.name {
        bail!(
            "`preview shot` currently supports previews only in the default app target `{}`",
            runtime_target.name
        );
    }
    Ok(PreviewTargetPlan {
        runtime_target,
        source_target,
    })
}

fn generate_preview_project(
    project: &ProjectContext,
    platform: ApplePlatform,
    preview: &DiscoveredPreview,
    output: Option<&Path>,
) -> Result<GeneratedPreviewProject> {
    let target_plan =
        preview_target_plan(&project.resolved_manifest, platform, &preview.target_name)?;
    let app_target = target_plan.runtime_target;
    let source_target = target_plan.source_target;

    let run_root = project
        .project_paths
        .orbi_dir
        .join("previews")
        .join(format!(
            "{}-{}",
            timestamp_slug(),
            sanitize_component(preview_display_name(preview).as_str())
        ));
    ensure_dir(&run_root)?;
    let orbi_dir = run_root.join(".orbi");
    let build_dir = orbi_dir.join("build");
    let artifacts_dir = orbi_dir.join("artifacts");
    let receipts_dir = orbi_dir.join("receipts");
    ensure_dir(&orbi_dir)?;
    ensure_dir(&build_dir)?;
    ensure_dir(&artifacts_dir)?;
    ensure_dir(&receipts_dir)?;

    let helper_code = render_preview_helper(preview);
    let dependency_targets = collect_library_dependency_targets(project, source_target)?;
    let mut synthetic_targets = Vec::new();
    for dependency in &dependency_targets {
        synthetic_targets.push(clone_preview_target(
            project, &run_root, dependency, None, None,
        )?);
    }

    let host_source_root = run_root.join("Sources").join("__OrbiPreviewHost");
    ensure_dir(&host_source_root)?;
    let host_source_path = host_source_root.join("OrbiPreviewHostApp.swift");
    fs::write(
        &host_source_path,
        render_preview_host_app(platform, preview.raw_traits.as_deref()),
    )
    .with_context(|| format!("failed to write {}", host_source_path.display()))?;

    let mut cloned_app_target = clone_preview_target(
        project,
        &run_root,
        source_target,
        Some(&preview.source_file),
        Some(&helper_code),
    )?;
    cloned_app_target.name = app_target.name.clone();
    cloned_app_target.kind = app_target.kind;
    cloned_app_target.bundle_id = format!("{}.orbi.previewshot", app_target.bundle_id);
    cloned_app_target.display_name = app_target.display_name.clone();
    cloned_app_target.build_number = app_target.build_number.clone();
    cloned_app_target.platforms = app_target.platforms.clone();
    cloned_app_target.sources.push(host_source_root);
    cloned_app_target.dependencies = dependency_targets
        .iter()
        .map(|target| target.name.clone())
        .collect();
    cloned_app_target.info_plist = app_target.info_plist.clone();
    cloned_app_target.ios = app_target.ios.clone();
    synthetic_targets.push(cloned_app_target);

    let mut platforms = BTreeMap::new();
    platforms.insert(
        platform,
        project
            .resolved_manifest
            .platforms
            .get(&platform)
            .cloned()
            .context("platform configuration missing from manifest")?,
    );
    let synthetic_manifest = ResolvedManifest {
        name: format!("{} Preview", project.resolved_manifest.name),
        version: project.resolved_manifest.version.clone(),
        xcode: project.resolved_manifest.xcode.clone(),
        hooks: project.resolved_manifest.hooks.clone(),
        tests: Default::default(),
        quality: Default::default(),
        platforms,
        targets: synthetic_targets,
    };

    let manifest_path = run_root.join("orbi.preview.json");
    fs::write(
        &manifest_path,
        format!(
            "{{\n  \"$schema\": \"{}\",\n  \"name\": \"{}\"\n}}\n",
            crate::apple::manifest::SCHEMA_URL,
            synthetic_manifest.name
        ),
    )
    .with_context(|| format!("failed to write {}", manifest_path.display()))?;

    let screenshot_path = output
        .map(|path| resolve_path(&project.app.cwd, path))
        .unwrap_or_else(|| {
            artifacts_dir.join(format!(
                "{}.png",
                sanitize_component(preview_display_name(preview).as_str())
            ))
        });
    let (applied_traits, ignored_traits) = classify_preview_traits(preview.raw_traits.as_deref());

    Ok(GeneratedPreviewProject {
        project: ProjectContext {
            app: project.app.clone(),
            // Keep hooks anchored to the real project root and manifest so
            // relative hook commands behave the same way as normal builds.
            root: project.root.clone(),
            manifest_path: project.manifest_path.clone(),
            manifest_schema: ManifestSchema::AppleAppV1,
            resolved_manifest: synthetic_manifest,
            selected_xcode: project.selected_xcode.clone(),
            project_paths: ProjectPaths {
                orbi_dir,
                build_dir,
                artifacts_dir,
                receipts_dir,
            },
        },
        run_root,
        screenshot_path,
        ignored_traits,
        applied_traits,
    })
}

fn collect_library_dependency_targets<'a>(
    project: &'a ProjectContext,
    app_target: &'a TargetManifest,
) -> Result<Vec<&'a TargetManifest>> {
    let eligible = project
        .resolved_manifest
        .topological_targets(&app_target.name)?
        .into_iter()
        .filter(|target| target.name != app_target.name)
        .filter(|target| {
            matches!(
                target.kind,
                TargetKind::Framework | TargetKind::StaticLibrary | TargetKind::DynamicLibrary
            )
        })
        .collect::<Vec<_>>();
    Ok(eligible)
}

fn clone_preview_target(
    project: &ProjectContext,
    run_root: &Path,
    target: &TargetManifest,
    helper_source_file: Option<&Path>,
    helper_code: Option<&str>,
) -> Result<TargetManifest> {
    let mut sources = Vec::new();
    for (index, source_root) in target.sources.iter().enumerate() {
        let resolved_root = resolve_path(&project.root, source_root);
        let destination_root = PathBuf::from("Sources")
            .join("__OrbiPreview")
            .join(sanitize_component(target.name.as_str()))
            .join(format!("root-{index}"));
        let destination = run_root.join(&destination_root);
        copy_preview_source_root(
            &resolved_root,
            &destination,
            helper_source_file,
            helper_code,
        )?;
        sources.push(destination);
    }

    Ok(TargetManifest {
        name: target.name.clone(),
        kind: target.kind,
        bundle_id: target.bundle_id.clone(),
        display_name: target.display_name.clone(),
        build_number: target.build_number.clone(),
        platforms: target.platforms.clone(),
        sources,
        resources: absolutize_paths(&project.root, &target.resources),
        dependencies: target.dependencies.clone(),
        frameworks: target.frameworks.clone(),
        weak_frameworks: target.weak_frameworks.clone(),
        system_libraries: target.system_libraries.clone(),
        xcframeworks: absolutize_xcframeworks(&project.root, &target.xcframeworks),
        swift_packages: absolutize_swift_packages(&project.root, &target.swift_packages),
        info_plist: target.info_plist.clone(),
        ios: target.ios.clone(),
        entitlements: None,
        push: None,
        extension: None,
    })
}

fn copy_preview_source_root(
    source_root: &Path,
    destination_root: &Path,
    helper_source_file: Option<&Path>,
    helper_code: Option<&str>,
) -> Result<()> {
    ensure_dir(destination_root)?;
    for entry in WalkDir::new(source_root) {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(source_root)
            .with_context(|| format!("failed to relativize {}", path.display()))?;
        let destination = destination_root.join(relative);
        if entry.file_type().is_dir() {
            ensure_dir(&destination)?;
            continue;
        }

        if path.extension().and_then(|value| value.to_str()) == Some("swift") {
            let contents = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let mut contents = strip_main_attributes(&contents);
            if helper_source_file.is_some_and(|candidate| candidate == path)
                && let Some(helper_code) = helper_code
            {
                if !contents.ends_with('\n') {
                    contents.push('\n');
                }
                contents.push('\n');
                contents.push_str(helper_code);
            }
            fs::write(&destination, contents)
                .with_context(|| format!("failed to write {}", destination.display()))?;
        } else {
            copy_file(path, &destination)?;
        }
    }
    Ok(())
}

fn strip_main_attributes(source: &str) -> String {
    let mut stripped = String::new();
    for line in source.lines() {
        stripped.push_str(&strip_main_attribute_line(line));
        stripped.push('\n');
    }
    stripped
}

fn strip_main_attribute_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("@main") {
        return line.to_owned();
    }
    let remainder = &trimmed["@main".len()..];
    if remainder
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return line.to_owned();
    }
    let indent_len = line.len() - trimmed.len();
    let indent = &line[..indent_len];
    format!("{indent}{}", remainder.trim_start())
}

fn render_preview_helper(preview: &DiscoveredPreview) -> String {
    format!(
        "@MainActor\nfunc {PREVIEW_HELPER_NAME}() -> Any {{\n{}\n}}\n",
        indent_block(preview.helper_body.as_str(), 4)
    )
}

fn render_preview_host_app(platform: ApplePlatform, raw_traits: Option<&str>) -> String {
    let layout_modifier = render_preview_layout_modifier(raw_traits);
    let orientation_note = raw_traits
        .and_then(orientation_trait_note)
        .map(|note| format!("\n        // {note}"))
        .unwrap_or_default();
    let root_view = if layout_modifier.is_empty() {
        "OrbiPreviewContent(content: __orbi_make_preview_content())".to_owned()
    } else {
        format!("OrbiPreviewContent(content: __orbi_make_preview_content()){layout_modifier}")
    };
    let platform_import = match platform {
        ApplePlatform::Macos => "\nimport AppKit",
        ApplePlatform::Ios | ApplePlatform::Tvos | ApplePlatform::Visionos => "\nimport UIKit",
        ApplePlatform::Watchos => "",
    };
    let platform_cases = render_preview_platform_cases(platform);
    let platform_wrappers = render_preview_platform_wrappers(platform);
    format!(
        "import SwiftUI{platform_import}\n\n@main\nstruct OrbiPreviewHostApp: App {{\n    var body: some Scene {{\n        WindowGroup {{\n            {root_view}{orientation_note}\n        }}\n    }}\n}}\n\nprivate struct OrbiPreviewContent: View {{\n    let content: Any\n\n    var body: some View {{\n        if let view = content as? any View {{\n            AnyView(view)\n{platform_cases}        }} else {{\n            Text(\"Unsupported preview content: \\(String(describing: type(of: content)))\")\n        }}\n    }}\n}}\n{platform_wrappers}"
    )
}

fn render_preview_platform_cases(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => {
            "        } else if let view = content as? NSView {\n            OrbiNSViewPreview(view: view)\n        } else if let controller = content as? NSViewController {\n            OrbiNSViewControllerPreview(controller: controller)\n"
        }
        ApplePlatform::Ios | ApplePlatform::Tvos | ApplePlatform::Visionos => {
            "        } else if let view = content as? UIView {\n            OrbiUIViewPreview(view: view)\n        } else if let controller = content as? UIViewController {\n            OrbiUIViewControllerPreview(controller: controller)\n"
        }
        ApplePlatform::Watchos => "",
    }
}

fn render_preview_platform_wrappers(platform: ApplePlatform) -> &'static str {
    match platform {
        ApplePlatform::Macos => {
            "\nprivate struct OrbiNSViewPreview: NSViewRepresentable {\n    let view: NSView\n\n    func makeNSView(context: Context) -> NSView {\n        view\n    }\n\n    func updateNSView(_ nsView: NSView, context: Context) {}\n}\n\nprivate struct OrbiNSViewControllerPreview: NSViewControllerRepresentable {\n    let controller: NSViewController\n\n    func makeNSViewController(context: Context) -> NSViewController {\n        controller\n    }\n\n    func updateNSViewController(_ nsViewController: NSViewController, context: Context) {}\n}\n"
        }
        ApplePlatform::Ios | ApplePlatform::Tvos | ApplePlatform::Visionos => {
            "\nprivate struct OrbiUIViewPreview: UIViewRepresentable {\n    let view: UIView\n\n    func makeUIView(context: Context) -> UIView {\n        view\n    }\n\n    func updateUIView(_ uiView: UIView, context: Context) {}\n}\n\nprivate struct OrbiUIViewControllerPreview: UIViewControllerRepresentable {\n    let controller: UIViewController\n\n    func makeUIViewController(context: Context) -> UIViewController {\n        controller\n    }\n\n    func updateUIViewController(_ uiViewController: UIViewController, context: Context) {}\n}\n"
        }
        ApplePlatform::Watchos => "",
    }
}

fn render_preview_layout_modifier(raw_traits: Option<&str>) -> String {
    let Some(raw_traits) = raw_traits else {
        return String::new();
    };
    if raw_traits.contains(".sizeThatFitsLayout") {
        return "\n                .previewLayout(.sizeThatFits)".to_owned();
    }

    let marker = ".fixedLayout(width:";
    let Some(start) = raw_traits.find(marker) else {
        return String::new();
    };
    let fixed = &raw_traits[start + marker.len()..];
    let Some(comma) = fixed.find(',') else {
        return String::new();
    };
    let width = fixed[..comma].trim();
    let fixed = &fixed[comma + 1..];
    let Some(height_marker) = fixed.find("height:") else {
        return String::new();
    };
    let height = fixed[height_marker + "height:".len()..]
        .split(')')
        .next()
        .unwrap_or_default()
        .trim();
    if width.is_empty() || height.is_empty() {
        return String::new();
    }
    format!("\n                .previewLayout(.fixed(width: {width}, height: {height}))")
}

fn classify_preview_traits(raw_traits: Option<&str>) -> (Vec<String>, Vec<String>) {
    let Some(raw_traits) = raw_traits else {
        return (Vec::new(), Vec::new());
    };
    let mut applied = Vec::new();
    let mut ignored = Vec::new();
    if raw_traits.contains(".sizeThatFitsLayout") {
        applied.push("sizeThatFitsLayout".to_owned());
    }
    if raw_traits.contains(".fixedLayout(") {
        applied.push("fixedLayout".to_owned());
    }
    for orientation in [
        ".portrait",
        ".portraitUpsideDown",
        ".landscapeLeft",
        ".landscapeRight",
    ] {
        if raw_traits.contains(orientation) {
            ignored.push(orientation.trim_start_matches('.').to_owned());
        }
    }
    (applied, ignored)
}

fn orientation_trait_note(raw_traits: &str) -> Option<&'static str> {
    if raw_traits.contains(".landscapeLeft") || raw_traits.contains(".landscapeRight") {
        return Some("preview orientation traits are not applied yet");
    }
    if raw_traits.contains(".portrait") || raw_traits.contains(".portraitUpsideDown") {
        return Some("preview orientation traits are not applied yet");
    }
    None
}

fn absolutize_paths(root: &Path, paths: &[PathBuf]) -> Vec<PathBuf> {
    paths.iter().map(|path| resolve_path(root, path)).collect()
}

fn absolutize_xcframeworks(
    root: &Path,
    dependencies: &[XcframeworkDependency],
) -> Vec<XcframeworkDependency> {
    dependencies
        .iter()
        .map(|dependency| XcframeworkDependency {
            path: resolve_path(root, &dependency.path),
            library: dependency.library.clone(),
            embed: dependency.embed,
        })
        .collect()
}

fn absolutize_swift_packages(
    root: &Path,
    dependencies: &[SwiftPackageDependency],
) -> Vec<SwiftPackageDependency> {
    dependencies
        .iter()
        .map(|dependency| SwiftPackageDependency {
            product: dependency.product.clone(),
            source: match &dependency.source {
                SwiftPackageSource::Path { path } => SwiftPackageSource::Path {
                    path: resolve_path(root, path),
                },
                SwiftPackageSource::Git {
                    url,
                    version,
                    revision,
                } => SwiftPackageSource::Git {
                    url: url.clone(),
                    version: version.clone(),
                    revision: revision.clone(),
                },
            },
        })
        .collect()
}

fn render_preview_shot(
    project: &ProjectContext,
    platform: ApplePlatform,
    screenshot_path: &Path,
    delay_ms: u64,
) -> Result<(PathBuf, PathBuf)> {
    let destination = preview_destination(platform);
    let outcome = build::build_for_testing_destination(project, platform, destination)?;
    ensure_parent_dir(screenshot_path)?;
    let backend = preview_backend(project, platform, &outcome.receipt)?;
    backend.launch_app(&outcome.receipt.bundle_id)?;
    thread::sleep(Duration::from_millis(delay_ms));
    backend.take_screenshot(screenshot_path)?;
    let _ = backend.stop_app(&outcome.receipt.bundle_id);
    Ok((outcome.receipt_path, screenshot_path.to_path_buf()))
}

fn preview_destination(platform: ApplePlatform) -> DestinationKind {
    match platform {
        ApplePlatform::Macos => DestinationKind::Device,
        _ => DestinationKind::Simulator,
    }
}

fn preview_backend(
    project: &ProjectContext,
    platform: ApplePlatform,
    receipt: &crate::apple::build::receipt::BuildReceipt,
) -> Result<PreviewRuntimeBackend> {
    match platform {
        ApplePlatform::Ios
        | ApplePlatform::Tvos
        | ApplePlatform::Visionos
        | ApplePlatform::Watchos => Ok(PreviewRuntimeBackend::Simulator(
            SimulatorPreviewBackend::prepare(project, platform, receipt)?,
        )),
        ApplePlatform::Macos => Ok(PreviewRuntimeBackend::Macos(Box::new(
            MacosBackend::prepare(project, receipt)?,
        ))),
    }
}

impl PreviewRuntimeBackend {
    fn launch_app(&self, bundle_id: &str) -> Result<()> {
        match self {
            Self::Simulator(backend) => backend.launch_app(bundle_id),
            Self::Macos(backend) => backend.launch_app(bundle_id, true, &[]),
        }
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        match self {
            Self::Simulator(backend) => backend.stop_app(bundle_id),
            Self::Macos(backend) => backend.stop_app(bundle_id),
        }
    }

    fn take_screenshot(&self, path: &Path) -> Result<()> {
        match self {
            Self::Simulator(backend) => backend.take_screenshot(path),
            Self::Macos(backend) => backend.take_screenshot(path),
        }
    }
}

impl SimulatorPreviewBackend {
    fn prepare(
        project: &ProjectContext,
        platform: ApplePlatform,
        receipt: &crate::apple::build::receipt::BuildReceipt,
    ) -> Result<Self> {
        let backend = Self::attach(project, platform)?;
        let mut install = xcrun_command(backend.selected_xcode.as_ref());
        install.args([
            "simctl",
            "install",
            &backend.device.udid,
            receipt
                .bundle_path
                .to_str()
                .context("bundle path contains invalid UTF-8")?,
        ]);
        run_command(&mut install)?;
        Ok(backend)
    }

    fn attach(project: &ProjectContext, platform: ApplePlatform) -> Result<Self> {
        let device = select_simulator_device(project, platform)?;
        if !device.is_booted() {
            let mut boot = xcrun_command(project.selected_xcode.as_ref());
            boot.args(["simctl", "boot", &device.udid]);
            run_command(&mut boot)?;
        }

        let mut bootstatus = xcrun_command(project.selected_xcode.as_ref());
        bootstatus.args(["simctl", "bootstatus", &device.udid, "-b"]);
        run_command(&mut bootstatus)?;

        Ok(Self {
            device,
            selected_xcode: project.selected_xcode.clone(),
        })
    }

    fn launch_app(&self, bundle_id: &str) -> Result<()> {
        self.stop_app(bundle_id)?;
        let mut command = xcrun_command(self.selected_xcode.as_ref());
        command.args(["simctl", "launch", &self.device.udid, bundle_id]);
        run_command_capture(&mut command).map(|_| ())
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        let mut command = xcrun_command(self.selected_xcode.as_ref());
        command.args(["simctl", "terminate", &self.device.udid, bundle_id]);
        let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
        if success {
            return Ok(());
        }
        let combined = format!("{stdout}\n{stderr}").to_ascii_lowercase();
        if combined.contains("found nothing to terminate") || combined.contains("not running") {
            return Ok(());
        }
        bail!("failed to stop `{bundle_id}` on {}", self.device.name)
    }

    fn take_screenshot(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let mut command = xcrun_command(self.selected_xcode.as_ref());
        command.args([
            "simctl",
            "io",
            &self.device.udid,
            "screenshot",
            path.to_str()
                .context("screenshot path contains invalid UTF-8")?,
        ]);
        run_command_capture(&mut command).map(|_| ())
    }
}

fn sanitize_component(value: &str) -> String {
    let mut component = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            component.push(character.to_ascii_lowercase());
        } else if !component.ends_with('-') {
            component.push('-');
        }
    }
    component
        .trim_matches('-')
        .to_owned()
        .tap_if_empty("preview")
}

fn trim_common_indent(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let common_indent = lines
        .iter()
        .filter_map(|line| {
            if line.trim().is_empty() {
                None
            } else {
                Some(
                    line.chars()
                        .take_while(|character| character.is_whitespace())
                        .count(),
                )
            }
        })
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                line.chars().skip(common_indent).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn indent_block(text: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    text.lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

trait TapIfEmpty {
    fn tap_if_empty(self, fallback: &str) -> String;
}

impl TapIfEmpty for String {
    fn tap_if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.to_owned()
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::{
        DiscoveredPreview, discoverable_preview_target_names, extract_function_body,
        extract_preview_constructor, parse_macro_expansion_sections, parse_preview_name,
        parse_preview_traits, preview_target_plan, render_preview_helper, render_preview_host_app,
        strip_main_attribute_line, trim_common_indent,
    };
    use crate::manifest::{
        ApplePlatform, PlatformManifest, ResolvedManifest, TargetKind, TargetManifest,
    };

    fn target(
        name: &str,
        kind: TargetKind,
        platforms: Vec<ApplePlatform>,
        dependencies: Vec<&str>,
    ) -> TargetManifest {
        TargetManifest {
            name: name.to_owned(),
            kind,
            bundle_id: format!("dev.orbi.tests.{}", name.to_ascii_lowercase()),
            display_name: None,
            build_number: None,
            platforms,
            sources: Vec::new(),
            resources: Vec::new(),
            dependencies: dependencies.into_iter().map(ToOwned::to_owned).collect(),
            frameworks: Vec::new(),
            weak_frameworks: Vec::new(),
            system_libraries: Vec::new(),
            xcframeworks: Vec::new(),
            swift_packages: Vec::new(),
            info_plist: BTreeMap::new(),
            ios: None,
            entitlements: None,
            push: None,
            extension: None,
        }
    }

    fn manifest(platforms: Vec<ApplePlatform>, targets: Vec<TargetManifest>) -> ResolvedManifest {
        ResolvedManifest {
            name: "PreviewTests".to_owned(),
            version: "0.1.0".to_owned(),
            xcode: None,
            hooks: Default::default(),
            tests: Default::default(),
            quality: Default::default(),
            platforms: platforms
                .into_iter()
                .map(|platform| {
                    (
                        platform,
                        PlatformManifest {
                            deployment_target: "1.0".to_owned(),
                            universal_binary: false,
                        },
                    )
                })
                .collect(),
            targets,
        }
    }

    #[test]
    fn watchos_discovery_includes_watch_extension_dependency() {
        let manifest = manifest(
            vec![ApplePlatform::Watchos],
            vec![
                target(
                    "WatchExtension",
                    TargetKind::WatchExtension,
                    vec![ApplePlatform::Watchos],
                    vec![],
                ),
                target(
                    "WatchApp",
                    TargetKind::WatchApp,
                    vec![ApplePlatform::Watchos],
                    vec!["WatchExtension"],
                ),
            ],
        );

        let names = discoverable_preview_target_names(&manifest, ApplePlatform::Watchos).unwrap();

        assert_eq!(names, vec!["WatchApp", "WatchExtension"]);
    }

    #[test]
    fn watch_extension_preview_uses_watch_app_runtime_target() {
        let manifest = manifest(
            vec![ApplePlatform::Watchos],
            vec![
                target(
                    "WatchExtension",
                    TargetKind::WatchExtension,
                    vec![ApplePlatform::Watchos],
                    vec![],
                ),
                target(
                    "WatchApp",
                    TargetKind::WatchApp,
                    vec![ApplePlatform::Watchos],
                    vec!["WatchExtension"],
                ),
            ],
        );

        let plan =
            preview_target_plan(&manifest, ApplePlatform::Watchos, "WatchExtension").unwrap();

        assert_eq!(plan.runtime_target.name, "WatchApp");
        assert_eq!(plan.source_target.name, "WatchExtension");
    }

    #[test]
    fn tvos_discovery_uses_app_target() {
        let manifest = manifest(
            vec![ApplePlatform::Tvos],
            vec![target(
                "TVApp",
                TargetKind::App,
                vec![ApplePlatform::Tvos],
                vec![],
            )],
        );

        let names = discoverable_preview_target_names(&manifest, ApplePlatform::Tvos).unwrap();

        assert_eq!(names, vec!["TVApp"]);
    }

    #[test]
    fn visionos_discovery_uses_app_target() {
        let manifest = manifest(
            vec![ApplePlatform::Visionos],
            vec![target(
                "VisionApp",
                TargetKind::App,
                vec![ApplePlatform::Visionos],
                vec![],
            )],
        );

        let names = discoverable_preview_target_names(&manifest, ApplePlatform::Visionos).unwrap();

        assert_eq!(names, vec!["VisionApp"]);
    }

    #[test]
    fn non_watch_preview_rejects_non_runtime_target() {
        let manifest = manifest(
            vec![ApplePlatform::Tvos],
            vec![
                target(
                    "Helper",
                    TargetKind::Framework,
                    vec![ApplePlatform::Tvos],
                    vec![],
                ),
                target(
                    "TVApp",
                    TargetKind::App,
                    vec![ApplePlatform::Tvos],
                    vec!["Helper"],
                ),
            ],
        );

        let error = preview_target_plan(&manifest, ApplePlatform::Tvos, "Helper")
            .err()
            .unwrap()
            .to_string();

        assert!(error.contains("default app target `TVApp`"));
    }

    #[test]
    fn parses_preview_sections_from_macro_output() {
        let output = "warning: save unknown driver flag\n@__swiftmacro_test.swift\n------------------------------\nstruct Example {}\n------------------------------\n";
        let sections = parse_macro_expansion_sections(output);
        assert_eq!(sections, vec!["struct Example {}".to_owned()]);
    }

    #[test]
    fn extracts_make_preview_function_body() {
        let section = r#"
struct Example: DeveloperToolsSupport.PreviewRegistry {
    static func makePreview() throws -> DeveloperToolsSupport.Preview {
        DeveloperToolsSupport.Preview("Basic") {
            Text("Hi")
        }
    }
}
"#;
        let body = extract_function_body(
            section,
            "static func makePreview() throws -> DeveloperToolsSupport.Preview",
        )
        .unwrap();
        assert!(body.contains(r#"DeveloperToolsSupport.Preview("Basic")"#));
    }

    #[test]
    fn parses_preview_name_and_traits() {
        assert_eq!(
            parse_preview_name(r#""Basic", traits: .sizeThatFitsLayout"#),
            Some("Basic".to_owned())
        );
        assert_eq!(
            parse_preview_traits(r#""Basic", traits: .sizeThatFitsLayout, .landscapeLeft"#),
            Some(".sizeThatFitsLayout, .landscapeLeft".to_owned())
        );
        assert_eq!(parse_preview_name("traits: .sizeThatFitsLayout"), None);
        assert_eq!(parse_preview_name(""), None);
    }

    #[test]
    fn extracts_named_and_unnamed_preview_constructor_forms() {
        let (named_args, named_body) = extract_preview_constructor(
            r#"
DeveloperToolsSupport.Preview("Basic") {
    Text("Hi")
}
"#,
        )
        .unwrap();
        assert_eq!(named_args, r#""Basic""#);
        assert_eq!(named_body, r#"Text("Hi")"#);

        let (unnamed_args, unnamed_body) = extract_preview_constructor(
            r#"
DeveloperToolsSupport.Preview {
    Text("Hi")
}
"#,
        )
        .unwrap();
        assert_eq!(unnamed_args, "");
        assert_eq!(unnamed_body, r#"Text("Hi")"#);
    }

    #[test]
    fn preview_helper_returns_type_erased_content() {
        let preview = DiscoveredPreview {
            target_name: "App".to_owned(),
            source_file: PathBuf::from("Sources/App/HomeViewController.swift"),
            line: 10,
            column: 1,
            name: Some("Controller".to_owned()),
            helper_body: "return HomeViewController()".to_owned(),
            raw_traits: None,
        };

        let helper = render_preview_helper(&preview);

        assert!(helper.contains("func __orbi_make_preview_content() -> Any"));
        assert!(helper.contains("return HomeViewController()"));
    }

    #[test]
    fn preview_host_wraps_appkit_preview_content() {
        let host = render_preview_host_app(ApplePlatform::Macos, None);

        assert!(host.contains("import AppKit"));
        assert!(host.contains("if let view = content as? any View"));
        assert!(host.contains("content as? NSView"));
        assert!(host.contains("content as? NSViewController"));
        assert!(host.contains("NSViewRepresentable"));
        assert!(host.contains("NSViewControllerRepresentable"));
        assert!(!host.contains("import UIKit"));
    }

    #[test]
    fn preview_host_wraps_uikit_preview_content() {
        let host = render_preview_host_app(ApplePlatform::Ios, None);

        assert!(host.contains("import UIKit"));
        assert!(host.contains("if let view = content as? any View"));
        assert!(host.contains("content as? UIView"));
        assert!(host.contains("content as? UIViewController"));
        assert!(host.contains("UIViewRepresentable"));
        assert!(host.contains("UIViewControllerRepresentable"));
        assert!(!host.contains("import AppKit"));
    }

    #[test]
    fn strips_main_attribute_lines_without_touching_main_actor() {
        assert_eq!(
            strip_main_attribute_line("@main struct ExampleApp: App {"),
            "struct ExampleApp: App {"
        );
        assert_eq!(strip_main_attribute_line("    @main"), "    ");
        assert_eq!(
            strip_main_attribute_line("    @MainActor var value = 1"),
            "    @MainActor var value = 1"
        );
    }

    #[test]
    fn trims_common_indent_in_preview_helper_body() {
        let body = trim_common_indent(
            "            struct Wrapper: View {\n                var body: some View {\n                    Text(\"Hi\")\n                }\n            }\n",
        );
        assert_eq!(
            body,
            "struct Wrapper: View {\n    var body: some View {\n        Text(\"Hi\")\n    }\n}"
        );
    }
}
