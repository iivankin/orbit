use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use super::P12_PASSWORD_SERVICE;
use crate::context::ProjectContext;
use crate::util::{read_json_file_if_exists, run_command, run_command_capture, write_json_file};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct SigningState {
    pub(super) certificates: Vec<ManagedCertificate>,
    pub(super) profiles: Vec<ManagedProfile>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CertificateOrigin {
    #[default]
    Generated,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ManagedCertificate {
    pub(super) id: String,
    pub(super) certificate_type: String,
    pub(super) serial_number: String,
    #[serde(default)]
    pub(super) origin: CertificateOrigin,
    pub(super) display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) system_keychain_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) system_signing_identity: Option<String>,
    pub(super) private_key_path: PathBuf,
    pub(super) certificate_der_path: PathBuf,
    pub(super) p12_path: PathBuf,
    pub(super) p12_password_account: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ManagedProfile {
    pub(super) id: String,
    pub(super) profile_type: String,
    pub(super) bundle_id: String,
    pub(super) path: PathBuf,
    pub(super) uuid: Option<String>,
    pub(super) certificate_ids: Vec<String>,
    pub(super) device_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct SigningIdentity {
    pub(super) hash: String,
    pub(super) keychain_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct TeamSigningPaths {
    pub(super) state_path: PathBuf,
    pub(super) certificates_dir: PathBuf,
    pub(super) profiles_dir: PathBuf,
}

pub(super) fn certificate_has_local_signing_material(certificate: &ManagedCertificate) -> bool {
    certificate.system_signing_identity.is_some() || certificate.p12_path.exists()
}

pub(super) fn read_certificate_serial(path: &Path) -> Result<String> {
    read_certificate_serial_with_format(path, Some("DER"))
}

fn read_certificate_serial_pem(path: &Path) -> Result<String> {
    read_certificate_serial_with_format(path, None)
}

fn read_certificate_serial_with_format(path: &Path, inform: Option<&str>) -> Result<String> {
    let mut command = Command::new("openssl");
    command.arg("x509");
    if let Some(inform) = inform {
        command.args(["-inform", inform]);
    }
    command.args([
        "-in",
        path.to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-noout",
        "-serial",
    ]);
    let output = crate::util::command_output(&mut command)?;
    output
        .trim()
        .strip_prefix("serial=")
        .map(ToOwned::to_owned)
        .context("openssl did not return a certificate serial number")
}

pub(super) fn team_signing_paths(project: &ProjectContext, team_id: &str) -> TeamSigningPaths {
    let team_dir = project
        .app
        .global_paths
        .data_dir
        .join("teams")
        .join(team_id);
    TeamSigningPaths {
        state_path: team_dir.join("signing.json"),
        certificates_dir: team_dir.join("certificates"),
        profiles_dir: team_dir.join("profiles"),
    }
}

pub(super) fn load_state(project: &ProjectContext, team_id: &str) -> Result<SigningState> {
    let paths = team_signing_paths(project, team_id);
    Ok(read_json_file_if_exists(&paths.state_path)?.unwrap_or_default())
}

pub(super) fn save_state(
    project: &ProjectContext,
    team_id: &str,
    state: &SigningState,
) -> Result<()> {
    let paths = team_signing_paths(project, team_id);
    write_json_file(&paths.state_path, state)
}

pub(super) fn store_p12_password(account: &str, password: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "add-generic-password",
        "-U",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
        "-w",
        password,
    ]);
    crate::util::command_output(&mut command).map(|_| ())
}

pub(super) fn load_p12_password(account: &str) -> Result<String> {
    let mut command = Command::new("security");
    command.args([
        "find-generic-password",
        "-w",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
    ]);
    crate::util::command_output(&mut command).map(|value| value.trim().to_owned())
}

pub(super) fn delete_p12_password(account: &str) -> Result<()> {
    let mut command = Command::new("security");
    command.args([
        "delete-generic-password",
        "-a",
        account,
        "-s",
        P12_PASSWORD_SERVICE,
    ]);
    crate::util::command_output(&mut command).map(|_| ())
}

pub(super) fn extract_private_key_from_p12(
    p12_path: &Path,
    output_path: &Path,
    password: &str,
) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "pkcs12",
        "-in",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-nodes",
        "-nocerts",
        "-out",
        output_path
            .to_str()
            .context("private key output path contains invalid UTF-8")?,
        "-passin",
        &format!("pass:{password}"),
    ]);
    run_command(&mut command)
}

pub(super) fn extract_certificate_from_p12(
    p12_path: &Path,
    output_path: &Path,
    password: &str,
) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "pkcs12",
        "-in",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-clcerts",
        "-nokeys",
        "-out",
        output_path
            .to_str()
            .context("certificate output path contains invalid UTF-8")?,
        "-passin",
        &format!("pass:{password}"),
    ]);
    run_command(&mut command)
}

pub(super) fn export_certificate_der(
    certificate_pem_path: &Path,
    output_path: &Path,
) -> Result<()> {
    let mut command = Command::new("openssl");
    command.args([
        "x509",
        "-in",
        certificate_pem_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-outform",
        "DER",
        "-out",
        output_path
            .to_str()
            .context("certificate output path contains invalid UTF-8")?,
    ]);
    run_command(&mut command)
}

pub(super) fn export_p12_from_der_certificate(
    private_key_path: &Path,
    certificate_der_path: &Path,
    output_path: &Path,
    password: &str,
) -> Result<()> {
    let certificate_pem = NamedTempFile::new()?;

    let mut decode = Command::new("openssl");
    decode.args([
        "x509",
        "-inform",
        "DER",
        "-in",
        certificate_der_path
            .to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-out",
        certificate_pem
            .path()
            .to_str()
            .context("temporary certificate path contains invalid UTF-8")?,
    ]);
    run_command(&mut decode)?;

    let private_key = private_key_path
        .to_str()
        .context("private key path contains invalid UTF-8")?;
    let certificate_pem = certificate_pem
        .path()
        .to_str()
        .context("temporary certificate path contains invalid UTF-8")?;
    let output_path = output_path
        .to_str()
        .context("P12 path contains invalid UTF-8")?;

    let mut export = Command::new("openssl");
    export.args([
        "pkcs12",
        "-legacy",
        "-export",
        "-inkey",
        private_key,
        "-in",
        certificate_pem,
        "-out",
        output_path,
        "-passout",
        &format!("pass:{password}"),
    ]);
    let debug = crate::util::debug_command(&export);
    let output = export
        .output()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    if output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if pkcs12_legacy_flag_is_unsupported(&stdout, &stderr) {
        // LibreSSL on macOS still emits the older PKCS#12 settings by default, but it does not
        // understand OpenSSL's `-legacy` flag. Retry without the flag instead of failing CI.
        let mut export = Command::new("openssl");
        export.args([
            "pkcs12",
            "-export",
            "-inkey",
            private_key,
            "-in",
            certificate_pem,
            "-out",
            output_path,
            "-passout",
            &format!("pass:{password}"),
        ]);
        return run_command(&mut export);
    }

    bail!(
        "`{debug}` failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr
    )
}

pub(super) fn recover_orphaned_certificate(
    paths: &TeamSigningPaths,
    state: &mut SigningState,
    certificate_type: &str,
    remote_id: &str,
    serial_number: &str,
    display_name: Option<&str>,
) -> Result<Option<ManagedCertificate>> {
    for entry in fs::read_dir(&paths.certificates_dir)
        .with_context(|| format!("failed to read {}", paths.certificates_dir.display()))?
    {
        let entry = entry?;
        let certificate_der_path = entry.path();
        if certificate_der_path
            .extension()
            .and_then(|value| value.to_str())
            != Some("cer")
        {
            continue;
        }

        let local_serial_number = read_certificate_serial(&certificate_der_path)?;
        if !local_serial_number.eq_ignore_ascii_case(serial_number) {
            continue;
        }

        let Some(stem) = certificate_der_path
            .file_stem()
            .and_then(|value| value.to_str())
        else {
            continue;
        };
        let private_key_path = paths.certificates_dir.join(format!("{stem}.key.pem"));
        if !private_key_path.exists() {
            continue;
        }

        let p12_path = paths.certificates_dir.join(format!("{stem}.p12"));
        let p12_password = uuid::Uuid::new_v4().to_string();
        export_p12_from_der_certificate(
            &private_key_path,
            &certificate_der_path,
            &p12_path,
            &p12_password,
        )?;

        let password_account = format!("{remote_id}-{serial_number}");
        store_p12_password(&password_account, &p12_password)?;

        state.certificates.retain(|candidate| {
            let matches_remote = candidate.id == remote_id
                || candidate.serial_number.eq_ignore_ascii_case(serial_number);
            if matches_remote {
                let _ = delete_p12_password(&candidate.p12_password_account);
            }
            !matches_remote
        });

        let certificate = ManagedCertificate {
            id: remote_id.to_owned(),
            certificate_type: certificate_type.to_owned(),
            serial_number: serial_number.to_owned(),
            origin: CertificateOrigin::Generated,
            display_name: display_name.map(ToOwned::to_owned),
            system_keychain_path: None,
            system_signing_identity: None,
            private_key_path,
            certificate_der_path,
            p12_path,
            p12_password_account: password_account,
        };
        state.certificates.push(certificate.clone());
        return Ok(Some(certificate));
    }

    Ok(None)
}

pub(super) fn read_certificate_common_name(path: &Path) -> Result<Option<String>> {
    read_certificate_common_name_with_format(path, None)
}

pub(super) fn read_der_certificate_common_name(path: &Path) -> Result<Option<String>> {
    read_certificate_common_name_with_format(path, Some("DER"))
}

fn read_certificate_common_name_with_format(
    path: &Path,
    inform: Option<&str>,
) -> Result<Option<String>> {
    let mut command = Command::new("openssl");
    command.arg("x509");
    if let Some(inform) = inform {
        command.args(["-inform", inform]);
    }
    command.args([
        "-in",
        path.to_str()
            .context("certificate path contains invalid UTF-8")?,
        "-noout",
        "-subject",
    ]);
    let output = crate::util::command_output(&mut command)?;
    Ok(parse_certificate_common_name(output.trim()))
}

pub(super) fn parse_certificate_common_name(subject: &str) -> Option<String> {
    subject
        .split(',')
        .find_map(|segment| {
            let segment = segment.trim();
            segment
                .strip_prefix("subject=")
                .unwrap_or(segment)
                .trim()
                .strip_prefix("CN = ")
                .or_else(|| segment.trim().strip_prefix("CN="))
                .map(ToOwned::to_owned)
        })
        .filter(|value| !value.is_empty())
}

pub(super) fn parse_codesigning_identity_line(line: &str) -> Option<(String, String)> {
    let quote_start = line.find('"')?;
    let quote_end = line[quote_start + 1..].find('"')?;
    let name = line[quote_start + 1..quote_start + 1 + quote_end].to_owned();
    let hash = line.split_whitespace().nth(1)?.trim_matches('"').to_owned();
    Some((hash, name))
}

fn keychain_identities(keychain_path: &str, policy: &str) -> Result<Vec<(String, String)>> {
    let mut find_identity = Command::new("security");
    find_identity.args(["find-identity", "-v", "-p", policy, keychain_path]);
    let output = crate::util::command_output(&mut find_identity)?;
    Ok(output
        .lines()
        .filter_map(parse_codesigning_identity_line)
        .collect())
}

fn user_keychain_paths() -> Result<Vec<PathBuf>> {
    let mut command = Command::new("security");
    command.args(["list-keychains", "-d", "user"]);
    let output = crate::util::command_output(&mut command)?;
    let mut keychains = output
        .lines()
        .map(|line| PathBuf::from(line.trim().trim_matches('"')))
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    if keychains.is_empty() {
        keychains.push(PathBuf::from("login.keychain-db"));
    }
    Ok(keychains)
}

fn keychain_certificate_records(keychain_path: &str) -> Result<Vec<(String, String)>> {
    let mut command = Command::new("security");
    command.args(["find-certificate", "-a", "-Z", "-p", keychain_path]);
    let output = crate::util::command_output(&mut command)?;
    let mut records = Vec::new();
    let mut current_sha1 = None::<String>;
    let mut current_pem = Vec::new();
    let mut in_pem = false;
    for line in output.lines() {
        if let Some(hash) = line.strip_prefix("SHA-1 hash: ") {
            current_sha1 = Some(hash.trim().to_owned());
            continue;
        }
        if line == "-----BEGIN CERTIFICATE-----" {
            in_pem = true;
            current_pem.clear();
        }
        if in_pem {
            current_pem.push(line.to_owned());
            if line == "-----END CERTIFICATE-----" {
                if let Some(hash) = current_sha1.take() {
                    records.push((hash, current_pem.join("\n")));
                }
                current_pem.clear();
                in_pem = false;
            }
        }
    }
    Ok(records)
}

pub(super) fn recover_system_keychain_identity(
    serial_number: &str,
    display_name: Option<&str>,
) -> Result<Option<SigningIdentity>> {
    for keychain_path in user_keychain_paths()? {
        let keychain_str = keychain_path
            .to_str()
            .context("keychain path contains invalid UTF-8")?;
        let mut identities = HashMap::new();
        for policy in ["codesigning", "basic"] {
            for (hash, name) in keychain_identities(keychain_str, policy)? {
                identities.entry(hash).or_insert(name);
            }
        }
        if identities.is_empty() {
            continue;
        }

        for (hash, pem) in keychain_certificate_records(keychain_str)? {
            let Some(identity_name) = identities.get(&hash) else {
                continue;
            };
            let temp = NamedTempFile::new()?;
            fs::write(temp.path(), pem.as_bytes())
                .with_context(|| format!("failed to write {}", temp.path().display()))?;
            let local_serial = read_certificate_serial_pem(temp.path())?;
            if !local_serial.eq_ignore_ascii_case(serial_number) {
                continue;
            }
            if let Some(display_name) = display_name
                && !identity_name.contains(display_name)
            {
                let local_common_name = read_certificate_common_name(temp.path())?;
                if !local_common_name
                    .as_deref()
                    .is_some_and(|common_name| common_name.contains(display_name))
                {
                    continue;
                }
            }
            return Ok(Some(SigningIdentity {
                hash,
                keychain_path: keychain_path.clone(),
            }));
        }
    }
    Ok(None)
}

pub(super) fn delete_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove {}", path.display()))?;
    }
    Ok(())
}

pub(super) fn delete_certificate_files(certificate: &ManagedCertificate) -> Result<()> {
    delete_file_if_exists(&certificate.private_key_path)?;
    delete_file_if_exists(&certificate.certificate_der_path)?;
    delete_file_if_exists(&certificate.p12_path)
}

fn import_p12_into_keychain(p12_path: &Path, keychain_path: &str, password: &str) -> Result<()> {
    let mut import = Command::new("security");
    import.args([
        "import",
        p12_path
            .to_str()
            .context("P12 path contains invalid UTF-8")?,
        "-k",
        keychain_path,
        "-P",
        password,
        "-T",
        "/usr/bin/codesign",
        "-T",
        "/usr/bin/productbuild",
        "-T",
        "/usr/bin/productsign",
        "-T",
        "/usr/bin/security",
    ]);
    run_command_capture(&mut import).map(|_| ())
}

fn ensure_keychain_in_search_list(keychain_path: &str) -> Result<()> {
    let mut list = Command::new("security");
    list.args(["list-keychains", "-d", "user"]);
    let output = crate::util::command_output(&mut list)?;
    let existing = output
        .lines()
        .map(|line| line.trim().trim_matches('"'))
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if existing.iter().any(|candidate| candidate == keychain_path) {
        return Ok(());
    }

    let mut update = Command::new("security");
    update.args(["list-keychains", "-d", "user", "-s", keychain_path]);
    for candidate in &existing {
        update.arg(candidate);
    }
    run_command(&mut update)
}

pub(super) fn resolve_signing_identity(
    project: &ProjectContext,
    certificate: &ManagedCertificate,
) -> Result<SigningIdentity> {
    if let (Some(hash), Some(keychain_path)) = (
        certificate.system_signing_identity.as_ref(),
        certificate.system_keychain_path.as_ref(),
    ) {
        return Ok(SigningIdentity {
            hash: hash.clone(),
            keychain_path: keychain_path.clone(),
        });
    }

    let keychain_path = &project.app.global_paths.keychain_path;
    if !keychain_path.exists() {
        let mut create = Command::new("security");
        create.args([
            "create-keychain",
            "-p",
            "",
            keychain_path
                .to_str()
                .context("keychain path contains invalid UTF-8")?,
        ]);
        run_command(&mut create)?;
    }

    let keychain_str = keychain_path
        .to_str()
        .context("keychain path contains invalid UTF-8")?;
    let mut unlock = Command::new("security");
    unlock.args(["unlock-keychain", "-p", "", keychain_str]);
    let _ = run_command(&mut unlock);

    let mut settings = Command::new("security");
    settings.args(["set-keychain-settings", "-lut", "21600", keychain_str]);
    let _ = run_command(&mut settings);
    ensure_keychain_in_search_list(keychain_str)?;

    let p12_password = match load_p12_password(&certificate.p12_password_account) {
        Ok(password) => password,
        Err(error) => {
            if !can_repair_local_p12(certificate) {
                return Err(error);
            }
            repair_local_p12_password(certificate)?
        }
    };
    if let Err(error) = import_p12_into_keychain(&certificate.p12_path, keychain_str, &p12_password)
    {
        if !certificate.private_key_path.exists() || !certificate.certificate_der_path.exists() {
            return Err(error);
        }

        let repaired_password = uuid::Uuid::new_v4().to_string();
        // Re-export with macOS-compatible PKCS#12 settings so `security import` can read it.
        export_p12_from_der_certificate(
            &certificate.private_key_path,
            &certificate.certificate_der_path,
            &certificate.p12_path,
            &repaired_password,
        )
        .context("failed to repair local P12 for codesigning import")?;
        store_p12_password(&certificate.p12_password_account, &repaired_password)?;
        import_p12_into_keychain(&certificate.p12_path, keychain_str, &repaired_password)
            .context("failed to import repaired codesigning certificate into Orbit keychain")?;
    }

    let mut partition = Command::new("security");
    partition.args([
        "set-key-partition-list",
        "-S",
        "apple-tool:,apple:",
        "-s",
        "-k",
        "",
        keychain_str,
    ]);
    let _ = crate::util::command_output_allow_failure(&mut partition);

    let expected_common_name = read_der_certificate_common_name(&certificate.certificate_der_path)?
        .or_else(|| certificate.display_name.clone());
    for policy in ["codesigning", "basic"] {
        let identities = keychain_identities(keychain_str, policy)?;
        if let Some(expected_common_name) = expected_common_name.as_ref()
            && let Some((hash, _)) = identities
                .iter()
                .find(|(_, name)| name == expected_common_name)
        {
            return Ok(SigningIdentity {
                hash: hash.clone(),
                keychain_path: keychain_path.clone(),
            });
        }

        if let [identity] = identities.as_slice() {
            return Ok(SigningIdentity {
                hash: identity.0.clone(),
                keychain_path: keychain_path.clone(),
            });
        }
    }

    bail!(
        "failed to resolve imported signing identity for certificate {}",
        certificate.id
    )
}

fn can_repair_local_p12(certificate: &ManagedCertificate) -> bool {
    !certificate.p12_password_account.is_empty()
        && certificate.private_key_path.exists()
        && certificate.certificate_der_path.exists()
}

fn repair_local_p12_password(certificate: &ManagedCertificate) -> Result<String> {
    let repaired_password = uuid::Uuid::new_v4().to_string();
    export_p12_from_der_certificate(
        &certificate.private_key_path,
        &certificate.certificate_der_path,
        &certificate.p12_path,
        &repaired_password,
    )
    .context("failed to repair local P12 after missing password lookup")?;
    store_p12_password(&certificate.p12_password_account, &repaired_password)?;
    Ok(repaired_password)
}

fn pkcs12_legacy_flag_is_unsupported(stdout: &str, stderr: &str) -> bool {
    let combined = crate::util::combine_command_output(stdout, stderr).to_ascii_lowercase();
    combined.contains("legacy")
        && (combined.contains("unknown option") || combined.contains("unrecognized flag"))
}

#[cfg(test)]
mod tests {
    use super::pkcs12_legacy_flag_is_unsupported;

    #[test]
    fn detects_libressl_rejecting_legacy_flag() {
        assert!(pkcs12_legacy_flag_is_unsupported(
            "",
            "pkcs12: Unrecognized flag legacy\npkcs12: Use -help for summary.\n"
        ));
    }

    #[test]
    fn detects_openssl_unknown_legacy_option() {
        assert!(pkcs12_legacy_flag_is_unsupported(
            "",
            "unknown option '-legacy'\nusage: pkcs12 ...\n"
        ));
    }

    #[test]
    fn ignores_other_pkcs12_failures() {
        assert!(!pkcs12_legacy_flag_is_unsupported(
            "",
            "pkcs12: Can't open input file missing.p12\n"
        ));
    }
}
