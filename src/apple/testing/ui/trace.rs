use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::backend::UiBackend;
use super::flow::{canonical_or_absolute, resolve_path_from_flow};
use super::parser::parse_ui_flow;
use super::{UiCommand, UiLaunchApp};
use crate::apple::build::receipt::BuildReceipt;
use crate::context::ProjectContext;

#[derive(Debug, Clone, Default)]
pub(super) struct UiTracePlan {
    pub(super) prelaunch_commands: Vec<UiCommand>,
    pub(super) launch: Option<UiLaunchApp>,
}

#[derive(Debug, Default)]
struct UiTracePlanState {
    prelaunch_commands: Vec<UiCommand>,
    launch: Option<UiLaunchApp>,
    seen_runtime_command: bool,
}

pub(super) fn plan_macos_ui_trace(flow_paths: &[PathBuf]) -> Result<UiTracePlan> {
    let mut state = UiTracePlanState::default();
    let mut stack = Vec::new();
    for flow_path in flow_paths {
        analyze_macos_ui_trace_flow(flow_path, &mut state, &mut stack)?;
    }
    Ok(UiTracePlan {
        prelaunch_commands: state.prelaunch_commands,
        launch: state.launch,
    })
}

pub(super) fn apply_macos_ui_trace_prelaunch(
    backend: &dyn UiBackend,
    bundle_id: &str,
    trace_plan: &UiTracePlan,
) -> Result<()> {
    for command in &trace_plan.prelaunch_commands {
        match command {
            UiCommand::ClearState(app_id) => {
                backend.clear_app_state(resolve_trace_bundle_id(
                    bundle_id,
                    app_id.as_deref(),
                    command.summary().as_str(),
                )?)?;
            }
            UiCommand::ClearKeychain => backend.clear_keychain()?,
            UiCommand::SetPermissions(config) => {
                backend.set_permissions(
                    resolve_trace_bundle_id(
                        bundle_id,
                        config.app_id.as_deref(),
                        command.summary().as_str(),
                    )?,
                    config,
                )?;
            }
            _ => unreachable!("trace plan only stores prelaunch-safe commands"),
        }
    }

    let Some(launch) = trace_plan.launch.as_ref() else {
        return Ok(());
    };
    let launch_bundle_id =
        resolve_trace_bundle_id(bundle_id, launch.app_id.as_deref(), "launchApp")?;
    if launch.clear_keychain {
        backend.clear_keychain()?;
    }
    if launch.clear_state {
        backend.clear_app_state(launch_bundle_id)?;
    }
    if let Some(permissions) = launch.permissions.as_ref() {
        backend.set_permissions(
            resolve_trace_bundle_id(
                launch_bundle_id,
                permissions.app_id.as_deref(),
                "launchApp.permissions",
            )?,
            permissions,
        )?;
    }
    Ok(())
}

pub(super) fn prepare_macos_ui_trace_launch_command(
    project: &ProjectContext,
    receipt: &BuildReceipt,
    launch: Option<&UiLaunchApp>,
) -> Result<Vec<String>> {
    let launch_executable =
        crate::apple::build::pipeline::prepare_macos_trace_launch_executable(project, receipt)?;
    let launch_executable = launch_executable.to_owned();
    let mut command = vec![launch_executable];
    if let Some(launch) = launch {
        for (key, value) in &launch.arguments {
            command.push(format!("-{key}"));
            command.push(value.clone());
        }
    }
    Ok(command)
}

fn analyze_macos_ui_trace_flow(
    flow_path: &Path,
    state: &mut UiTracePlanState,
    stack: &mut Vec<PathBuf>,
) -> Result<()> {
    let canonical = canonical_or_absolute(flow_path)?;
    if stack.contains(&canonical) {
        let chain = stack
            .iter()
            .map(|entry| entry.display().to_string())
            .chain([canonical.display().to_string()])
            .collect::<Vec<_>>()
            .join(" -> ");
        bail!("detected recursive `runFlow` chain while planning UI trace: {chain}");
    }

    let flow = parse_ui_flow(&canonical)?;
    stack.push(canonical);
    let result = analyze_macos_ui_trace_commands(
        flow.path.as_path(),
        flow.commands.as_slice(),
        state,
        stack,
    );
    stack.pop();
    result
}

fn analyze_macos_ui_trace_commands(
    flow_path: &Path,
    commands: &[UiCommand],
    state: &mut UiTracePlanState,
    stack: &mut Vec<PathBuf>,
) -> Result<()> {
    for command in commands {
        match command {
            UiCommand::LaunchApp(launch) => {
                if state.launch.is_some() {
                    bail!(
                        "UI test profiling currently supports only one `launchApp` across the traced suite; found another in `{}`",
                        flow_path.display()
                    );
                }
                if state.seen_runtime_command {
                    bail!(
                        "UI test profiling requires `launchApp` to happen before runtime interaction commands; move the launch earlier in `{}`",
                        flow_path.display()
                    );
                }
                state.launch = Some(launch.clone());
                state.seen_runtime_command = true;
            }
            UiCommand::StopApp(_) | UiCommand::KillApp(_) => {
                bail!(
                    "UI test profiling does not support `{}` because Orbit records one launched app process for the whole run",
                    command.summary()
                );
            }
            UiCommand::ClearState(_) | UiCommand::ClearKeychain | UiCommand::SetPermissions(_) => {
                if state.seen_runtime_command {
                    bail!(
                        "UI test profiling supports `{}` only before the traced app launches; move it into the prelaunch prefix in `{}`",
                        command.summary(),
                        flow_path.display()
                    );
                }
                state.prelaunch_commands.push(command.clone());
            }
            UiCommand::RunFlow(relative_path) => {
                let nested_path = resolve_path_from_flow(flow_path, relative_path);
                analyze_macos_ui_trace_flow(&nested_path, state, stack)?;
            }
            UiCommand::Repeat { times, commands } => {
                for _ in 0..*times {
                    analyze_macos_ui_trace_commands(flow_path, commands, state, stack)?;
                }
            }
            UiCommand::Retry { commands, .. } => {
                if commands_use_trace_sensitive_process_control(flow_path, commands, stack)? {
                    bail!(
                        "UI test profiling does not support app lifecycle or prelaunch commands inside `retry` blocks in `{}`",
                        flow_path.display()
                    );
                }
                analyze_macos_ui_trace_commands(flow_path, commands, state, stack)?;
            }
            _ => state.seen_runtime_command = true,
        }
    }
    Ok(())
}

fn commands_use_trace_sensitive_process_control(
    flow_path: &Path,
    commands: &[UiCommand],
    stack: &mut Vec<PathBuf>,
) -> Result<bool> {
    for command in commands {
        match command {
            UiCommand::LaunchApp(_)
            | UiCommand::StopApp(_)
            | UiCommand::KillApp(_)
            | UiCommand::ClearState(_)
            | UiCommand::ClearKeychain
            | UiCommand::SetPermissions(_) => return Ok(true),
            UiCommand::RunFlow(relative_path) => {
                let nested_path = resolve_path_from_flow(flow_path, relative_path);
                let canonical = canonical_or_absolute(&nested_path)?;
                if stack.contains(&canonical) {
                    continue;
                }
                let nested_flow = parse_ui_flow(&canonical)?;
                stack.push(canonical);
                let result = commands_use_trace_sensitive_process_control(
                    nested_flow.path.as_path(),
                    nested_flow.commands.as_slice(),
                    stack,
                );
                stack.pop();
                if result? {
                    return Ok(true);
                }
            }
            UiCommand::Repeat { commands, .. } | UiCommand::Retry { commands, .. } => {
                if commands_use_trace_sensitive_process_control(flow_path, commands, stack)? {
                    return Ok(true);
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

fn resolve_trace_bundle_id<'a>(
    bundle_id: &'a str,
    requested: Option<&'a str>,
    source: &str,
) -> Result<&'a str> {
    if let Some(requested) = requested
        && requested != bundle_id
    {
        bail!(
            "{source} targets `{requested}`, but traced UI runs currently support only Orbit's built app `{bundle_id}`"
        );
    }
    Ok(requested.unwrap_or(bundle_id))
}
