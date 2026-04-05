use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn create_p12(identity_dir: &Path, password: &str) -> PathBuf {
    fs::create_dir_all(identity_dir).unwrap();
    let key_path = identity_dir.join("key.pem");
    let cert_path = identity_dir.join("cert.pem");
    let p12_path = identity_dir.join("signing.p12");
    assert!(
        Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                key_path.to_str().unwrap(),
                "-out",
                cert_path.to_str().unwrap(),
                "-subj",
                "/CN=Orbit Test",
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("openssl")
            .args([
                "pkcs12",
                "-export",
                "-inkey",
                key_path.to_str().unwrap(),
                "-in",
                cert_path.to_str().unwrap(),
                "-out",
                p12_path.to_str().unwrap(),
                "-passout",
                &format!("pass:{password}"),
            ])
            .status()
            .unwrap()
            .success()
    );
    p12_path
}

pub fn create_api_key(path: &Path) {
    assert!(
        Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-out",
                path.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
}
