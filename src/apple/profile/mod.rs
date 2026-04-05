mod export;
mod inspect;
mod recording;

use crate::cli::ProfileKind;

pub use self::inspect::inspect_trace_command;
pub(crate) use self::recording::{
    TraceRecording, ensure_simulator_profiling_supported, finish_started_trace,
    start_optional_launched_command_trace, start_optional_launched_process_trace,
    trace_recording_process_id, wait_for_launched_trace_exit,
};

impl ProfileKind {
    pub(crate) fn trace_template(self) -> &'static str {
        match self {
            Self::Cpu => "Time Profiler",
            Self::Memory => "Allocations",
        }
    }

    pub(crate) fn trace_label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "memory",
        }
    }

    pub(crate) fn trace_slug(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "memory",
        }
    }
}
