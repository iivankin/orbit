use anyhow::Result;

use crate::apple;
use crate::cli::{
    AppleCommand, AppleDeviceCommand, AppleSigningCommand, Cli, Command, DepsCommand, IdeCommand,
    UiCommand,
};
use crate::context::AppContext;

pub fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(_) => unreachable!("`init` is handled before Apple dispatch"),
        Command::Lint(args) => apple::quality::lint_project(app, args, cli.manifest.as_deref()),
        Command::Format(args) => apple::quality::format_project(app, args, cli.manifest.as_deref()),
        Command::Test(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::testing::run_tests(&project, args)
        }
        Command::Ui(ui_args) => match &ui_args.command {
            UiCommand::ResetIdb(_) => apple::ui::reset_idb(),
            UiCommand::DumpTree(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::dump_tree(&project, args)
            }
            UiCommand::DescribePoint(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::describe_point(&project, args)
            }
            UiCommand::Focus(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::focus(&project, args)
            }
            UiCommand::Logs(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::logs(&project, args)
            }
            UiCommand::AddMedia(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::add_media(&project, args)
            }
            UiCommand::Open(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::open(&project, args)
            }
            UiCommand::InstallDylib(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::install_dylib(&project, args)
            }
            UiCommand::Instruments(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::instruments(&project, args)
            }
            UiCommand::UpdateContacts(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::update_contacts(&project, args)
            }
            UiCommand::Crash(args) => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::crash(&project, args)
            }
        },
        Command::Deps(deps_args) => match &deps_args.command {
            DepsCommand::Update(args) => {
                apple::deps::update_dependencies(app, args, cli.manifest.as_deref())
            }
        },
        Command::Bsp(_) => apple::bsp::serve(app, cli.manifest.as_deref()),
        Command::Ide(ide_args) => match &ide_args.command {
            IdeCommand::InstallBuildServer(_) => {
                apple::bsp::install_connection_files(app, cli.manifest.as_deref())
            }
            IdeCommand::DumpArgs(args) => apple::ide::dump_args(app, args, cli.manifest.as_deref()),
        },
        Command::Run(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::build::run_on_destination(&project, args)
        }
        Command::Build(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::build::build_artifact(&project, args)
        }
        Command::Submit(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::submit::submit_artifact(&project, args)
        }
        Command::Clean(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::clean::clean_project(&project, args)
        }
        Command::Apple(apple_args) => match &apple_args.command {
            AppleCommand::Device { command } => match command {
                AppleDeviceCommand::List(args) => apple::device::list_devices(app, args),
                AppleDeviceCommand::Register(args) => apple::device::register_device(app, args),
                AppleDeviceCommand::Import(args) => apple::device::import_devices(app, args),
                AppleDeviceCommand::Remove(args) => apple::device::remove_device(app, args),
            },
            AppleCommand::Signing { command } => match command {
                AppleSigningCommand::Export(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple::signing::export_signing_credentials(&project, args)
                }
                AppleSigningCommand::Import(args) => {
                    let project = app.load_project(cli.manifest.as_deref())?;
                    apple::signing::import_signing_credentials(&project, args)
                }
            },
        },
    }
}
