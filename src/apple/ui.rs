mod direct;

use anyhow::Result;

use self::direct::DirectUiCommand;
use crate::apple::runtime;
use crate::apple::testing::ui::{self, UiCrashDeleteRequest, UiCrashQuery, backend::UiBackend};
use crate::cli::{
    UiAddMediaArgs, UiCleanTraceTempArgs, UiCommand as UiCliCommand, UiCrashArgs, UiCrashCommand,
    UiDescribePointArgs, UiDoctorArgs, UiDumpTreeArgs, UiFocusArgs, UiInstallDylibArgs,
    UiInstrumentsArgs, UiLogsArgs, UiOpenArgs, UiSchemaArgs, UiUpdateContactsArgs,
};
use crate::context::ProjectContext;

pub fn execute(project: &ProjectContext, command: &UiCliCommand) -> Result<()> {
    match command {
        UiCliCommand::Schema(args) => schema(args),
        UiCliCommand::Doctor(args) => doctor(project, args),
        UiCliCommand::CleanTraceTemp(args) => clean_trace_temp(args),
        UiCliCommand::DumpTree(args) => dump_tree(project, args),
        UiCliCommand::DescribePoint(args) => describe_point(project, args),
        UiCliCommand::Focus(args) => focus(project, args),
        UiCliCommand::LaunchApp(args) => run_direct(project, direct::launch_app(args)?),
        UiCliCommand::StopApp(args) => run_direct(project, direct::stop_app(args)),
        UiCliCommand::KillApp(args) => run_direct(project, direct::kill_app(args)),
        UiCliCommand::ClearState(args) => run_direct(project, direct::clear_state(args)),
        UiCliCommand::ClearKeychain(args) => run_direct(
            project,
            direct::clear_keychain(args.platform.map(runtime::apple_platform_from_cli)),
        ),
        UiCliCommand::Tap(args) => run_direct(project, direct::tap(args)?),
        UiCliCommand::Hover(args) => run_direct(project, direct::hover(args)?),
        UiCliCommand::RightClick(args) => run_direct(project, direct::right_click(args)?),
        UiCliCommand::TapPoint(args) => run_direct(project, direct::tap_point(args)?),
        UiCliCommand::DoubleTap(args) => run_direct(project, direct::double_tap(args)?),
        UiCliCommand::LongPress(args) => run_direct(project, direct::long_press(args)?),
        UiCliCommand::Swipe(args) => run_direct(project, direct::swipe(args)?),
        UiCliCommand::SwipeOn(args) => run_direct(project, direct::swipe_on(args)?),
        UiCliCommand::Drag(args) => run_direct(project, direct::drag(args)?),
        UiCliCommand::Scroll(args) => run_direct(project, direct::scroll(args)),
        UiCliCommand::ScrollOn(args) => run_direct(project, direct::scroll_on(args)?),
        UiCliCommand::ScrollUntilVisible(args) => {
            run_direct(project, direct::scroll_until_visible(args)?)
        }
        UiCliCommand::InputText(args) => run_direct(project, direct::input_text(args)),
        UiCliCommand::EraseText(args) => run_direct(project, direct::erase_text(args)),
        UiCliCommand::PressKey(args) => run_direct(project, direct::press_key(args)?),
        UiCliCommand::PressKeyCode(args) => run_direct(project, direct::press_key_code(args)?),
        UiCliCommand::KeySequence(args) => run_direct(project, direct::key_sequence(args)),
        UiCliCommand::PressButton(args) => run_direct(project, direct::press_button(args)?),
        UiCliCommand::SelectMenuItem(args) => run_direct(project, direct::select_menu_item(args)?),
        UiCliCommand::HideKeyboard(args) => run_direct(
            project,
            direct::hide_keyboard(args.platform.map(runtime::apple_platform_from_cli)),
        ),
        UiCliCommand::AssertVisible(args) => run_direct(project, direct::assert_visible(args)?),
        UiCliCommand::AssertNotVisible(args) => {
            run_direct(project, direct::assert_not_visible(args)?)
        }
        UiCliCommand::WaitUntil(args) => run_direct(project, direct::wait_until(args)?),
        UiCliCommand::WaitForAnimationToEnd(args) => {
            run_direct(project, direct::wait_for_animation_to_end(args)?)
        }
        UiCliCommand::TakeScreenshot(args) => run_direct(project, direct::take_screenshot(args)),
        UiCliCommand::Logs(args) => logs(project, args),
        UiCliCommand::AddMedia(args) => add_media(project, args),
        UiCliCommand::Open(args) => open(project, args),
        UiCliCommand::SetLocation(args) => run_direct(project, direct::set_location(args)),
        UiCliCommand::SetPermissions(args) => run_direct(project, direct::set_permissions(args)?),
        UiCliCommand::Travel(args) => run_direct(project, direct::travel(args)?),
        UiCliCommand::InstallDylib(args) => install_dylib(project, args),
        UiCliCommand::Instruments(args) => instruments(project, args),
        UiCliCommand::UpdateContacts(args) => update_contacts(project, args),
        UiCliCommand::Crash(args) => crash(project, args),
        UiCliCommand::ResetIdb(_) => reset_idb(),
    }
}

pub fn schema(args: &UiSchemaArgs) -> Result<()> {
    let platform = args.platform.map(runtime::apple_platform_from_cli);
    if args.json {
        let value = ui::schema_json(platform);
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{}", ui::schema_text(platform));
    }
    Ok(())
}

pub fn doctor(project: &ProjectContext, args: &UiDoctorArgs) -> Result<()> {
    let platform = runtime::resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to inspect",
    )?;
    ui::doctor(project, platform)
}

pub fn clean_trace_temp(args: &UiCleanTraceTempArgs) -> Result<()> {
    ui::clean_trace_temp(args)
}

pub fn dump_tree(project: &ProjectContext, args: &UiDumpTreeArgs) -> Result<()> {
    let platform = resolve_platform(
        project,
        args.platform.map(runtime::apple_platform_from_cli),
        "Select a platform to inspect",
    )?;
    let tree = ui::dump_tree_json(project, platform)?;
    println!("{}", serde_json::to_string_pretty(&tree)?);
    Ok(())
}

pub fn describe_point(project: &ProjectContext, args: &UiDescribePointArgs) -> Result<()> {
    let platform = resolve_platform(
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

fn run_direct(project: &ProjectContext, command: DirectUiCommand) -> Result<()> {
    let platform = resolve_platform(project, command.platform, "Select a platform to inspect")?;
    ui::run_ui_command(
        project,
        platform,
        command.command,
        command.focus_after_launch,
    )
}

fn resolve_platform(
    project: &ProjectContext,
    platform: Option<crate::manifest::ApplePlatform>,
    prompt: &str,
) -> Result<crate::manifest::ApplePlatform> {
    runtime::resolve_platform(project, platform, prompt)
}

fn attach_backend(
    project: &ProjectContext,
    platform: Option<crate::manifest::ApplePlatform>,
) -> Result<Box<dyn UiBackend>> {
    let platform = resolve_platform(project, platform, "Select a platform to inspect")?;
    ui::attach_backend(project, platform)
}
