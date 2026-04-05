use super::*;

pub(super) fn fetch_downloadable_xcodes(version: &str) -> Result<Vec<DownloadableXcode>> {
    let response = reqwest::blocking::ClientBuilder::new()
        .brotli(true)
        .deflate(true)
        .gzip(true)
        .build()
        .context("failed to build Xcode releases HTTP client")?
        .get(XCODE_RELEASES_INDEX_URL)
        .header(USER_AGENT, XCODE_RELEASES_USER_AGENT)
        .send()
        .context("failed to fetch Xcode release metadata from xcodereleases.com")?;
    let status = response.status();
    let body = response
        .bytes()
        .context("failed to read Xcode release metadata response body")?;
    if !status.is_success() {
        bail!(
            "failed to fetch Xcode release metadata from xcodereleases.com with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }

    let entries: Vec<XcodeReleasesEntry> = serde_json::from_slice(&body)
        .context("failed to parse Xcode release metadata from xcodereleases.com")?;
    matching_downloadable_xcodes(version, &entries)
}

pub(super) fn matching_downloadable_xcodes(
    requested_version: &str,
    entries: &[XcodeReleasesEntry],
) -> Result<Vec<DownloadableXcode>> {
    let stable = entries
        .iter()
        .filter(|entry| entry.version.release.release || entry.version.release.gm)
        .filter(|entry| version_matches(requested_version, &entry.version.number).unwrap_or(false))
        .collect::<Vec<_>>();
    if stable.is_empty() {
        return Ok(Vec::new());
    }

    let selected_version = if stable
        .iter()
        .any(|entry| entry.version.number == requested_version)
    {
        requested_version.to_owned()
    } else {
        stable
            .iter()
            .map(|entry| entry.version.number.as_str())
            .max_by(|left, right| compare_versions(left, right))
            .unwrap_or(requested_version)
            .to_owned()
    };

    let mut candidates = stable
        .into_iter()
        .filter(|entry| entry.version.number == selected_version)
        .filter_map(|entry| {
            let build_version = entry.version.build.as_ref()?.trim();
            if build_version.is_empty() {
                return None;
            }
            let download = entry.links.download.as_ref()?;
            let archive_url = download.url.trim();
            if archive_url.is_empty() {
                return None;
            }
            let remote_path = reqwest::Url::parse(archive_url).ok()?.path().to_owned();
            let archive_filename = Path::new(&remote_path)
                .file_name()?
                .to_string_lossy()
                .into_owned();
            let (variant_label, variant_rank) =
                download_variant(entry.name.as_str(), &download.architectures);
            Some(DownloadableXcode {
                version: entry.version.number.clone(),
                build_version: build_version.to_owned(),
                variant_label,
                variant_rank,
                archive_url: archive_url.to_owned(),
                archive_filename,
                remote_path,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.variant_rank
            .cmp(&right.variant_rank)
            .then_with(|| left.variant_label.cmp(&right.variant_label))
            .then_with(|| left.archive_filename.cmp(&right.archive_filename))
    });
    Ok(candidates)
}

fn download_variant(name: &str, architectures: &[String]) -> (String, u8) {
    let architectures = architectures
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let is_apple_silicon =
        architectures.len() == 1 && architectures.first().is_some_and(|value| value == "arm64");
    let is_universal = architectures.iter().any(|value| value == "arm64")
        && architectures.iter().any(|value| value == "x86_64");
    let prefers_apple_silicon = host_prefers_apple_silicon();

    if is_apple_silicon {
        return (
            "Apple Silicon".to_owned(),
            if prefers_apple_silicon { 0 } else { 1 },
        );
    }
    if is_universal {
        return (
            "Universal".to_owned(),
            if prefers_apple_silicon { 1 } else { 0 },
        );
    }
    if name.contains("Apple Silicon") {
        return (
            "Apple Silicon".to_owned(),
            if prefers_apple_silicon { 0 } else { 1 },
        );
    }
    if !architectures.is_empty() {
        return (architectures.join("/"), 2);
    }
    ("Default".to_owned(), 2)
}

fn host_prefers_apple_silicon() -> bool {
    matches!(std::env::consts::ARCH, "aarch64" | "arm64")
}

pub(super) fn install_requested_xcode(
    app: &AppContext,
    candidate: &DownloadableXcode,
    roots: &[PathBuf],
) -> Result<()> {
    let spinner = CliSpinner::new(format!("Installing {}", candidate.display_name()));
    let result = (|| {
        let install_root = preferred_xcode_install_root(roots)?;
        ensure_dir(&install_root)?;
        let archive_path = xcode_archive_path(app, candidate);
        if archive_path.exists() {
            spinner.set_message(format!("Using cached archive {}", archive_path.display()));
            return install_downloaded_xcode(&archive_path, candidate, &install_root, &spinner);
        }

        download_and_install_xcode(app, candidate, &archive_path, &install_root, &spinner)
    })();
    match result {
        Ok(install_path) => {
            spinner.finish_success(format!(
                "Installed {} at {}.",
                candidate.display_name(),
                install_path.display()
            ));
            Ok(())
        }
        Err(error) => {
            spinner.finish_clear();
            Err(error)
        }
    }
}

fn xcode_archive_path(app: &AppContext, candidate: &DownloadableXcode) -> PathBuf {
    app.global_paths
        .cache_dir
        .join("xcodes")
        .join("archives")
        .join(format!("{}-{}", candidate.version, candidate.build_version))
        .join(&candidate.archive_filename)
}

fn download_and_install_xcode(
    app: &AppContext,
    candidate: &DownloadableXcode,
    archive_path: &Path,
    install_root: &Path,
    spinner: &CliSpinner,
) -> Result<PathBuf> {
    ensure_parent_dir(archive_path)?;
    let partial_path = partial_download_path(archive_path)?;
    if partial_path.exists() {
        fs::remove_file(&partial_path)
            .with_context(|| format!("failed to clear {}", partial_path.display()))?;
    }

    spinner.set_message(format!(
        "Authorizing Apple Developer download for {}",
        candidate.display_name()
    ));
    let expansion_root = expansion_root_for_archive(archive_path, candidate)?;
    let direct_result = (|| -> Result<()> {
        let mut developer_services = DeveloperServicesClient::authenticate_for_xcode_download(
            app,
            &candidate.version,
            &candidate.build_version,
        )?;
        download_and_extract_xcode_archive(
            &mut developer_services,
            candidate,
            archive_path,
            &partial_path,
            &expansion_root,
            spinner,
        )
    })();
    match direct_result {
        Ok(()) => {}
        Err(error) if should_retry_with_installed_xcode_auth(&error) => {
            spinner.set_message(format!(
                "Retrying Apple Developer download authorization for {}",
                candidate.display_name()
            ));
            let mut developer_services = DeveloperServicesClient::authenticate(app)?;
            download_and_extract_xcode_archive(
                &mut developer_services,
                candidate,
                archive_path,
                &partial_path,
                &expansion_root,
                spinner,
            )?;
        }
        Err(error) => return Err(error),
    }
    install_extracted_xcode(&expansion_root, candidate, install_root, spinner)
}

fn partial_download_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("download path was missing a file name")?;
    Ok(path.with_file_name(format!("{file_name}.part")))
}

fn expansion_root_for_archive(
    archive_path: &Path,
    candidate: &DownloadableXcode,
) -> Result<PathBuf> {
    Ok(archive_path
        .parent()
        .context("downloaded Xcode archive did not have a parent directory")?
        .join(format!("expand-{}", candidate.build_version)))
}

fn download_and_extract_xcode_archive(
    developer_services: &mut DeveloperServicesClient,
    candidate: &DownloadableXcode,
    archive_path: &Path,
    partial_path: &Path,
    expansion_root: &Path,
    spinner: &CliSpinner,
) -> Result<()> {
    for attempt in 1..=XCODE_DOWNLOAD_RETRY_ATTEMPTS {
        developer_services.authorize_download_path(&candidate.remote_path)?;
        let result = download_and_extract_from_authorized_archive(
            &developer_services.clone_http_client(),
            &developer_services.download_headers()?,
            candidate,
            archive_path,
            partial_path,
            expansion_root,
            spinner,
        );
        match result {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt < XCODE_DOWNLOAD_RETRY_ATTEMPTS
                    && should_retry_xcode_archive_download(&error) =>
            {
                spinner.set_message(format!(
                    "Retrying Xcode archive download after a transient Apple network error ({attempt}/{XCODE_DOWNLOAD_RETRY_ATTEMPTS})"
                ));
                thread::sleep(XCODE_DOWNLOAD_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("download retry loop should return or error")
}

fn cleanup_download_attempt(partial_path: &Path, expansion_root: &Path) {
    let _ = fs::remove_file(partial_path);
    let _ = fs::remove_dir_all(expansion_root);
}

fn download_and_extract_from_authorized_archive(
    client: &HttpClient,
    headers: &HeaderMap,
    candidate: &DownloadableXcode,
    archive_path: &Path,
    partial_path: &Path,
    expansion_root: &Path,
    spinner: &CliSpinner,
) -> Result<()> {
    cleanup_download_attempt(partial_path, expansion_root);
    spinner.set_message(format!(
        "Downloading and extracting {}",
        candidate.archive_filename
    ));
    spinner.suspend(|| {
        stream_download_to_path_and_extract(
            client,
            headers,
            &candidate.archive_url,
            &candidate.archive_filename,
            archive_path,
            partial_path,
            expansion_root,
        )
    })
}

fn stream_download_to_path_and_extract(
    client: &HttpClient,
    headers: &HeaderMap,
    url: &str,
    label: &str,
    destination: &Path,
    partial_path: &Path,
    expansion_root: &Path,
) -> Result<()> {
    let response = client
        .get(url)
        .headers(headers.clone())
        .send()
        .with_context(|| format!("failed to download Xcode archive from {url}"))?;
    if response.url().path().contains("/unauthorized") {
        bail!(
            "Apple redirected the Xcode archive download to an unauthorized page. Re-authenticate your Apple ID and make sure developer downloads are allowed for that account."
        );
    }

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .unwrap_or_else(|_| "<unreadable response body>".to_owned());
        bail!("Xcode archive download failed with {status}: {body}");
    }
    let content_length = response.content_length();

    cache_and_extract_download_stream(
        response,
        label,
        content_length,
        destination,
        partial_path,
        expansion_root,
    )
}

fn install_downloaded_xcode(
    archive_path: &Path,
    candidate: &DownloadableXcode,
    install_root: &Path,
    spinner: &CliSpinner,
) -> Result<PathBuf> {
    let expansion_root = expansion_root_for_archive(archive_path, candidate)?;
    spinner.set_message(format!("Extracting {}", archive_path.display()));
    spinner
        .suspend(|| {
            let file = fs::File::open(archive_path)
                .with_context(|| format!("failed to open {}", archive_path.display()))?;
            extract_xcode_app_from_xip_stream(file, &expansion_root)
        })
        .with_context(|| format!("failed to extract {}", archive_path.display()))?;

    install_extracted_xcode(&expansion_root, candidate, install_root, spinner)
}

fn install_extracted_xcode(
    expansion_root: &Path,
    candidate: &DownloadableXcode,
    install_root: &Path,
    spinner: &CliSpinner,
) -> Result<PathBuf> {
    let extracted_app = find_expanded_xcode_app(expansion_root)?;
    let install_path = install_root.join(candidate.install_bundle_name());
    if install_path.exists() {
        if let Some(existing) = load_xcode_bundle(&install_path)?
            && existing.version == candidate.version
            && existing.build_version == candidate.build_version
        {
            return Ok(install_path);
        }
        bail!(
            "Orbit refused to overwrite the existing Xcode install at {}",
            install_path.display()
        );
    }

    spinner.set_message(format!("Installing {}", candidate.display_name()));
    let mut move_app = Command::new("mv");
    move_app.arg(&extracted_app).arg(&install_path);
    run_command(&mut move_app).with_context(|| {
        format!(
            "failed to move {} into {}",
            extracted_app.display(),
            install_path.display()
        )
    })?;
    let _ = fs::remove_dir_all(expansion_root);

    let installed = load_xcode_bundle(&install_path)?.with_context(|| {
        format!(
            "expected a valid Xcode bundle at {} after installation",
            install_path.display()
        )
    })?;
    if installed.version != candidate.version || installed.build_version != candidate.build_version
    {
        bail!(
            "installed Xcode metadata at {} did not match the requested release {} ({})",
            install_path.display(),
            candidate.version,
            candidate.build_version
        );
    }
    Ok(install_path)
}

pub(super) fn extract_xcode_app_from_xip_stream<R: Read>(
    xip_stream: R,
    expansion_root: &Path,
) -> Result<()> {
    let content_stream = xar_stream::open_member_stream(xip_stream, "Content")
        .context("failed to read Xcode payload from XIP")?;
    extract_xcode_app_from_content_stream(content_stream, expansion_root)
}

fn extract_xcode_app_from_content_stream<R: Read>(
    mut content_reader: R,
    expansion_root: &Path,
) -> Result<()> {
    recreate_dir(expansion_root)?;

    let mut decoder = Command::new("compression_tool")
        .arg("-decode")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to start compression_tool for streamed Xcode extraction")?;
    let decoder_stdout = decoder
        .stdout
        .take()
        .context("compression_tool did not provide stdout for streamed Xcode extraction")?;

    let mut extractor = Command::new("cpio");
    extractor
        .args(["-idmu", "--quiet"])
        .current_dir(expansion_root)
        .stdin(Stdio::from(decoder_stdout))
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut extractor = extractor
        .spawn()
        .context("failed to start cpio for streamed Xcode extraction")?;

    let mut decoder_stdin = decoder
        .stdin
        .take()
        .context("compression_tool did not provide stdin for streamed Xcode extraction")?;
    io::copy(&mut content_reader, &mut decoder_stdin)
        .context("failed to stream the Xcode payload into compression_tool")?;
    drop(decoder_stdin);

    let decoder_status = decoder
        .wait()
        .context("failed to wait for compression_tool decode")?;
    if !decoder_status.success() {
        bail!("`compression_tool` exited with status {decoder_status}");
    }

    let extractor_status = extractor
        .wait()
        .context("failed to wait for cpio extraction")?;
    if !extractor_status.success() {
        bail!("`cpio` exited with status {extractor_status}");
    }

    Ok(())
}

fn cache_and_extract_download_stream<R: Read>(
    source: R,
    label: &str,
    total_bytes: Option<u64>,
    destination: &Path,
    partial_path: &Path,
    expansion_root: &Path,
) -> Result<()> {
    let file = fs::File::create(partial_path)
        .with_context(|| format!("failed to create {}", partial_path.display()))?;
    let reader = ProgressTeeReader::new(source, file, label, total_bytes, destination);
    let mut content_stream = xar_stream::open_member_stream(reader, "Content")
        .context("failed to open the Xcode payload from the archive stream")?;
    extract_xcode_app_from_content_stream(&mut content_stream, expansion_root)?;

    let mut reader = content_stream.into_inner();
    io::copy(&mut reader, &mut io::sink())
        .context("failed to drain the remainder of the Xcode archive stream")?;
    reader.flush_mirror()?;
    fs::rename(partial_path, destination).with_context(|| {
        format!(
            "failed to move downloaded Xcode archive to {}",
            destination.display()
        )
    })?;
    reader.finish();
    Ok(())
}

fn should_retry_with_installed_xcode_auth(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("download authorization failed with 401")
        || message.contains("download authorization failed with 403")
        || message.contains("apple developer download authorization failed with 401")
        || message.contains("apple developer download authorization failed with 403")
}

fn should_retry_xcode_archive_download(error: &anyhow::Error) -> bool {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .map(|error| error.is_timeout() || error.is_connect() || error.is_request())
            .unwrap_or(false)
    }) {
        return true;
    }

    let message = format!("{error:#}").to_ascii_lowercase();
    message.contains("tls handshake eof")
        || message.contains("unexpected eof")
        || message.contains("connection reset")
        || message.contains("connection aborted")
        || message.contains("broken pipe")
        || message.contains("timed out")
}

struct ProgressTeeReader<R, W> {
    inner: R,
    mirror: W,
    progress: CliDownloadProgress,
    downloaded_bytes: u64,
    destination: PathBuf,
}

impl<R, W> ProgressTeeReader<R, W> {
    fn new(inner: R, mirror: W, label: &str, total_bytes: Option<u64>, destination: &Path) -> Self {
        Self {
            inner,
            mirror,
            progress: CliDownloadProgress::new(label, total_bytes),
            downloaded_bytes: 0,
            destination: destination.to_path_buf(),
        }
    }
}

impl<R, W: Write> ProgressTeeReader<R, W> {
    fn flush_mirror(&mut self) -> Result<()> {
        self.mirror
            .flush()
            .context("failed to flush mirrored download")
    }

    fn finish(mut self) {
        self.progress
            .finish(self.downloaded_bytes, &self.destination);
    }
}

impl<R: Read, W: Write> Read for ProgressTeeReader<R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if read == 0 {
            return Ok(0);
        }

        self.mirror.write_all(&buffer[..read])?;
        self.downloaded_bytes += read as u64;
        self.progress.advance(self.downloaded_bytes);
        Ok(read)
    }
}

fn find_expanded_xcode_app(root: &Path) -> Result<PathBuf> {
    WalkDir::new(root)
        .max_depth(2)
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .find(|path| load_xcode_bundle(path).ok().flatten().is_some())
        .with_context(|| {
            format!(
                "streamed Xcode extraction did not produce a valid Xcode.app bundle under {}",
                root.display()
            )
        })
}

fn recreate_dir(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_dir_all(path).with_context(|| format!("failed to clear {}", path.display()))?;
    }
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))
}
