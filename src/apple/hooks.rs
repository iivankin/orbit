use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::context::ProjectContext;
use crate::manifest::{ApplePlatform, BuildConfiguration, DistributionKind, HooksManifest};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HookKind {
    BeforeBuild,
    BeforeRun,
    AfterSign,
}

impl HookKind {
    fn key(self) -> &'static str {
        match self {
            HookKind::BeforeBuild => "before_build",
            HookKind::BeforeRun => "before_run",
            HookKind::AfterSign => "after_sign",
        }
    }

    fn commands(self, hooks: &HooksManifest) -> &[String] {
        match self {
            HookKind::BeforeBuild => &hooks.before_build,
            HookKind::BeforeRun => &hooks.before_run,
            HookKind::AfterSign => &hooks.after_sign,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct HookContext<'a> {
    pub target_name: Option<&'a str>,
    pub platform: Option<ApplePlatform>,
    pub distribution: Option<DistributionKind>,
    pub configuration: Option<BuildConfiguration>,
    pub destination: Option<&'a str>,
    pub bundle_path: Option<&'a Path>,
    pub artifact_path: Option<&'a Path>,
    pub receipt_path: Option<&'a Path>,
}

pub fn run_project_hooks(
    project: &ProjectContext,
    kind: HookKind,
    context: &HookContext<'_>,
) -> Result<()> {
    for command_text in kind.commands(&project.resolved_manifest.hooks) {
        let command_text = command_text.trim();
        if command_text.is_empty() {
            bail!("manifest hook `{}` cannot be empty", kind.key());
        }
        run_hook_command(project, kind, context, command_text)?;
    }
    Ok(())
}

fn run_hook_command(
    project: &ProjectContext,
    kind: HookKind,
    context: &HookContext<'_>,
    command_text: &str,
) -> Result<()> {
    // Hooks are configured as shell snippets in the manifest, so run them in a
    // predictable POSIX shell from the project root.
    let mut command = Command::new("/bin/sh");
    command.arg("-lc");
    command.arg(command_text);
    command.current_dir(&project.root);
    command.env("ORBI_HOOK", kind.key());
    command.env("ORBI_PROJECT_ROOT", &project.root);
    command.env("ORBI_MANIFEST_PATH", &project.manifest_path);
    if let Some(target_name) = context.target_name {
        command.env("ORBI_TARGET_NAME", target_name);
    }
    if let Some(platform) = context.platform {
        command.env("ORBI_PLATFORM", platform.to_string());
    }
    if let Some(distribution) = context.distribution {
        command.env("ORBI_DISTRIBUTION", distribution.as_str());
    }
    if let Some(configuration) = context.configuration {
        command.env("ORBI_CONFIGURATION", configuration.as_str());
    }
    if let Some(destination) = context.destination {
        command.env("ORBI_DESTINATION", destination);
    }
    if let Some(bundle_path) = context.bundle_path {
        command.env("ORBI_BUNDLE_PATH", bundle_path);
    }
    if let Some(artifact_path) = context.artifact_path {
        command.env("ORBI_ARTIFACT_PATH", artifact_path);
    }
    if let Some(receipt_path) = context.receipt_path {
        command.env("ORBI_RECEIPT_PATH", receipt_path);
    }

    let output = command
        .output()
        .with_context(|| format!("failed to execute `{}` hook `{command_text}`", kind.key()))?;
    if !output.stdout.is_empty() {
        print!("{}", String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    if !output.status.success() {
        bail!(
            "`{}` hook `{command_text}` failed with {}",
            kind.key(),
            output.status
        );
    }
    Ok(())
}
