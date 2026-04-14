pub mod apple;
pub mod init;

use anyhow::Result;

use crate::cli::{AppleCommand, Cli, Command, UiCommand};
use crate::context::AppContext;
use crate::manifest::{ManifestBackend, detect_schema_with_env};

pub fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(_) => init::execute(app, cli.manifest.as_deref()),
        Command::InspectTrace(args) => crate::apple::profile::inspect_trace_command(app, args),
        Command::Ui(ui_args)
            if matches!(
                &ui_args.command,
                UiCommand::Schema(_) | UiCommand::ResetIdb(_)
            ) =>
        {
            apple::execute(app, cli)
        }
        Command::Apple(apple_args)
            if matches!(&apple_args.command, AppleCommand::Device { .. }) =>
        {
            apple::execute(app, cli)
        }
        _ => dispatch_project_command(app, cli),
    }
}

fn dispatch_project_command(app: &AppContext, cli: &Cli) -> Result<()> {
    let manifest_path = app.resolve_manifest_path_for_dispatch(cli.manifest.as_deref())?;
    match detect_schema_with_env(&manifest_path, app.manifest_env())?.backend() {
        ManifestBackend::Apple => apple::execute(app, cli),
        ManifestBackend::Android => {
            unreachable!("unsupported backend is rejected by detect_schema")
        }
    }
}
