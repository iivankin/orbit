mod naming;
mod plan;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::apple;
use crate::context::AppContext;
use crate::manifest::installed_schema_path;
use crate::util::{print_success, prompt_input, prompt_select, resolve_path};

use self::naming::{bundle_id_suffix, looks_like_bundle_id, suggested_product_name};
use self::plan::{
    InitAnswers, InitEcosystem, InitTemplate, TemplateChoice, create_scaffold, scaffold_plan,
};

const ECOSYSTEM_CHOICES: [EcosystemChoice; 1] = [EcosystemChoice {
    kind: InitEcosystem::Apple,
    label: "Apple",
    description: "iOS, macOS, tvOS, watchOS, and visionOS apps",
}];

#[derive(Debug, Clone, Copy)]
struct EcosystemChoice {
    kind: InitEcosystem,
    label: &'static str,
    description: &'static str,
}

pub fn execute(app: &AppContext, requested_manifest: Option<&Path>) -> Result<()> {
    if !app.interactive {
        bail!("`orbit init` requires an interactive terminal");
    }

    let manifest_path = init_manifest_path(app, requested_manifest);
    if manifest_path.exists() {
        bail!("manifest already exists at {}", manifest_path.display());
    }

    let project_root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    let answers = collect_init_answers(project_root)?;
    let schema_reference = installed_schema_reference(app, answers.ecosystem);
    let plan = scaffold_plan(&answers, &schema_reference);

    create_scaffold(project_root, &manifest_path, &plan)?;
    print_success(format!("Created {}", manifest_path.display()));
    let bsp_path = apple::bsp::install_connection_file_for_manifest(&manifest_path)?;
    print_success(format!("Installed {}", bsp_path.display()));

    println!("Next commands:");
    for command in &plan.next_commands {
        println!("  {command}");
    }

    Ok(())
}

fn init_manifest_path(app: &AppContext, requested_manifest: Option<&Path>) -> PathBuf {
    requested_manifest.map_or_else(
        || app.cwd.join("orbit.json"),
        |path| resolve_path(&app.cwd, path),
    )
}

fn collect_init_answers(project_root: &Path) -> Result<InitAnswers> {
    let ecosystem = prompt_ecosystem()?;
    let default_name = suggested_product_name(project_root);
    let name = prompt_non_empty("Product name", Some(default_name.as_str()))?;
    let default_bundle_id = format!("dev.orbit.{}", bundle_id_suffix(&name));
    let bundle_id = prompt_validated(
        "Bundle ID",
        Some(default_bundle_id.as_str()),
        looks_like_bundle_id,
        "Enter a reverse-DNS bundle ID like `dev.orbit.exampleapp`.",
    )?;
    let template = prompt_template(ecosystem)?;

    Ok(InitAnswers {
        ecosystem,
        name,
        bundle_id,
        template,
    })
}

fn prompt_ecosystem() -> Result<InitEcosystem> {
    let labels = ECOSYSTEM_CHOICES
        .iter()
        .map(|choice| format!("{}: {}", choice.label, choice.description))
        .collect::<Vec<_>>();
    let index = prompt_select("Ecosystem", &labels)?;
    Ok(ECOSYSTEM_CHOICES[index].kind)
}

fn prompt_template(ecosystem: InitEcosystem) -> Result<InitTemplate> {
    let choices = ecosystem.template_choices();
    let labels = choices
        .iter()
        .map(render_template_label)
        .collect::<Vec<_>>();
    let index = prompt_select("Template", &labels)?;
    Ok(choices[index].kind)
}

fn render_template_label(choice: &TemplateChoice) -> String {
    format!("{}: {}", choice.label, choice.description)
}

fn installed_schema_reference(app: &AppContext, ecosystem: InitEcosystem) -> String {
    installed_schema_path(&app.global_paths.schema_dir, ecosystem.manifest_schema())
        .display()
        .to_string()
}

fn prompt_non_empty(prompt: &str, default: Option<&str>) -> Result<String> {
    prompt_validated(
        prompt,
        default,
        |value| !value.is_empty(),
        "Value cannot be empty.",
    )
}

fn prompt_validated(
    prompt: &str,
    default: Option<&str>,
    validator: impl Fn(&str) -> bool,
    error_message: &str,
) -> Result<String> {
    loop {
        let value = prompt_input(prompt, default)?;
        let value = value.trim();
        if validator(value) {
            return Ok(value.to_owned());
        }
        println!("{error_message}");
    }
}
