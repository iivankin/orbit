use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Serialize;
use serde_json::{Value, json};

use crate::apple::analysis::{
    AnalysisProject, C_FAMILY_HEADER_EXTENSIONS, C_FAMILY_SOURCE_EXTENSIONS,
    SemanticArtifactCacheStatus, SemanticCompilationArtifact, SemanticCompilerInvocation,
    build_cached_semantic_compilation_artifact_with_status, collect_target_header_files,
    load_persistent_analysis_project,
};
use crate::apple::build;
use crate::apple::build::external::target_dependency_watch_roots;
use crate::apple::build::toolchain::DestinationKind;
use crate::context::AppContext;
use crate::manifest::{ApplePlatform, TargetKind};
use crate::util::{print_success, write_json_file};

pub(crate) const BSP_VERSION: &str = "2.2.0";
pub(crate) const BSP_CONNECTION_FILE_NAME: &str = "orbi.json";

#[derive(Debug, Serialize)]
pub(crate) struct BspConnectionDetails {
    pub name: String,
    pub version: String,
    #[serde(rename = "bspVersion")]
    pub bsp_version: String,
    pub languages: Vec<String>,
    pub argv: Vec<String>,
}

pub fn serve(app: &AppContext, requested_manifest: Option<&Path>) -> Result<()> {
    let mut server = BspServer::new(app, requested_manifest);
    server.run()
}

pub fn install_connection_files(app: &AppContext, requested_manifest: Option<&Path>) -> Result<()> {
    let manifest_path = app.resolve_manifest_path_for_dispatch(requested_manifest)?;
    let standard_path =
        install_connection_file_for_manifest_with_env(&manifest_path, app.manifest_env())?;
    print_success(format!("Installed {}", standard_path.display()));
    Ok(())
}

pub(crate) fn install_connection_file_for_manifest(manifest_path: &Path) -> Result<PathBuf> {
    install_connection_file_for_manifest_with_env(manifest_path, None)
}

pub(crate) fn install_connection_file_for_manifest_with_env(
    manifest_path: &Path,
    env: Option<&str>,
) -> Result<PathBuf> {
    let manifest_path = manifest_path
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", manifest_path.display()))?;
    let project_root = manifest_path
        .parent()
        .context("manifest path did not contain a parent directory")?;
    let details = connection_details_with_env(&manifest_path, env)?;

    let standard_path = project_root.join(".bsp").join(BSP_CONNECTION_FILE_NAME);
    write_json_file(&standard_path, &details)?;
    Ok(standard_path)
}

#[allow(dead_code)]
pub(crate) fn connection_details(manifest_path: &Path) -> Result<BspConnectionDetails> {
    connection_details_with_env(manifest_path, None)
}

pub(crate) fn connection_details_with_env(
    manifest_path: &Path,
    env: Option<&str>,
) -> Result<BspConnectionDetails> {
    let executable_path = std::env::current_exe()
        .context("failed to resolve the current Orbi executable")?
        .canonicalize()
        .context("failed to canonicalize the current Orbi executable")?;
    let manifest_arg = manifest_path.to_string_lossy().into_owned();
    let mut argv = vec![
        executable_path.to_string_lossy().into_owned(),
        "--manifest".to_owned(),
        manifest_arg,
    ];
    if let Some(env) = env {
        argv.push("--env".to_owned());
        argv.push(env.to_owned());
    }
    argv.push("bsp".to_owned());
    Ok(BspConnectionDetails {
        name: "orbi".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        bsp_version: BSP_VERSION.to_owned(),
        languages: vec![
            "swift".to_owned(),
            "c".to_owned(),
            "objective-c".to_owned(),
            "cpp".to_owned(),
            "objective-cpp".to_owned(),
        ],
        argv,
    })
}

struct BspServer {
    app: AppContext,
    requested_manifest: Option<PathBuf>,
    snapshot: Option<BspSnapshot>,
    last_snapshot_cache_status: Option<SemanticArtifactCacheStatus>,
    shutdown_requested: bool,
    next_task_id: u64,
}

struct TaskProgress<'a> {
    task: &'a Value,
    origin_id: Option<&'a str>,
    progress: u64,
    total: u64,
    message: &'a str,
    unit: &'a str,
}

impl BspServer {
    fn new(app: &AppContext, requested_manifest: Option<&Path>) -> Self {
        Self {
            app: app.clone(),
            requested_manifest: requested_manifest.map(PathBuf::from),
            snapshot: None,
            last_snapshot_cache_status: None,
            shutdown_requested: false,
            next_task_id: 1,
        }
    }

    fn run(&mut self) -> Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut reader = BufReader::new(stdin.lock());
        let mut writer = BufWriter::new(stdout.lock());

        while let Some(message) = read_jsonrpc_message(&mut reader)? {
            if !self.handle_message(&message, &mut writer)? {
                break;
            }
        }

        writer.flush().context("failed to flush BSP output")
    }

    fn handle_message<W: Write>(&mut self, message: &Value, writer: &mut W) -> Result<bool> {
        let id = message.get("id").cloned();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .context("received a JSON-RPC message without a string `method`")?;
        let null = Value::Null;
        let params = message.get("params").unwrap_or(&null);

        let response = match method {
            "build/initialize" => response_for_result(id, self.initialize()),
            "build/initialized" | "$/cancelRequest" => None,
            "workspace/reload" => self.handle_reload_request(id, writer)?,
            "build/shutdown" => {
                self.shutdown_requested = true;
                id.map(|id| ok_response(id, Value::Null))
            }
            "build/exit" => {
                if !self.shutdown_requested {
                    bail!("received `build/exit` before `build/shutdown`");
                }
                return Ok(false);
            }
            "workspace/buildTargets" => response_for_result(id, self.workspace_build_targets()),
            "buildTarget/sources" => response_for_result(id, self.build_target_sources(params)),
            "buildTarget/inverseSources" => {
                response_for_result(id, self.build_target_inverse_sources(params))
            }
            "buildTarget/outputPaths" => {
                response_for_result(id, self.build_target_output_paths(params))
            }
            "textDocument/sourceKitOptions" => {
                response_for_result(id, self.text_document_sourcekit_options(params))
            }
            "buildTarget/prepare" => self.handle_prepare_request(id, params, writer)?,
            "workspace/waitForBuildSystemUpdates" => id.map(|id| ok_response(id, Value::Null)),
            "workspace/didChangeWatchedFiles" => {
                // Source edits can temporarily leave the workspace in a broken state.
                // Keep serving the last known-good snapshot instead of crashing the BSP server.
                let changes = watched_file_changes_from_params(params);
                match self.handle_watched_file_changes(changes.as_slice(), writer) {
                    Ok(()) => {
                        // Notifications are emitted by the watched-file handler itself.
                    }
                    Err(error) => {
                        self.emit_log_message(
                            writer,
                            2,
                            format!(
                                "Orbi BSP ignored watched file changes because reload failed: {error:#}"
                            ),
                            None,
                            None,
                            None,
                        )?;
                    }
                }
                None
            }
            other => id.map(|id| error_response(id, -32601, format!("method `{other}` not found"))),
        };

        if let Some(response) = response {
            write_jsonrpc_message(writer, &response)?;
        }
        Ok(true)
    }

    fn handle_watched_file_changes<W: Write>(
        &mut self,
        changes: &[WatchedFileChange],
        writer: &mut W,
    ) -> Result<()> {
        if self.snapshot.is_none() {
            self.reload_snapshot(&[])?;
        }
        if self.can_skip_snapshot_reload_for_watched_file_changes(changes) {
            let changed_files = changes
                .iter()
                .map(|change| change.path.clone())
                .collect::<Vec<_>>();
            self.emit_log_message(
                writer,
                4,
                "Orbi BSP updated target change notifications from watched source edits without rebuilding the semantic snapshot.",
                None,
                None,
                Some(structured_log_payload("report", None)),
            )?;
            if let Some(notification) = build_target_did_change_notification_with_changed_files(
                self.snapshot.as_ref(),
                self.snapshot.as_ref(),
                &changed_files,
            ) {
                write_jsonrpc_message(writer, &notification)?;
            }
            return Ok(());
        }

        let changed_files = changes
            .iter()
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        self.reload_snapshot(changed_files.as_slice())?;
        self.emit_log_message(
            writer,
            4,
            "Orbi BSP reloaded build settings after watched file changes.",
            None,
            None,
            Some(structured_log_payload("report", None)),
        )?;
        if let Some(notification) = self.take_pending_did_change_notification() {
            write_jsonrpc_message(writer, &notification)?;
        }
        Ok(())
    }

    fn initialize(&mut self) -> Result<Value> {
        self.reload_snapshot(&[])?;
        let snapshot = self.snapshot()?;
        Ok(json!({
            "displayName": "orbi",
            "version": env!("CARGO_PKG_VERSION"),
            "bspVersion": BSP_VERSION,
            "rootUri": snapshot.project_root_uri.as_str(),
            "capabilities": {
                "buildTargetChangedProvider": true,
                "inverseSourcesProvider": true,
                "dependencySourcesProvider": false,
                "dependencyModulesProvider": false,
                "resourcesProvider": false,
                "outputPathsProvider": true,
                "canReload": true
            },
            "dataKind": "sourceKit",
            "data": {
                "indexDatabasePath": snapshot.index_database_path,
                "indexStorePath": snapshot.index_store_path,
                "outputPathsProvider": true,
                "prepareProvider": true,
                "sourceKitOptionsProvider": true,
                "watchers": build_watchers()
            }
        }))
    }

    fn workspace_build_targets(&mut self) -> Result<Value> {
        Ok(json!({
            "targets": self.snapshot()?.targets.iter().map(BspTarget::build_target_json).collect::<Vec<_>>()
        }))
    }

    fn build_target_sources(&mut self, params: &Value) -> Result<Value> {
        let target_ids = target_ids_from_params(params)?;
        let snapshot = self.snapshot()?;
        let items = target_ids
            .into_iter()
            .filter_map(|target_id| snapshot.targets_by_id.get(&target_id))
            .map(BspTarget::sources_item_json)
            .collect::<Vec<_>>();
        Ok(json!({ "items": items }))
    }

    fn build_target_inverse_sources(&mut self, params: &Value) -> Result<Value> {
        let document_uri = params
            .get("textDocument")
            .and_then(Value::as_object)
            .and_then(|text_document| text_document.get("uri"))
            .and_then(Value::as_str)
            .context("missing `textDocument.uri` in `buildTarget/inverseSources` params")?;
        let document_path = file_path_from_uri(document_uri)?;
        let snapshot = self.snapshot()?;
        let target_ids = snapshot
            .targets_by_source_path
            .get(&document_path)
            .cloned()
            .unwrap_or_default();
        Ok(json!({
            "targets": target_ids.into_iter().map(|uri| json!({ "uri": uri })).collect::<Vec<_>>()
        }))
    }

    fn build_target_output_paths(&mut self, params: &Value) -> Result<Value> {
        let target_ids = target_ids_from_params(params)?;
        let snapshot = self.snapshot()?;
        let items = target_ids
            .into_iter()
            .filter_map(|target_id| snapshot.targets_by_id.get(&target_id))
            .map(BspTarget::output_paths_item_json)
            .collect::<Vec<_>>();
        Ok(json!({ "items": items }))
    }

    fn text_document_sourcekit_options(&mut self, params: &Value) -> Result<Value> {
        let target_id = params
            .get("target")
            .and_then(Value::as_object)
            .and_then(|target| target.get("uri"))
            .and_then(Value::as_str)
            .context("missing `target.uri` in `textDocument/sourceKitOptions` params")?;
        let document_uri = params
            .get("textDocument")
            .and_then(Value::as_object)
            .and_then(|text_document| text_document.get("uri"))
            .and_then(Value::as_str)
            .context("missing `textDocument.uri` in `textDocument/sourceKitOptions` params")?;
        let document_path = file_path_from_uri(document_uri)?;
        let requested_language = params.get("language").and_then(Value::as_str);
        let snapshot = self.snapshot()?;
        let Some(target) = snapshot.targets_by_id.get(target_id) else {
            return Ok(Value::Null);
        };
        let Some(mut options) = target.sourcekit_options(&document_path, requested_language) else {
            return Ok(Value::Null);
        };
        if options.language_id.as_deref() == Some("swift") {
            if options
                .compiler_arguments
                .first()
                .is_some_and(|argument| argument == "swiftc")
            {
                options.compiler_arguments.remove(0);
            }
            if let Some(index_unit_output_path) = options.index_unit_output_path.take() {
                options
                    .compiler_arguments
                    .push("-index-unit-output-path".to_owned());
                options.compiler_arguments.push(index_unit_output_path);
            }
        }
        Ok(json!({
            "compilerArguments": options.compiler_arguments,
            "workingDirectory": target.working_directory
        }))
    }

    fn prepare_targets(&mut self, params: &Value) -> Result<Value> {
        let target_ids = target_ids_from_params(params)?;
        let grouped_targets = {
            let snapshot = self.snapshot()?;
            let mut grouped = HashMap::<(String, String), Vec<String>>::new();
            for target_id in target_ids {
                let Some(target) = snapshot.targets_by_id.get(&target_id) else {
                    continue;
                };
                grouped
                    .entry((target.platform.clone(), target.destination.clone()))
                    .or_default()
                    .push(target.target_name.clone());
            }
            grouped
        };
        let snapshot = self
            .snapshot
            .as_ref()
            .context("BSP snapshot was not initialized")?;
        for ((platform, destination), target_names) in grouped_targets {
            build::prepare_for_ide(
                &snapshot.analysis_project.project,
                apple_platform_from_str(&platform)?,
                &target_names,
                destination_from_str(&destination)?,
                &snapshot.index_store_path,
            )?;
        }
        Ok(Value::Null)
    }

    fn handle_prepare_request<W: Write>(
        &mut self,
        id: Option<Value>,
        params: &Value,
        writer: &mut W,
    ) -> Result<Option<Value>> {
        let origin_id = origin_id_from_params(params);
        let task = self.new_task_id();
        let snapshot_was_missing = self.snapshot.is_none();
        let grouped_targets = {
            let target_ids = target_ids_from_params(params)?;
            let snapshot = self.snapshot()?;
            let mut grouped = HashMap::<(String, String), Vec<String>>::new();
            for target_id in target_ids {
                let Some(target) = snapshot.targets_by_id.get(&target_id) else {
                    continue;
                };
                grouped
                    .entry((target.platform.clone(), target.destination.clone()))
                    .or_default()
                    .push(target.target_name.clone());
            }
            grouped
        };
        let total_groups = grouped_targets.len();
        self.emit_log_message(
            writer,
            4,
            format!(
                "Preparing {} Orbi build target group(s) for editor support.",
                total_groups
            ),
            Some(&task),
            origin_id.as_deref(),
            Some(structured_log_payload(
                "begin",
                Some("Preparing Orbi build targets"),
            )),
        )?;
        self.emit_task_start(
            writer,
            &task,
            origin_id.as_deref(),
            format!(
                "Preparing {} Orbi build target group(s) for editor support.",
                total_groups
            ),
            Some(json!({
                "workDoneProgressTitle": "Preparing Orbi build targets"
            })),
        )?;

        let response = match self.prepare_targets(params) {
            Ok(value) => {
                if snapshot_was_missing && let Some(cache_status) = self.last_snapshot_cache_status
                {
                    self.emit_log_message(
                        writer,
                        4,
                        cache_status.message(None),
                        Some(&task),
                        origin_id.as_deref(),
                        Some(structured_log_payload(
                            "report",
                            Some("Semantic analysis cache"),
                        )),
                    )?;
                }
                if let Some(notification) = self.take_pending_did_change_notification() {
                    write_jsonrpc_message(writer, &notification)?;
                }
                self.emit_task_progress(
                    writer,
                    TaskProgress {
                        task: &task,
                        origin_id: origin_id.as_deref(),
                        progress: total_groups as u64,
                        total: total_groups as u64,
                        message: "Prepared Orbi build targets.",
                        unit: "targets",
                    },
                )?;
                self.emit_log_message(
                    writer,
                    4,
                    "Prepared Orbi build targets for editor support.",
                    Some(&task),
                    origin_id.as_deref(),
                    Some(structured_log_payload("end", None)),
                )?;
                self.emit_task_finish(
                    writer,
                    &task,
                    origin_id.as_deref(),
                    1,
                    "Prepared Orbi build targets.",
                    None,
                )?;
                Ok(value)
            }
            Err(error) => {
                self.emit_log_message(
                    writer,
                    1,
                    format!("Orbi BSP failed to prepare build targets: {error:#}"),
                    Some(&task),
                    origin_id.as_deref(),
                    Some(structured_log_payload("end", None)),
                )?;
                self.emit_task_finish(
                    writer,
                    &task,
                    origin_id.as_deref(),
                    2,
                    format!("Failed to prepare Orbi build targets: {error}"),
                    None,
                )?;
                Err(error)
            }
        };
        Ok(id.map(|id| match response {
            Ok(result) => ok_response(id, result),
            Err(error) => error_response(id, -32603, format!("{error:#}")),
        }))
    }

    fn handle_reload_request<W: Write>(
        &mut self,
        id: Option<Value>,
        writer: &mut W,
    ) -> Result<Option<Value>> {
        let task = self.new_task_id();
        self.emit_log_message(
            writer,
            4,
            "Reloading Orbi BSP workspace state.",
            Some(&task),
            None,
            Some(structured_log_payload(
                "begin",
                Some("Reloading Orbi workspace"),
            )),
        )?;
        self.emit_task_start(
            writer,
            &task,
            None,
            "Reloading Orbi BSP workspace state.",
            Some(json!({
                "workDoneProgressTitle": "Reloading Orbi workspace"
            })),
        )?;
        let response = match self.reload_workspace() {
            Ok(value) => {
                if let Some(cache_status) = self.last_snapshot_cache_status {
                    self.emit_log_message(
                        writer,
                        4,
                        cache_status.message(None),
                        Some(&task),
                        None,
                        Some(structured_log_payload(
                            "report",
                            Some("Semantic analysis cache"),
                        )),
                    )?;
                }
                self.emit_task_progress(
                    writer,
                    TaskProgress {
                        task: &task,
                        origin_id: None,
                        progress: 1,
                        total: 1,
                        message: "Reloaded Orbi workspace.",
                        unit: "steps",
                    },
                )?;
                self.emit_log_message(
                    writer,
                    4,
                    "Reloaded Orbi BSP workspace state.",
                    Some(&task),
                    None,
                    Some(structured_log_payload("end", None)),
                )?;
                self.emit_task_finish(writer, &task, None, 1, "Reloaded Orbi workspace.", None)?;
                Ok(value)
            }
            Err(error) => {
                self.emit_log_message(
                    writer,
                    1,
                    format!("Orbi BSP failed to reload workspace state: {error:#}"),
                    Some(&task),
                    None,
                    Some(structured_log_payload("end", None)),
                )?;
                self.emit_task_finish(
                    writer,
                    &task,
                    None,
                    2,
                    format!("Failed to reload Orbi workspace: {error}"),
                    None,
                )?;
                Err(error)
            }
        };
        Ok(id.map(|id| match response {
            Ok(result) => ok_response(id, result),
            Err(error) => error_response(id, -32603, format!("{error:#}")),
        }))
    }

    fn reload_snapshot(&mut self, changed_files: &[PathBuf]) -> Result<()> {
        let analysis_project =
            load_persistent_analysis_project(&self.app, self.requested_manifest.as_deref())?;
        let cached_artifact = build_cached_semantic_compilation_artifact_with_status(
            &analysis_project.project,
            None,
        )?;
        self.last_snapshot_cache_status = Some(cached_artifact.cache_status);
        let artifact = cached_artifact.artifact;
        let new_snapshot = BspSnapshot::from_analysis(analysis_project, artifact)?;
        let previous_snapshot = self.snapshot.as_ref();
        let did_change_notification = previous_snapshot.and_then(|previous_snapshot| {
            build_target_did_change_notification_with_changed_files(
                Some(previous_snapshot),
                Some(&new_snapshot),
                changed_files,
            )
        });
        self.snapshot = Some(new_snapshot);
        if let Some(snapshot) = self.snapshot.as_mut() {
            snapshot.pending_did_change_notification = did_change_notification;
        }
        Ok(())
    }

    fn snapshot(&mut self) -> Result<&BspSnapshot> {
        if self.snapshot.is_none() {
            self.reload_snapshot(&[])?;
        }
        self.snapshot
            .as_ref()
            .context("BSP snapshot was not initialized")
    }

    fn reload_workspace(&mut self) -> Result<Value> {
        self.reload_snapshot(&[])?;
        Ok(Value::Null)
    }

    fn take_pending_did_change_notification(&mut self) -> Option<Value> {
        self.snapshot
            .as_mut()
            .and_then(|snapshot| snapshot.pending_did_change_notification.take())
    }

    fn can_skip_snapshot_reload_for_watched_file_changes(
        &self,
        changes: &[WatchedFileChange],
    ) -> bool {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return false;
        };
        if changes.is_empty() {
            return true;
        }
        changes.iter().all(|change| {
            if watched_file_change_affects_build_graph(&change.path) {
                return false;
            }
            if watched_file_change_is_quality_only(&change.path) {
                return true;
            }
            if snapshot.contains_dependency_input_path(&change.path) {
                return false;
            }
            if !watched_file_change_is_source_like(&change.path) {
                return true;
            }
            change.kind == WatchedFileChangeKind::Changed
                && snapshot.targets_by_source_path.contains_key(&change.path)
        })
    }

    fn new_task_id(&mut self) -> Value {
        let task_id = format!("orbi-task-{}", self.next_task_id);
        self.next_task_id += 1;
        json!({ "id": task_id })
    }

    fn emit_log_message<W: Write>(
        &self,
        writer: &mut W,
        message_type: i64,
        message: impl Into<String>,
        task: Option<&Value>,
        origin_id: Option<&str>,
        structure: Option<Value>,
    ) -> Result<()> {
        let mut params = json!({
            "type": message_type,
            "message": message.into()
        });
        if let Some(task) = task {
            params["task"] = task.clone();
        }
        if let Some(origin_id) = origin_id {
            params["originId"] = Value::String(origin_id.to_owned());
        }
        if let Some(structure) = structure {
            params["structure"] = structure;
        }
        write_jsonrpc_message(
            writer,
            &json!({
                "jsonrpc": "2.0",
                "method": "build/logMessage",
                "params": params
            }),
        )
    }

    fn emit_task_start<W: Write>(
        &self,
        writer: &mut W,
        task: &Value,
        origin_id: Option<&str>,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Result<()> {
        let mut params = json!({
            "taskId": task,
            "message": message.into()
        });
        if let Some(origin_id) = origin_id {
            params["originId"] = Value::String(origin_id.to_owned());
        }
        if let Some(data) = data {
            params["data"] = data;
        }
        write_jsonrpc_message(
            writer,
            &json!({
                "jsonrpc": "2.0",
                "method": "build/taskStart",
                "params": params
            }),
        )
    }

    fn emit_task_progress<W: Write>(&self, writer: &mut W, update: TaskProgress<'_>) -> Result<()> {
        let mut params = json!({
            "taskId": update.task,
            "message": update.message,
            "progress": update.progress,
            "total": update.total,
            "unit": update.unit
        });
        if let Some(origin_id) = update.origin_id {
            params["originId"] = Value::String(origin_id.to_owned());
        }
        write_jsonrpc_message(
            writer,
            &json!({
                "jsonrpc": "2.0",
                "method": "build/taskProgress",
                "params": params
            }),
        )
    }

    fn emit_task_finish<W: Write>(
        &self,
        writer: &mut W,
        task: &Value,
        origin_id: Option<&str>,
        status: i64,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Result<()> {
        let mut params = json!({
            "taskId": task,
            "status": status,
            "message": message.into()
        });
        if let Some(origin_id) = origin_id {
            params["originId"] = Value::String(origin_id.to_owned());
        }
        if let Some(data) = data {
            params["data"] = data;
        }
        write_jsonrpc_message(
            writer,
            &json!({
                "jsonrpc": "2.0",
                "method": "build/taskFinish",
                "params": params
            }),
        )
    }
}

struct BspSnapshot {
    analysis_project: AnalysisProject,
    project_root_uri: String,
    index_store_path: PathBuf,
    index_database_path: PathBuf,
    pending_did_change_notification: Option<Value>,
    targets: Vec<BspTarget>,
    targets_by_id: HashMap<String, BspTarget>,
    targets_by_source_path: HashMap<PathBuf, Vec<String>>,
    targets_by_dependency_root: Vec<(PathBuf, Vec<String>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchedFileChange {
    path: PathBuf,
    kind: WatchedFileChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchedFileChangeKind {
    Created,
    Changed,
    Deleted,
    Unknown,
}

impl BspSnapshot {
    fn from_analysis(
        analysis_project: AnalysisProject,
        artifact: SemanticCompilationArtifact,
    ) -> Result<Self> {
        let project_root_uri = file_uri(analysis_project.project.root.as_path())?;
        let project = &analysis_project.project;
        let targets_by_name = analysis_project
            .project
            .resolved_manifest
            .targets
            .iter()
            .map(|target| (target.name.as_str(), target))
            .collect::<HashMap<_, _>>();

        let mut invocations_by_target =
            HashMap::<(String, String, String), Vec<SemanticCompilerInvocation>>::new();
        for invocation in artifact.invocations {
            invocations_by_target
                .entry((
                    invocation.platform.clone(),
                    invocation.destination.clone(),
                    invocation.target.clone(),
                ))
                .or_default()
                .push(invocation);
        }

        let mut targets = invocations_by_target
            .into_iter()
            .map(|((platform, destination, target_name), invocations)| {
                let manifest_target = targets_by_name
                    .get(target_name.as_str())
                    .copied()
                    .with_context(|| {
                        format!("missing target `{}` in resolved manifest", target_name)
                    })?;
                let header_files =
                    collect_target_header_files(project, manifest_target, &|_| true)?;
                let output_root = analysis_project
                    .project
                    .project_paths
                    .build_dir
                    .join(&platform)
                    .join("ide")
                    .join(&destination)
                    .join(&target_name);
                BspTarget::from_invocations(
                    &project_root_uri,
                    manifest_target.kind,
                    &manifest_target.dependencies,
                    BspTargetLocation {
                        platform: &platform,
                        destination: &destination,
                    },
                    &output_root,
                    header_files,
                    invocations,
                )
            })
            .collect::<Result<Vec<_>>>()?;

        let target_id_by_key = targets
            .iter()
            .map(|target| {
                (
                    (target.platform.clone(), target.target_name.clone()),
                    target.id.clone(),
                )
            })
            .collect::<HashMap<_, _>>();
        for target in &mut targets {
            target.dependencies = target
                .dependency_names
                .iter()
                .filter_map(|dependency_name| {
                    target_id_by_key
                        .get(&(target.platform.clone(), dependency_name.clone()))
                        .cloned()
                })
                .collect();
        }

        let targets_by_id = targets
            .iter()
            .cloned()
            .map(|target| (target.id.clone(), target))
            .collect();
        let mut targets_by_source_path = HashMap::<PathBuf, Vec<String>>::new();
        for target in &targets {
            for source_item in &target.source_items {
                targets_by_source_path
                    .entry(source_item.path.clone())
                    .or_default()
                    .push(target.id.clone());
            }
        }
        for target_ids in targets_by_source_path.values_mut() {
            target_ids.sort();
            target_ids.dedup();
        }
        let mut targets_by_dependency_root = BTreeMap::<PathBuf, Vec<String>>::new();
        for target in &targets {
            let manifest_target = targets_by_name
                .get(target.target_name.as_str())
                .copied()
                .with_context(|| {
                    format!(
                        "missing target `{}` in resolved manifest",
                        target.target_name
                    )
                })?;
            for root in target_dependency_watch_roots(project, manifest_target) {
                targets_by_dependency_root
                    .entry(root)
                    .or_default()
                    .push(target.id.clone());
            }
        }
        let mut targets_by_dependency_root =
            targets_by_dependency_root.into_iter().collect::<Vec<_>>();
        for (_, target_ids) in &mut targets_by_dependency_root {
            target_ids.sort();
            target_ids.dedup();
        }
        Ok(Self {
            analysis_project,
            project_root_uri,
            index_store_path: artifact.index_store_path,
            index_database_path: artifact.index_database_path,
            pending_did_change_notification: None,
            targets,
            targets_by_id,
            targets_by_source_path,
            targets_by_dependency_root,
        })
    }

    fn contains_dependency_input_path(&self, path: &Path) -> bool {
        self.targets_by_dependency_root
            .iter()
            .any(|(root, _)| path.starts_with(root))
    }
}

#[derive(Clone, PartialEq, Eq)]
struct BspTarget {
    id: String,
    platform: String,
    destination: String,
    target_name: String,
    project_root_uri: String,
    kind: TargetKind,
    dependency_names: Vec<String>,
    dependencies: Vec<String>,
    language_ids: Vec<String>,
    output_root_uri: String,
    toolchain_uri: String,
    working_directory: String,
    source_items: Vec<BspSourceItem>,
}

#[derive(Clone, PartialEq, Eq)]
struct BspSourceItem {
    path: PathBuf,
    uri: String,
    kind: BspSourceItemKind,
    language_id: Option<String>,
    output_path: Option<String>,
    compiler_arguments: Option<Vec<String>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BspSourceItemKind {
    Source,
    Header,
}

struct BspSourceKitOptions {
    compiler_arguments: Vec<String>,
    language_id: Option<String>,
    index_unit_output_path: Option<String>,
}

struct BspTargetLocation<'a> {
    platform: &'a str,
    destination: &'a str,
}

impl BspTarget {
    fn from_invocations(
        project_root_uri: &str,
        kind: TargetKind,
        dependency_names: &[String],
        location: BspTargetLocation<'_>,
        output_root: &Path,
        header_files: Vec<PathBuf>,
        invocations: Vec<SemanticCompilerInvocation>,
    ) -> Result<Self> {
        let BspTargetLocation {
            platform,
            destination,
        } = location;
        let first_invocation = invocations
            .first()
            .context("BSP target group did not contain any compiler invocations")?;
        let target_name = first_invocation.target.clone();
        let toolchain_uri = file_uri(&first_invocation.toolchain_root)?;
        let working_directory = first_invocation
            .working_directory
            .to_string_lossy()
            .into_owned();
        let id = build_target_uri(platform, &target_name)?;
        let mut language_ids = BTreeSet::new();
        let mut source_items = Vec::new();
        for invocation in invocations {
            language_ids.insert(invocation.language.clone());
            for source_file in invocation.source_files {
                let output_path = if invocation.language == "swift" {
                    index_unit_output_path(&source_file)
                } else {
                    invocation
                        .output_path
                        .clone()
                        .unwrap_or_else(|| index_unit_output_path(&source_file))
                };
                source_items.push(BspSourceItem {
                    uri: file_uri(&source_file)?,
                    path: source_file,
                    kind: BspSourceItemKind::Source,
                    language_id: Some(invocation.language.clone()),
                    output_path: Some(output_path),
                    compiler_arguments: Some(invocation.arguments.clone()),
                });
            }
        }
        for header_file in header_files {
            if source_items.iter().any(|item| item.path == header_file) {
                continue;
            }
            source_items.push(BspSourceItem {
                uri: file_uri(&header_file)?,
                path: header_file,
                kind: BspSourceItemKind::Header,
                language_id: None,
                output_path: None,
                compiler_arguments: None,
            });
        }
        source_items.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(Self {
            id,
            platform: platform.to_owned(),
            destination: destination.to_owned(),
            target_name,
            project_root_uri: project_root_uri.to_owned(),
            kind,
            dependency_names: dependency_names.to_vec(),
            dependencies: Vec::new(),
            language_ids: language_ids.into_iter().collect(),
            output_root_uri: directory_uri(output_root)?,
            toolchain_uri,
            working_directory,
            source_items,
        })
    }

    fn build_target_json(&self) -> Value {
        json!({
            "id": { "uri": self.id },
            "displayName": self.target_name,
            "baseDirectory": self.project_root_uri,
            "tags": [build_target_tag(self.kind)],
            "languageIds": self.language_ids,
            "dependencies": self.dependencies.iter().map(|uri| json!({ "uri": uri })).collect::<Vec<_>>(),
            "capabilities": {
                "canCompile": true,
                "canTest": false,
                "canRun": false,
                "canDebug": false
            },
            "dataKind": "sourceKit",
            "data": {
                "toolchain": self.toolchain_uri
            }
        })
    }

    fn sources_item_json(&self) -> Value {
        json!({
            "target": { "uri": self.id },
            "sources": self.source_items.iter().map(|source| {
                let mut data = json!({});
                if let Some(language_id) = &source.language_id {
                    data["language"] = Value::String(language_id.clone());
                }
                if source.kind == BspSourceItemKind::Header {
                    data["kind"] = Value::String("header".to_owned());
                } else {
                    data["kind"] = Value::String("source".to_owned());
                    if let Some(output_path) = &source.output_path {
                        data["outputPath"] = Value::String(output_path.clone());
                    }
                }
                json!({
                    "uri": source.uri,
                    "kind": 1,
                    "generated": false,
                    "dataKind": "sourceKit",
                    "data": data
                })
            }).collect::<Vec<_>>()
        })
    }

    fn source_item(&self, path: &Path) -> Option<&BspSourceItem> {
        self.source_items.iter().find(|item| item.path == path)
    }

    fn sourcekit_options(
        &self,
        path: &Path,
        requested_language: Option<&str>,
    ) -> Option<BspSourceKitOptions> {
        let source_item = self.source_item(path)?;
        if source_item.kind == BspSourceItemKind::Source {
            let language_id = source_item.language_id.as_deref()?;
            let compiler_arguments = source_item.compiler_arguments.clone()?;
            return Some(BspSourceKitOptions {
                compiler_arguments,
                language_id: Some(language_id.to_owned()),
                index_unit_output_path: source_item.output_path.clone(),
            });
        }

        let substitute = self.header_substitute_source_item(requested_language)?;
        // C-family headers do not have standalone compile commands. Reuse one target-local
        // main file and patch the source path so SourceKit-LSP can open the header with real settings.
        let original_path = &substitute.path;
        let compiler_arguments = patch_compiler_arguments_for_related_file(
            substitute.compiler_arguments.as_ref()?,
            original_path,
            path,
        );
        Some(BspSourceKitOptions {
            compiler_arguments,
            language_id: substitute.language_id.clone(),
            index_unit_output_path: None,
        })
    }

    fn header_substitute_source_item(
        &self,
        requested_language: Option<&str>,
    ) -> Option<&BspSourceItem> {
        let candidates = self
            .source_items
            .iter()
            .filter(|item| {
                item.kind == BspSourceItemKind::Source
                    && item
                        .language_id
                        .as_deref()
                        .is_some_and(is_c_family_language_id)
                    && item.compiler_arguments.is_some()
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        if let Some(requested_language) = requested_language {
            if let Some(candidate) = candidates.iter().copied().find(|item| {
                item.language_id
                    .as_deref()
                    .is_some_and(|language_id| language_id == requested_language)
            }) {
                return Some(candidate);
            }
            if let Some(candidate) = candidates.iter().copied().find(|item| {
                item.language_id.as_deref().is_some_and(|language_id| {
                    same_c_family_driver(language_id, requested_language)
                })
            }) {
                return Some(candidate);
            }
        }
        candidates.into_iter().next()
    }

    fn output_paths_item_json(&self) -> Value {
        json!({
            "target": { "uri": self.id },
            "outputPaths": [
                {
                    "uri": self.output_root_uri,
                    "kind": 2
                }
            ]
        })
    }
}

fn build_target_tag(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Framework => "library",
        _ => "application",
    }
}

fn is_c_family_language_id(language_id: &str) -> bool {
    matches!(language_id, "c" | "objective-c" | "cpp" | "objective-cpp")
}

fn same_c_family_driver(left: &str, right: &str) -> bool {
    matches!(
        (left, right),
        ("c" | "objective-c", "c" | "objective-c")
            | ("cpp" | "objective-cpp", "cpp" | "objective-cpp")
    )
}

fn patch_compiler_arguments_for_related_file(
    arguments: &[String],
    original_file: &Path,
    new_file: &Path,
) -> Vec<String> {
    let mut patched_arguments = arguments.to_vec();
    let Some(original_basename) = original_file.file_name().and_then(|value| value.to_str()) else {
        return patched_arguments;
    };
    let original_path = original_file.to_string_lossy();
    if let Some(index) = patched_arguments.iter().rposition(|argument| {
        argument.ends_with(original_basename) && original_path.ends_with(argument)
    }) {
        patched_arguments[index] = new_file.to_string_lossy().into_owned();
        if let Some(language_flag) = clang_language_flag_for_file(original_file) {
            patched_arguments.insert(0, language_flag.to_owned());
        }
    }
    patched_arguments
}

fn clang_language_flag_for_file(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("c") => Some("-xc"),
        Some("m") => Some("-xobjective-c"),
        Some("mm") => Some("-xobjective-c++"),
        Some("cpp" | "cc" | "cxx") => Some("-xc++"),
        _ => None,
    }
}

fn apple_platform_from_str(value: &str) -> Result<ApplePlatform> {
    match value {
        "ios" => Ok(ApplePlatform::Ios),
        "macos" => Ok(ApplePlatform::Macos),
        "tvos" => Ok(ApplePlatform::Tvos),
        "visionos" => Ok(ApplePlatform::Visionos),
        "watchos" => Ok(ApplePlatform::Watchos),
        other => bail!("unsupported Apple platform `{other}` in BSP snapshot"),
    }
}

fn destination_from_str(value: &str) -> Result<DestinationKind> {
    match value {
        "simulator" => Ok(DestinationKind::Simulator),
        "device" => Ok(DestinationKind::Device),
        other => bail!("unsupported build destination `{other}` in BSP snapshot"),
    }
}

fn build_target_uri(platform: &str, target_name: &str) -> Result<String> {
    let mut url = Url::parse("orbi://target").context("failed to create Orbi build target URI")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("failed to build Orbi build target URI"))?
        .push(platform)
        .push(target_name);
    Ok(url.to_string())
}

fn target_ids_from_params(params: &Value) -> Result<Vec<String>> {
    let targets = params
        .get("targets")
        .and_then(Value::as_array)
        .context("missing `targets` array in `buildTarget/sources` params")?;
    targets
        .iter()
        .map(|target| {
            target
                .get("uri")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .context("missing `targets[*].uri` in `buildTarget/sources` params")
        })
        .collect()
}

fn file_uri(path: &Path) -> Result<String> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| anyhow::anyhow!("failed to convert `{}` to a file URI", path.display()))
}

fn directory_uri(path: &Path) -> Result<String> {
    Url::from_directory_path(path)
        .map(|url| url.to_string())
        .map_err(|_| anyhow::anyhow!("failed to convert `{}` to a directory URI", path.display()))
}

fn file_path_from_uri(uri: &str) -> Result<PathBuf> {
    Url::parse(uri)
        .with_context(|| format!("failed to parse document URI `{uri}`"))?
        .to_file_path()
        .map_err(|_| anyhow::anyhow!("document URI `{uri}` is not a file URI"))
}

fn index_unit_output_path(path: &Path) -> String {
    format!("{}.o", path.to_string_lossy())
}

fn build_watchers() -> Vec<Value> {
    let watch_kind = 1 + 2 + 4;
    [
        "orbi.json",
        "**/Package.swift",
        "**/Package.resolved",
        "**/.swift-format",
        "**/.swift-format.json",
        "**/.editorconfig",
        "**/*.swift",
        "**/*.c",
        "**/*.m",
        "**/*.mm",
        "**/*.cpp",
        "**/*.cc",
        "**/*.cxx",
        "**/*.h",
        "**/*.hh",
        "**/*.hpp",
        "**/*.hxx",
        "**/*.xcframework",
        "**/*.xcframework/**",
    ]
    .into_iter()
    .map(|glob_pattern| {
        json!({
            "globPattern": glob_pattern,
            "kind": watch_kind
        })
    })
    .collect()
}

fn ok_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

fn response_for_result(id: Option<Value>, result: Result<Value>) -> Option<Value> {
    id.map(|id| match result {
        Ok(result) => ok_response(id, result),
        Err(error) => error_response(id, -32603, format!("{error:#}")),
    })
}

fn build_target_did_change_notification_with_changed_files(
    previous: Option<&BspSnapshot>,
    current: Option<&BspSnapshot>,
    changed_files: &[PathBuf],
) -> Option<Value> {
    let changes = build_target_change_events(previous, current, changed_files);
    if changes.is_empty() {
        return None;
    }
    Some(json!({
        "jsonrpc": "2.0",
        "method": "buildTarget/didChange",
        "params": {
            "changes": changes
        }
    }))
}

fn build_target_change_events(
    previous: Option<&BspSnapshot>,
    current: Option<&BspSnapshot>,
    changed_files: &[PathBuf],
) -> Vec<Value> {
    let previous_targets = previous
        .map(|snapshot| &snapshot.targets_by_id)
        .into_iter()
        .flat_map(|targets| targets.keys().cloned())
        .collect::<BTreeSet<_>>();
    let current_targets = current
        .map(|snapshot| &snapshot.targets_by_id)
        .into_iter()
        .flat_map(|targets| targets.keys().cloned())
        .collect::<BTreeSet<_>>();
    let mut events = BTreeMap::<String, i64>::new();
    for target_id in previous_targets.union(&current_targets) {
        match (
            previous.and_then(|snapshot| snapshot.targets_by_id.get(target_id)),
            current.and_then(|snapshot| snapshot.targets_by_id.get(target_id)),
        ) {
            (None, Some(_)) => {
                events.insert(target_id.clone(), 1);
            }
            (Some(_), None) => {
                events.insert(target_id.clone(), 3);
            }
            (Some(previous_target), Some(current_target)) if previous_target != current_target => {
                events.insert(target_id.clone(), 2);
            }
            _ => {}
        }
    }

    for changed_file in changed_files {
        for target_id in targets_for_changed_file(previous, current, changed_file) {
            events.entry(target_id).or_insert(2);
        }
    }

    events
        .into_iter()
        .map(|(target_id, kind)| {
            json!({
                "target": { "uri": target_id },
                "kind": kind
            })
        })
        .collect()
}

fn targets_for_changed_file(
    previous: Option<&BspSnapshot>,
    current: Option<&BspSnapshot>,
    changed_file: &Path,
) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for snapshot in [previous, current].into_iter().flatten() {
        if let Some(target_ids) = snapshot.targets_by_source_path.get(changed_file) {
            targets.extend(target_ids.iter().cloned());
        }
        for (root, target_ids) in &snapshot.targets_by_dependency_root {
            if changed_file.starts_with(root) {
                targets.extend(target_ids.iter().cloned());
            }
        }
    }
    targets
}

fn watched_file_changes_from_params(params: &Value) -> Vec<WatchedFileChange> {
    params
        .get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| {
            let path = change
                .get("uri")
                .and_then(Value::as_str)
                .and_then(|uri| file_path_from_uri(uri).ok())?;
            let kind = match change.get("type").and_then(Value::as_i64) {
                Some(1) => WatchedFileChangeKind::Created,
                Some(2) => WatchedFileChangeKind::Changed,
                Some(3) => WatchedFileChangeKind::Deleted,
                _ => WatchedFileChangeKind::Unknown,
            };
            Some(WatchedFileChange { path, kind })
        })
        .collect()
}

fn watched_file_change_affects_build_graph(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("orbi.json" | "Package.swift" | "Package.resolved")
    )
}

fn watched_file_change_is_quality_only(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(".swift-format" | ".swift-format.json" | ".editorconfig")
    )
}

fn watched_file_change_is_source_like(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("swift")
                || C_FAMILY_SOURCE_EXTENSIONS
                    .iter()
                    .chain(C_FAMILY_HEADER_EXTENSIONS.iter())
                    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
        })
}

fn origin_id_from_params(params: &Value) -> Option<String> {
    params
        .get("originId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn structured_log_payload(kind: &str, title: Option<&str>) -> Value {
    let mut payload = json!({ "kind": kind });
    if let Some(title) = title {
        payload["title"] = Value::String(title.to_owned());
    }
    payload
}

fn read_jsonrpc_message<R: BufRead>(reader: &mut R) -> Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .context("failed to read JSON-RPC header line")?;
        if read == 0 {
            if content_length.is_none() {
                return Ok(None);
            }
            bail!("unexpected EOF while reading JSON-RPC headers");
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some(value) = line
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("Content-Length:")
        {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .context("invalid Content-Length header")?,
            );
        }
    }

    let content_length = content_length.context("missing Content-Length header")?;
    let mut body = vec![0_u8; content_length];
    reader
        .read_exact(&mut body)
        .context("failed to read JSON-RPC body")?;
    serde_json::from_slice(&body).context("failed to parse JSON-RPC body")
}

fn write_jsonrpc_message<W: Write>(writer: &mut W, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message).context("failed to encode JSON-RPC body")?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())
        .context("failed to write JSON-RPC header")?;
    writer
        .write_all(&body)
        .context("failed to write JSON-RPC body")?;
    writer.flush().context("failed to flush JSON-RPC response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::read_json_file;

    #[test]
    fn install_connection_file_for_manifest_writes_standard_bsp_file() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("orbi.json");
        std::fs::write(&manifest_path, "{}").unwrap();

        let standard_path = install_connection_file_for_manifest(&manifest_path).unwrap();

        assert_eq!(
            standard_path,
            manifest_path
                .canonicalize()
                .unwrap()
                .parent()
                .unwrap()
                .join(".bsp/orbi.json")
        );

        let details: Value = read_json_file(&standard_path).unwrap();
        assert_eq!(details["name"], "orbi");
        assert_eq!(details["bspVersion"], BSP_VERSION);
        assert_eq!(details["argv"][1], "--manifest");
        assert_eq!(
            details["argv"][2],
            manifest_path.canonicalize().unwrap().display().to_string()
        );
        assert_eq!(details["argv"][3], "bsp");
    }

    #[test]
    fn install_connection_file_with_env_writes_env_into_bsp_file() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("orbi.json");
        std::fs::write(&manifest_path, "{}").unwrap();

        let standard_path =
            install_connection_file_for_manifest_with_env(&manifest_path, Some("stage")).unwrap();

        let details: Value = read_json_file(&standard_path).unwrap();
        assert_eq!(details["argv"][3], "--env");
        assert_eq!(details["argv"][4], "stage");
        assert_eq!(details["argv"][5], "bsp");
    }
}
