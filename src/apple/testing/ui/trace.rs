use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use super::UiLaunchApp;
use super::backend::UiBackend;
use crate::apple::build::pipeline::{
    prepare_macos_trace_launch_executable, stop_existing_macos_application,
};
use crate::apple::build::receipt::BuildReceipt;
use crate::apple::profile::{
    TraceRecording, finish_started_trace, start_optional_launched_command_trace,
};
use crate::apple::xcode::SelectedXcode;
use crate::cli::ProfileKind;
use crate::context::ProjectPaths;

pub(super) struct MacosUiTraceRuntime {
    root: PathBuf,
    project_paths: ProjectPaths,
    selected_xcode: Option<SelectedXcode>,
    receipt: BuildReceipt,
    kind: ProfileKind,
    interactive: bool,
    active_recording: Option<TraceRecording>,
}

impl MacosUiTraceRuntime {
    pub(super) fn new(
        root: PathBuf,
        project_paths: &ProjectPaths,
        selected_xcode: Option<SelectedXcode>,
        receipt: &BuildReceipt,
        kind: ProfileKind,
        interactive: bool,
    ) -> Self {
        Self {
            root,
            project_paths: project_paths.clone(),
            selected_xcode,
            receipt: receipt.clone(),
            kind,
            interactive,
            active_recording: None,
        }
    }

    pub(super) fn launch_app(
        &mut self,
        backend: &dyn UiBackend,
        launch: &UiLaunchApp,
    ) -> Result<()> {
        if let Some(path) = self.finish_active()? {
            println!("trace: {}", path.display());
        }

        apply_trace_launch_preconditions(backend, &self.receipt.bundle_id, launch)?;
        stop_existing_macos_application(&self.receipt)?;

        let previous_frontmost_pid = backend.frontmost_application_pid()?;
        let launch_environment =
            backend.prepare_trace_launch_environment(previous_frontmost_pid)?;
        let launch_target = prepare_macos_ui_trace_launch_command(
            &self.project_paths,
            &self.receipt,
            Some(launch),
        )?;
        let (_, recording) = start_optional_launched_command_trace(
            &self.root,
            self.selected_xcode.as_ref(),
            self.interactive,
            Some(self.kind),
            launch_target.as_slice(),
            launch_environment.as_slice(),
            None,
        )?
        .expect("trace kind should produce a launched trace");
        let prepare_result = (|| {
            backend.pin_pending_trace_launch()?;
            backend.prepare_external_running_target()
        })();
        if let Err(error) = prepare_result {
            let _ = backend.abort_pending_trace_launch();
            let _ = finish_started_trace(self.kind, recording);
            return Err(error);
        }
        self.active_recording = Some(recording);
        Ok(())
    }

    pub(super) fn finish_active(&mut self) -> Result<Option<PathBuf>> {
        let Some(recording) = self.active_recording.take() else {
            return Ok(None);
        };
        let path = finish_started_trace(self.kind, recording)
            .with_context(|| format!("failed to finalize {} trace", self.kind.trace_label()))?;
        Ok(Some(path))
    }
}

fn apply_trace_launch_preconditions(
    backend: &dyn UiBackend,
    bundle_id: &str,
    launch: &UiLaunchApp,
) -> Result<()> {
    let launch_bundle_id =
        resolve_trace_bundle_id(bundle_id, launch.app_id.as_deref(), "launchApp")?;
    if launch.clear_keychain {
        backend.clear_keychain()?;
    }
    if launch.clear_state {
        backend.clear_app_state(launch_bundle_id)?;
    }
    if let Some(permissions) = launch.permissions.as_ref() {
        backend.set_permissions(
            resolve_trace_bundle_id(
                launch_bundle_id,
                permissions.app_id.as_deref(),
                "launchApp.permissions",
            )?,
            permissions,
        )?;
    }
    Ok(())
}

pub(super) fn prepare_macos_ui_trace_launch_command(
    project_paths: &ProjectPaths,
    receipt: &BuildReceipt,
    launch: Option<&UiLaunchApp>,
) -> Result<Vec<String>> {
    let mut command = vec![prepare_macos_trace_launch_executable(
        project_paths,
        receipt,
    )?];
    if let Some(launch) = launch {
        for (key, value) in &launch.arguments {
            command.push(format!("-{key}"));
            command.push(value.clone());
        }
    }
    Ok(command)
}

pub(super) fn resolve_trace_bundle_id<'a>(
    bundle_id: &'a str,
    requested: Option<&'a str>,
    source: &str,
) -> Result<&'a str> {
    if let Some(requested) = requested
        && requested != bundle_id
    {
        bail!(
            "{source} targets `{requested}`, but traced UI runs currently support only Orbi's built app `{bundle_id}`"
        );
    }
    Ok(requested.unwrap_or(bundle_id))
}
