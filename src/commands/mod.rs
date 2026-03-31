pub mod apple;
pub mod init;

use anyhow::Result;

use crate::cli::{AppleCommand, AppleDeviceCommand, Cli, Command, DepsCommand, IdeCommand};
use crate::context::AppContext;
use crate::manifest::{ManifestBackend, detect_schema};

pub fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(_) => init::execute(app, cli.manifest.as_deref()),
        Command::Lint(_) | Command::Format(_) | Command::Bsp(_) => {
            dispatch_project_command(app, cli)
        }
        Command::Deps(deps_args) => match &deps_args.command {
            DepsCommand::Lock(_) => dispatch_project_command(app, cli),
            DepsCommand::Update(_) => dispatch_project_command(app, cli),
        },
        Command::Ide(ide_args) => match &ide_args.command {
            IdeCommand::InstallBuildServer(_) | IdeCommand::DumpArgs(_) => {
                dispatch_project_command(app, cli)
            }
        },
        Command::Apple(apple_args) => match &apple_args.command {
            AppleCommand::Device { command } => match command {
                AppleDeviceCommand::List(_)
                | AppleDeviceCommand::Register(_)
                | AppleDeviceCommand::Import(_)
                | AppleDeviceCommand::Remove(_) => apple::execute(app, cli),
            },
            AppleCommand::Signing { .. } => dispatch_project_command(app, cli),
        },
        Command::Run(_) | Command::Build(_) | Command::Submit(_) | Command::Clean(_) => {
            dispatch_project_command(app, cli)
        }
    }
}

fn dispatch_project_command(app: &AppContext, cli: &Cli) -> Result<()> {
    let manifest_path = app.resolve_manifest_path_for_dispatch(cli.manifest.as_deref())?;
    match detect_schema(&manifest_path)?.backend() {
        ManifestBackend::Apple => apple::execute(app, cli),
        ManifestBackend::Android => {
            unreachable!("unsupported backend is rejected by detect_schema")
        }
    }
}
