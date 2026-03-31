use anyhow::Result;

use crate::apple;
use crate::cli::{
    AppleCommand, AppleDeviceCommand, AppleSigningCommand, Cli, Command, DepsCommand, IdeCommand,
};
use crate::context::AppContext;

pub fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(_) => unreachable!("`init` is handled before Apple dispatch"),
        Command::Lint(args) => apple::quality::lint_project(app, args, cli.manifest.as_deref()),
        Command::Format(args) => apple::quality::format_project(app, args, cli.manifest.as_deref()),
        Command::Deps(deps_args) => match &deps_args.command {
            DepsCommand::Lock(_) => apple::deps::lock_dependencies(app, cli.manifest.as_deref()),
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
