pub mod external;
pub mod pipeline;
pub mod receipt;
pub mod toolchain;

use anyhow::Result;

use crate::cli::{BuildArgs, RunArgs, SubmitArgs};
use crate::context::ProjectContext;

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    pipeline::run_on_destination(project, args)
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    pipeline::build_artifact(project, args)
}

pub fn submit_artifact(project: &ProjectContext, args: &SubmitArgs) -> Result<()> {
    pipeline::submit_artifact(project, args)
}
