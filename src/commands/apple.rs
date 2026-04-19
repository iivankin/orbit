use anyhow::Result;

use crate::apple;
use crate::cli::{Cli, Command, DepsCommand, IdeCommand, PreviewCommand, UiCommand};
use crate::context::AppContext;

pub fn execute(app: &AppContext, cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init(_) => unreachable!("`init` is handled before Apple dispatch"),
        Command::InspectTrace(_) => {
            unreachable!("`inspect-trace` is handled before Apple dispatch")
        }
        Command::Lint(args) => apple::quality::lint_project(app, args, cli.manifest.as_deref()),
        Command::Format(args) => apple::quality::format_project(app, args, cli.manifest.as_deref()),
        Command::Test(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::testing::run_tests(&project, args)
        }
        Command::Preview(preview_args) => match &preview_args.command {
            PreviewCommand::List(args) => apple::preview::list(app, args, cli.manifest.as_deref()),
            PreviewCommand::Shot(args) => apple::preview::shot(app, args, cli.manifest.as_deref()),
        },
        Command::Ui(ui_args) => match &ui_args.command {
            UiCommand::Schema(args) => apple::ui::schema(args),
            UiCommand::CleanTraceTemp(args) => apple::ui::clean_trace_temp(args),
            UiCommand::ResetIdb(_) => apple::ui::reset_idb(),
            _ => {
                let project = app.load_project(cli.manifest.as_deref())?;
                apple::ui::execute(&project, &ui_args.command)
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
            crate::asc::submit_artifact(&project, args)
        }
        Command::Clean(args) => {
            let project = app.load_project(cli.manifest.as_deref())?;
            apple::clean::clean_project(&project, args)
        }
        Command::Asc(_) => unreachable!("`asc` is dispatched before Apple project commands"),
    }
}
