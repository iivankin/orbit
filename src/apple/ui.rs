use anyhow::Result;

use crate::apple::runtime;
use crate::apple::testing::ui::{self, UiCrashDeleteRequest, UiCrashQuery, backend::UiBackend};
use crate::cli::{
    UiAddMediaArgs, UiCrashArgs, UiCrashCommand, UiDescribePointArgs, UiDoctorArgs, UiDumpTreeArgs,
    UiFocusArgs, UiInstallDylibArgs, UiInstrumentsArgs, UiLogsArgs, UiOpenArgs,
    UiUpdateContactsArgs,
};
use crate::context::ProjectContext;

pub fn doctor(project: &ProjectContext, args: &UiDoctorArgs) -> Result<()> {
    let platform = runtime::resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to inspect",
    )?;
    ui::doctor(project, platform)
}

pub fn dump_tree(project: &ProjectContext, args: &UiDumpTreeArgs) -> Result<()> {
    let platform = runtime::resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to inspect",
    )?;
    let tree = ui::dump_tree_json(project, platform)?;
    println!("{}", serde_json::to_string_pretty(&tree)?);
    Ok(())
}

pub fn describe_point(project: &ProjectContext, args: &UiDescribePointArgs) -> Result<()> {
    let platform = runtime::resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to inspect",
    )?;
    let value = ui::describe_point_json(project, platform, args.x, args.y)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

pub fn focus(project: &ProjectContext, args: &UiFocusArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.focus()
}

pub fn logs(project: &ProjectContext, args: &UiLogsArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.stream_logs(&args.log_args)
}

pub fn add_media(project: &ProjectContext, args: &UiAddMediaArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.add_media(&args.paths)
}

pub fn open(project: &ProjectContext, args: &UiOpenArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.open_link(&args.url)
}

pub fn install_dylib(project: &ProjectContext, args: &UiInstallDylibArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.install_dylib(&args.path)
}

pub fn instruments(project: &ProjectContext, args: &UiInstrumentsArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.run_instruments(&args.template, &args.instrument_args)
}

pub fn update_contacts(project: &ProjectContext, args: &UiUpdateContactsArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    backend.update_contacts(&args.path)
}

pub fn crash(project: &ProjectContext, args: &UiCrashArgs) -> Result<()> {
    let backend = attach_backend(project, args.platform.map(runtime::apple_platform_from_cli))?;
    match &args.command {
        UiCrashCommand::List(command) => backend.list_crash_logs(&UiCrashQuery {
            before: command.before.clone(),
            since: command.since.clone(),
            bundle_id: command.bundle_id.clone(),
        }),
        UiCrashCommand::Show(command) => backend.show_crash_log(&command.name),
        UiCrashCommand::Delete(command) => backend.delete_crash_logs(&UiCrashDeleteRequest {
            name: command.name.clone(),
            before: command.before.clone(),
            since: command.since.clone(),
            bundle_id: command.bundle_id.clone(),
            delete_all: command.all,
        }),
    }
}

pub fn reset_idb() -> Result<()> {
    ui::reset_idb()
}

fn attach_backend(
    project: &ProjectContext,
    platform: Option<crate::manifest::ApplePlatform>,
) -> Result<Box<dyn UiBackend>> {
    let platform = runtime::resolve_platform(project, platform, "Select a platform to inspect")?;
    ui::attach_backend(project, platform)
}
