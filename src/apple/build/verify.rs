use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::apple::build::receipt::BuildReceipt;
use crate::manifest::{ApplePlatform, DistributionKind};
use crate::util::{combine_command_output, command_output_allow_failure};

pub(crate) fn should_verify_developer_id_artifact(receipt: &BuildReceipt) -> bool {
    receipt.platform == ApplePlatform::Macos
        && receipt.distribution == DistributionKind::DeveloperId
}

pub(crate) fn verify_post_build(receipt: &BuildReceipt) -> Result<String> {
    let verifier = DeveloperIdArtifactVerifier::new(receipt)?;
    verifier.verify_codesign()?;
    verifier.verify_pkg_signature()?;
    let gatekeeper = verifier.verify_gatekeeper_before_notarization()?;
    Ok(format!(
        "Verified Developer ID signing for `{}`; Gatekeeper reports `{gatekeeper}` before notarization.",
        receipt.artifact_path.display()
    ))
}

pub(crate) fn verify_post_notarization(receipt: &BuildReceipt) -> Result<String> {
    let verifier = DeveloperIdArtifactVerifier::new(receipt)?;
    verifier.verify_codesign()?;
    verifier.verify_pkg_signature()?;
    verifier.verify_stapled_ticket()?;
    verifier.verify_gatekeeper_after_notarization()?;
    Ok(format!(
        "Verified notarized Developer ID package `{}` with stapled ticket.",
        receipt.artifact_path.display()
    ))
}

struct DeveloperIdArtifactVerifier<'a> {
    receipt: &'a BuildReceipt,
}

impl<'a> DeveloperIdArtifactVerifier<'a> {
    fn new(receipt: &'a BuildReceipt) -> Result<Self> {
        if !should_verify_developer_id_artifact(receipt) {
            bail!(
                "Developer ID verification only supports macOS Developer ID receipts, got `{}`",
                receipt.artifact_path.display()
            );
        }
        if !receipt.bundle_path.exists() {
            bail!(
                "bundle path `{}` does not exist",
                receipt.bundle_path.display()
            );
        }
        if !receipt.artifact_path.exists() {
            bail!(
                "artifact path `{}` does not exist",
                receipt.artifact_path.display()
            );
        }
        Ok(Self { receipt })
    }

    fn verify_codesign(&self) -> Result<()> {
        let (success, stdout, stderr) = run_capture("codesign", |command| {
            command.args(["-dv", "--verbose=4"]);
            command.arg(&self.receipt.bundle_path);
        })?;
        if !success {
            return Err(command_failure(
                "codesign developer-id verification",
                &self.receipt.bundle_path,
                &stdout,
                &stderr,
            ));
        }
        let output = combine_command_output(&stdout, &stderr);
        if !output.contains("Developer ID Application:") {
            bail!(
                "codesign verification for `{}` did not report a Developer ID Application identity\n{}",
                self.receipt.bundle_path.display(),
                output
            );
        }
        if !output.contains("(runtime)") {
            bail!(
                "codesign verification for `{}` did not report hardened runtime\n{}",
                self.receipt.bundle_path.display(),
                output
            );
        }
        Ok(())
    }

    fn verify_pkg_signature(&self) -> Result<()> {
        let (success, stdout, stderr) = run_capture("pkgutil", |command| {
            command.args(["--check-signature"]);
            command.arg(&self.receipt.artifact_path);
        })?;
        if !success {
            return Err(command_failure(
                "pkg signature verification",
                &self.receipt.artifact_path,
                &stdout,
                &stderr,
            ));
        }
        let output = combine_command_output(&stdout, &stderr);
        if !output.contains("Developer ID Installer:") {
            bail!(
                "pkgutil verification for `{}` did not report a Developer ID Installer identity\n{}",
                self.receipt.artifact_path.display(),
                output
            );
        }
        Ok(())
    }

    fn verify_gatekeeper_before_notarization(&self) -> Result<&'static str> {
        let (success, stdout, stderr) = run_capture("spctl", |command| {
            command.args(["-a", "-vvv", "--type", "install"]);
            command.arg(&self.receipt.artifact_path);
        })?;
        classify_pre_notary_gatekeeper_result(&stdout, &stderr, success)
    }

    fn verify_stapled_ticket(&self) -> Result<()> {
        let (success, stdout, stderr) = run_capture("xcrun", |command| {
            command.arg("stapler");
            command.arg("validate");
            command.arg(&self.receipt.artifact_path);
        })?;
        if !success {
            return Err(command_failure(
                "stapler validation",
                &self.receipt.artifact_path,
                &stdout,
                &stderr,
            ));
        }
        Ok(())
    }

    fn verify_gatekeeper_after_notarization(&self) -> Result<()> {
        let (success, stdout, stderr) = run_capture("spctl", |command| {
            command.args(["-a", "-vvv", "--type", "install"]);
            command.arg(&self.receipt.artifact_path);
        })?;
        if !success {
            return Err(command_failure(
                "Gatekeeper notarization validation",
                &self.receipt.artifact_path,
                &stdout,
                &stderr,
            ));
        }
        Ok(())
    }
}

fn classify_pre_notary_gatekeeper_result(
    stdout: &str,
    stderr: &str,
    success: bool,
) -> Result<&'static str> {
    if success {
        return Ok("accepted");
    }
    let output = combine_command_output(stdout, stderr);
    if output.contains("Unnotarized Developer ID") {
        return Ok("unnotarized-developer-id");
    }
    bail!(
        "Gatekeeper rejected the package for an unexpected reason before notarization\n{}",
        output
    );
}

fn run_capture<F>(program: &str, configure: F) -> Result<(bool, String, String)>
where
    F: FnOnce(&mut Command),
{
    let mut command = Command::new(program);
    configure(&mut command);
    command_output_allow_failure(&mut command)
        .with_context(|| format!("failed to run `{program}` during Developer ID verification"))
}

fn command_failure(step: &str, path: &Path, stdout: &str, stderr: &str) -> anyhow::Error {
    let output = combine_command_output(stdout, stderr);
    if output.is_empty() {
        return anyhow::anyhow!(
            "{step} for `{}` failed without stdout/stderr",
            path.display()
        );
    }
    anyhow::anyhow!("{step} for `{}` failed\n{}", path.display(), output)
}

#[cfg(test)]
mod tests {
    use super::classify_pre_notary_gatekeeper_result;

    #[test]
    fn accepts_expected_unnotarized_gatekeeper_rejection() {
        let status = classify_pre_notary_gatekeeper_result(
            "",
            "pkg: rejected\nsource=Unnotarized Developer ID",
            false,
        )
        .unwrap();
        assert_eq!(status, "unnotarized-developer-id");
    }

    #[test]
    fn accepts_gatekeeper_success_before_notarization_probe() {
        let status = classify_pre_notary_gatekeeper_result(
            "pkg: accepted\nsource=Notarized Developer ID",
            "",
            true,
        )
        .unwrap();
        assert_eq!(status, "accepted");
    }

    #[test]
    fn rejects_unexpected_gatekeeper_failure() {
        let error = classify_pre_notary_gatekeeper_result(
            "",
            "pkg: rejected\nsource=no usable signature",
            false,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unexpected reason before notarization")
        );
    }
}
