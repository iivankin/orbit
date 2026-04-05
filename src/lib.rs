pub mod apple;
pub mod cli;
pub mod commands;
pub mod context;
pub mod manifest;
pub mod util;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;
use crate::context::AppContext;

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let app = AppContext::new(cli.non_interactive, cli.verbose)?;
    commands::execute(&app, &cli)
}
