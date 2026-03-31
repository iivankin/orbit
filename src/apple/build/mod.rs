pub mod clang;
pub mod default_icon;
pub mod external;
pub mod pipeline;
pub mod receipt;
pub mod swiftc;
pub mod toolchain;
pub mod verify;

use anyhow::Result;

use self::toolchain::DestinationKind;
use crate::cli::{BuildArgs, RunArgs};
use crate::context::ProjectContext;
use crate::manifest::ApplePlatform;
use std::path::Path;

pub fn run_on_destination(project: &ProjectContext, args: &RunArgs) -> Result<()> {
    pipeline::run_on_destination(project, args)
}

pub fn build_artifact(project: &ProjectContext, args: &BuildArgs) -> Result<()> {
    pipeline::build_artifact(project, args)
}

pub fn build_for_testing_destination(
    project: &ProjectContext,
    platform: ApplePlatform,
    destination: DestinationKind,
) -> Result<pipeline::BuildOutcome> {
    pipeline::build_for_testing_destination(project, platform, destination)
}

pub fn prepare_for_ide(
    project: &ProjectContext,
    platform: ApplePlatform,
    target_names: &[String],
    destination: DestinationKind,
    index_store_path: &Path,
) -> Result<()> {
    pipeline::prepare_for_ide(
        project,
        platform,
        target_names,
        destination,
        index_store_path,
    )
}
