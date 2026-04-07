use std::fs;

use anyhow::Result;

use crate::apple::signing::{clean_local_signing_state, clean_remote_signing_state};
use crate::cli::CleanArgs;
use crate::context::ProjectContext;
use crate::util::prompt_confirm;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum CleanupPlan {
    Noop,
    LocalOnly,
    AppleOnly,
    LocalAndApple,
}

impl CleanupPlan {
    fn cleans_local_state(self) -> bool {
        matches!(self, Self::LocalOnly | Self::LocalAndApple)
    }

    fn cleans_apple(self) -> bool {
        matches!(self, Self::AppleOnly | Self::LocalAndApple)
    }

    fn without_apple_cleanup(self) -> Self {
        match self {
            Self::LocalAndApple => Self::LocalOnly,
            Self::AppleOnly => Self::Noop,
            Self::LocalOnly | Self::Noop => self,
        }
    }
}

pub fn clean_project(project: &ProjectContext, args: &CleanArgs) -> Result<()> {
    let mut plan = cleanup_plan(args);

    if plan.cleans_apple()
        && project.app.interactive
        && !prompt_confirm(
            "Delete Orbit-managed Apple Developer resources for this project?",
            false,
        )?
    {
        println!("skipped Apple Developer cleanup");
        plan = plan.without_apple_cleanup();
    }

    // Remote cleanup needs the pre-clean signing state to know which
    // Orbit-managed profiles and identifiers belong to this project.
    let remote_summary = plan
        .cleans_apple()
        .then(|| clean_remote_signing_state(project))
        .transpose()?;

    if plan.cleans_local_state() && project.project_paths.orbit_dir.exists() {
        fs::remove_dir_all(&project.project_paths.orbit_dir)?;
        println!(
            "removed_local_orbit_dir: {}",
            project.project_paths.orbit_dir.display()
        );
    }

    if plan.cleans_local_state() {
        let summary = clean_local_signing_state(project)?;
        println!("removed_local_profiles: {}", summary.removed_profiles);
        println!(
            "removed_local_certificates: {}",
            summary.removed_certificates
        );
    }

    if let Some(summary) = remote_summary {
        println!("removed_remote_profiles: {}", summary.removed_profiles);
        println!("removed_remote_apps: {}", summary.removed_apps);
        println!("removed_remote_app_groups: {}", summary.removed_app_groups);
        println!("removed_remote_merchants: {}", summary.removed_merchants);
        println!(
            "removed_remote_cloud_containers: {}",
            summary.removed_cloud_containers
        );
    }

    Ok(())
}

fn cleanup_plan(args: &CleanArgs) -> CleanupPlan {
    if args.all || (args.local && args.apple) {
        CleanupPlan::LocalAndApple
    } else if args.apple {
        CleanupPlan::AppleOnly
    } else {
        CleanupPlan::LocalOnly
    }
}

#[cfg(test)]
mod tests {
    use super::{CleanupPlan, cleanup_plan};
    use crate::cli::CleanArgs;

    fn args(local: bool, apple: bool, all: bool) -> CleanArgs {
        CleanArgs { local, apple, all }
    }

    #[test]
    fn apple_cleanup_does_not_imply_local_cleanup() {
        assert_eq!(
            cleanup_plan(&args(false, true, false)),
            CleanupPlan::AppleOnly
        );
    }

    #[test]
    fn declining_apple_cleanup_still_keeps_local_cleanup_for_all() {
        assert_eq!(
            cleanup_plan(&args(false, false, true)).without_apple_cleanup(),
            CleanupPlan::LocalOnly
        );
    }

    #[test]
    fn declining_apple_cleanup_skips_apple_only_cleanup() {
        assert_eq!(
            cleanup_plan(&args(false, true, false)).without_apple_cleanup(),
            CleanupPlan::Noop
        );
    }
}
