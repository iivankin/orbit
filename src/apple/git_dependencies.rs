use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use semver::Version;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::context::AppContext;
use crate::util::{command_output, ensure_dir, run_command};

pub(crate) fn latest_remote_revision(url: &str) -> Result<String> {
    let output = command_output(Command::new("git").args(["ls-remote", url, "HEAD"]))?;
    let revision = output
        .split_whitespace()
        .next()
        .context("`git ls-remote` did not return a HEAD revision")?;
    Ok(revision.to_owned())
}

pub(crate) fn exact_remote_version_revision(url: &str, version: &str) -> Result<String> {
    let target = Version::parse(version)
        .with_context(|| format!("invalid exact semver version `{version}`"))?;
    let output = command_output(Command::new("git").args(["ls-remote", "--tags", url]))?;
    for (tag_name, revision) in remote_tag_revisions(&output) {
        let normalized = tag_name.strip_prefix('v').unwrap_or(&tag_name);
        let Ok(candidate) = Version::parse(normalized) else {
            continue;
        };
        if candidate == target {
            return Ok(revision);
        }
    }
    anyhow::bail!("no remote tag on `{url}` matched exact version `{version}`")
}

pub(crate) fn latest_remote_version_revision(
    url: &str,
    current_version: &str,
) -> Result<(String, String)> {
    let current_version = Version::parse(current_version)
        .with_context(|| format!("invalid exact semver version `{current_version}`"))?;
    let output = command_output(Command::new("git").args(["ls-remote", "--tags", url]))?;
    let candidates = remote_tag_revisions(&output);

    let mut best: Option<(Version, String, String)> = None;
    for (tag_name, revision) in candidates {
        let normalized = tag_name.strip_prefix('v').unwrap_or(&tag_name);
        let Ok(version) = Version::parse(normalized) else {
            continue;
        };
        if version.major != current_version.major {
            continue;
        }
        let should_replace = best
            .as_ref()
            .map_or(true, |(current_version, _, _)| version > *current_version);
        if should_replace {
            best = Some((version, tag_name, revision));
        }
    }

    let (version, _, revision) = best.with_context(|| {
        format!(
            "no remote semver tags on `{url}` matched major version {}",
            current_version.major
        )
    })?;
    Ok((version.to_string(), revision))
}

pub(crate) fn materialize_git_dependency(
    app: &AppContext,
    url: &str,
    revision: &str,
) -> Result<PathBuf> {
    let checkout_root = git_dependency_checkout_root(app, url, revision);
    let checkout_dir = checkout_root.join("checkout");
    if git_checkout_matches_revision(&checkout_dir, revision)? {
        return Ok(checkout_dir);
    }

    if checkout_root.exists() {
        fs::remove_dir_all(&checkout_root)
            .with_context(|| format!("failed to reset {}", checkout_root.display()))?;
    }
    ensure_dir(
        checkout_root
            .parent()
            .context("git dependency checkout root did not have a parent directory")?,
    )?;

    let temp_root = checkout_root.with_file_name(format!(
        "{}-tmp-{}",
        checkout_root_name(url, revision),
        Uuid::new_v4()
    ));
    let temp_checkout = temp_root.join("checkout");
    ensure_dir(&temp_root)?;

    let clone_result = (|| -> Result<()> {
        run_command(
            Command::new("git")
                .arg("clone")
                .arg("--recursive")
                .arg(url)
                .arg(&temp_checkout),
        )?;
        run_command(
            Command::new("git")
                .args(["-C"])
                .arg(&temp_checkout)
                .args(["checkout", "--force", revision]),
        )?;
        Ok(())
    })();

    if clone_result.is_err() && temp_root.exists() {
        let _ = fs::remove_dir_all(&temp_root);
    }
    clone_result?;
    fs::rename(&temp_root, &checkout_root).with_context(|| {
        format!(
            "failed to move git dependency checkout into {}",
            checkout_root.display()
        )
    })?;
    Ok(checkout_dir)
}

fn git_checkout_matches_revision(checkout_dir: &Path, expected_revision: &str) -> Result<bool> {
    if !checkout_dir.join(".git").exists() {
        return Ok(false);
    }
    let head = match command_output(
        Command::new("git")
            .args(["-C"])
            .arg(checkout_dir)
            .args(["rev-parse", "HEAD"]),
    ) {
        Ok(head) => head,
        Err(_) => return Ok(false),
    };
    Ok(head.trim() == expected_revision)
}

fn git_dependency_checkout_root(app: &AppContext, url: &str, revision: &str) -> PathBuf {
    app.global_paths
        .cache_dir
        .join("git-swift-packages")
        .join(checkout_root_name(url, revision))
}

fn remote_tag_revisions(output: &str) -> Vec<(String, String)> {
    let mut direct = std::collections::BTreeMap::new();
    let mut peeled = std::collections::BTreeMap::new();

    for line in output.lines() {
        let mut parts = line.split_whitespace();
        let Some(revision) = parts.next() else {
            continue;
        };
        let Some(reference) = parts.next() else {
            continue;
        };
        let Some(tag_name) = reference.strip_prefix("refs/tags/") else {
            continue;
        };
        if let Some(base_name) = tag_name.strip_suffix("^{}") {
            peeled.insert(base_name.to_owned(), revision.to_owned());
        } else {
            direct.insert(tag_name.to_owned(), revision.to_owned());
        }
    }

    let mut resolved = Vec::new();
    for (tag_name, revision) in direct {
        resolved.push((
            tag_name.clone(),
            peeled.get(&tag_name).cloned().unwrap_or(revision),
        ));
    }
    for (tag_name, revision) in peeled {
        if resolved.iter().any(|(existing, _)| existing == &tag_name) {
            continue;
        }
        resolved.push((tag_name, revision));
    }
    resolved
}

fn checkout_root_name(url: &str, revision: &str) -> String {
    format!(
        "{}-{}",
        repo_slug(url),
        short_hash(&format!("{url}\n{revision}"))
    )
}

fn repo_slug(url: &str) -> String {
    let candidate = url
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("package")
        .trim_end_matches(".git");
    let slug = candidate
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if slug.is_empty() {
        "package".to_owned()
    } else {
        slug
    }
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
