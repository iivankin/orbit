use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};

use super::export::{TraceExportMode, capture_xctrace_export, debug_export_command};
use crate::cli::InspectTraceArgs;
use crate::context::AppContext;
use crate::util::resolve_path;

const XPATH_TIME_PROFILE: &str =
    r#"/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]"#;
const XPATH_ALLOCATIONS_STATISTICS: &str =
    r#"/trace-toc/run/tracks/track[@name="Allocations"]/details/detail[@name="Statistics"]"#;
const XPATH_ALLOCATIONS_LIST: &str =
    r#"/trace-toc/run/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]"#;
const DIAGNOSIS_THRESHOLD_PERCENT: f64 = 1.0;
const DIAGNOSIS_STACK_DEPTH: usize = 5;
const DIAGNOSIS_MAX_ITEMS: usize = 10;

#[derive(Debug, Clone)]
struct TraceMetadata {
    process_name: String,
    process_path: Option<String>,
    duration_s: f64,
    template_name: String,
    device_platform: Option<String>,
    device_name: Option<String>,
    has_time_profile_table: bool,
    has_allocations_track: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FrameKey {
    binary_name: String,
    name: String,
}

#[derive(Debug, Clone)]
struct TraceFrame {
    key: FrameKey,
    is_user: bool,
    is_symbolicated: bool,
}

#[derive(Debug, Clone)]
struct TraceSample {
    weight_ns: u64,
    frames: Vec<TraceFrame>,
}

#[derive(Debug, Default)]
struct TimeProfileSummary {
    total_weight_ns: u64,
    sample_count: usize,
    unsymbolicated_user_weight_ns: u64,
    self_time: HashMap<FrameKey, u64>,
    total_time: HashMap<FrameKey, u64>,
    stack_time: HashMap<Vec<FrameKey>, u64>,
}

#[derive(Debug, Clone)]
struct AllocationStatRow {
    category: String,
    persistent_bytes: u64,
    transient_bytes: u64,
    total_bytes: u64,
    count_persistent: u64,
    count_transient: u64,
    count_events: u64,
}

#[derive(Debug, Clone)]
struct AllocationListRow {
    size_bytes: u64,
    live: bool,
    responsible_caller: Option<String>,
    responsible_library: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AllocationCallerKey {
    library: String,
    caller: String,
}

pub fn inspect_trace_command(app: &AppContext, args: &InspectTraceArgs) -> Result<()> {
    let trace_path = resolve_path(&app.cwd, &args.trace);
    let toc_debug = debug_export_command(&trace_path, None, TraceExportMode::Toc);
    let toc_xml = capture_xctrace_export(&trace_path, None, TraceExportMode::Toc, &toc_debug)?;
    let metadata = parse_trace_metadata(&toc_xml)?;

    if metadata.has_time_profile_table {
        let profile_debug = debug_export_command(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_TIME_PROFILE),
        );
        let profile_xml = capture_xctrace_export(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_TIME_PROFILE),
            &profile_debug,
        )?;
        let samples = parse_time_profile_samples(&profile_xml, &metadata)?;
        print!("{}", render_time_profile_diagnosis(&metadata, &samples));
        return Ok(());
    }

    if metadata.has_allocations_track {
        let statistics_debug = debug_export_command(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_STATISTICS),
        );
        let statistics_xml = capture_xctrace_export(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_STATISTICS),
            &statistics_debug,
        )?;
        let allocations_debug = debug_export_command(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_LIST),
        );
        let allocations_xml = capture_xctrace_export(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_LIST),
            &allocations_debug,
        )?;
        let stats = parse_allocations_statistics(&statistics_xml)?;
        let rows = parse_allocations_list(&allocations_xml)?;
        print!("{}", render_allocations_diagnosis(&metadata, &stats, &rows));
        return Ok(());
    }

    let template = display_field(&metadata.template_name);
    bail!(
        "inspect-trace currently supports Time Profiler and Allocations traces only; trace template: {template}"
    );
}

fn parse_trace_metadata(toc_xml: &str) -> Result<TraceMetadata> {
    let document = Document::parse(toc_xml).context("failed to parse xctrace TOC XML")?;
    let root = document.root_element();
    if !root.has_tag_name("trace-toc") {
        bail!(
            "unexpected xctrace TOC XML root: {}",
            root.tag_name().name()
        );
    }

    let run = root
        .children()
        .find(|node| node.has_tag_name("run"))
        .context("trace TOC did not contain a run")?;
    let info = child_element(run, "info").context("trace TOC run did not contain info")?;
    let target = child_element(info, "target").context("trace TOC run did not contain target")?;
    let summary =
        child_element(info, "summary").context("trace TOC run did not contain summary")?;

    let process = child_element(target, "process");
    let process_name = process
        .and_then(|node| node.attribute("name"))
        .unwrap_or_default()
        .to_owned();
    let duration_s = child_text(summary, "duration")
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or_default();
    let template_name = child_text(summary, "template-name")
        .unwrap_or_default()
        .to_owned();
    let device = child_element(target, "device");
    let device_platform = device
        .and_then(|node| node.attribute("platform"))
        .map(str::to_owned);
    let device_name = device
        .and_then(|node| node.attribute("name"))
        .map(str::to_owned);

    let processes = child_element(run, "processes");
    let process_path = processes.and_then(|processes_node| {
        processes_node
            .children()
            .find(|node| {
                node.has_tag_name("process")
                    && node.attribute("name") == Some(process_name.as_str())
                    && node.attribute("path").is_some()
            })
            .and_then(|node| node.attribute("path"))
            .map(str::to_owned)
            .or_else(|| {
                processes_node
                    .children()
                    .find(|node| {
                        node.has_tag_name("process")
                            && node.attribute("name") != Some("kernel")
                            && node.attribute("path").is_some()
                    })
                    .and_then(|node| node.attribute("path"))
                    .map(str::to_owned)
            })
    });

    let has_time_profile_table = run
        .descendants()
        .any(|node| node.has_tag_name("table") && node.attribute("schema") == Some("time-profile"));
    let has_allocations_track = run
        .descendants()
        .any(|node| node.has_tag_name("track") && node.attribute("name") == Some("Allocations"));

    Ok(TraceMetadata {
        process_name,
        process_path,
        duration_s,
        template_name,
        device_platform,
        device_name,
        has_time_profile_table,
        has_allocations_track,
    })
}

fn parse_time_profile_samples(xml: &str, metadata: &TraceMetadata) -> Result<Vec<TraceSample>> {
    let document = Document::parse(xml).context("failed to parse xctrace time-profile XML")?;
    let mut registry = HashMap::new();
    for node in document.descendants().filter(|node| node.is_element()) {
        if let Some(id) = node.attribute("id") {
            registry.insert(id.to_owned(), node);
        }
    }

    let mut samples = Vec::new();
    for row in document
        .descendants()
        .filter(|node| node.has_tag_name("row"))
    {
        let Some(backtrace) = resolve_row_backtrace(row, &registry) else {
            continue;
        };
        let weight_ns = resolve_row_weight(row, &registry);
        let frames = extract_frames(backtrace, &registry, metadata);
        samples.push(TraceSample { weight_ns, frames });
    }
    Ok(samples)
}

fn render_time_profile_diagnosis(metadata: &TraceMetadata, samples: &[TraceSample]) -> String {
    let summary = summarize_time_profile(samples);
    let mut output = String::new();

    render_trace_header(&mut output, metadata);
    let _ = writeln!(
        output,
        "Samples: {}  Total CPU: {:.0}ms",
        summary.sample_count,
        summary.total_weight_ns as f64 / 1_000_000.0
    );

    if summary.total_weight_ns > 0 && summary.unsymbolicated_user_weight_ns > 0 {
        let unsymbolicated_pct =
            100.0 * summary.unsymbolicated_user_weight_ns as f64 / summary.total_weight_ns as f64;
        let _ = writeln!(
            output,
            "Note: {:.0}% of user samples are unsymbolicated or runtime-only",
            unsymbolicated_pct
        );
    }

    output.push('\n');

    if summary.self_time.is_empty() {
        output.push_str("No symbolicated user frames found.\n");
        return output;
    }

    output.push_str("SELF TIME\n");
    for (frame, weight_ns) in top_frame_weights(&summary.self_time) {
        let pct = 100.0 * weight_ns as f64 / summary.total_weight_ns as f64;
        if pct < DIAGNOSIS_THRESHOLD_PERCENT {
            break;
        }
        let _ = writeln!(
            output,
            "  {:5.1}%  {:6.0}ms  {}  {}",
            pct,
            weight_ns as f64 / 1_000_000.0,
            frame.binary_name,
            frame.name
        );
    }

    let mut callers = top_frame_weights(&summary.total_time)
        .into_iter()
        .filter(|(frame, total_weight)| {
            let self_weight = summary.self_time.get(frame).copied().unwrap_or_default();
            *total_weight > self_weight.saturating_mul(11) / 10
        })
        .collect::<Vec<_>>();
    if !callers.is_empty() {
        output.push('\n');
        output.push_str("TOTAL TIME (callers with significant overhead)\n");
        callers.truncate(DIAGNOSIS_MAX_ITEMS);
        for (frame, weight_ns) in callers {
            let pct = 100.0 * weight_ns as f64 / summary.total_weight_ns as f64;
            if pct < DIAGNOSIS_THRESHOLD_PERCENT {
                break;
            }
            let _ = writeln!(
                output,
                "  {:5.1}%  {:6.0}ms  {}  {}",
                pct,
                weight_ns as f64 / 1_000_000.0,
                frame.binary_name,
                frame.name
            );
        }
    }

    output.push('\n');
    output.push_str("CALL STACKS\n");
    let mut stacks = summary
        .stack_time
        .iter()
        .map(|(stack, weight)| (stack, *weight))
        .collect::<Vec<_>>();
    stacks.sort_by(|left, right| right.1.cmp(&left.1));
    for (stack, weight_ns) in stacks.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
        let pct = 100.0 * weight_ns as f64 / summary.total_weight_ns as f64;
        if pct < DIAGNOSIS_THRESHOLD_PERCENT {
            break;
        }
        let chain = stack
            .iter()
            .rev()
            .map(|frame| frame.name.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        let _ = writeln!(
            output,
            "  {:5.1}%  {:6.0}ms  {}",
            pct,
            weight_ns as f64 / 1_000_000.0,
            chain
        );
    }

    output
}

fn parse_allocations_statistics(xml: &str) -> Result<Vec<AllocationStatRow>> {
    let document =
        Document::parse(xml).context("failed to parse xctrace allocations statistics XML")?;
    let mut rows = Vec::new();
    for row in document
        .descendants()
        .filter(|node| node.has_tag_name("row"))
    {
        rows.push(AllocationStatRow {
            category: row.attribute("category").unwrap_or("<unknown>").to_owned(),
            persistent_bytes: parse_u64_attribute(row, "persistent-bytes"),
            transient_bytes: parse_u64_attribute(row, "transient-bytes"),
            total_bytes: parse_u64_attribute(row, "total-bytes"),
            count_persistent: parse_u64_attribute(row, "count-persistent"),
            count_transient: parse_u64_attribute(row, "count-transient"),
            count_events: parse_u64_attribute(row, "count-events"),
        });
    }
    Ok(rows)
}

fn parse_allocations_list(xml: &str) -> Result<Vec<AllocationListRow>> {
    let document = Document::parse(xml).context("failed to parse xctrace allocations list XML")?;
    let mut rows = Vec::new();
    for row in document
        .descendants()
        .filter(|node| node.has_tag_name("row"))
    {
        rows.push(AllocationListRow {
            size_bytes: parse_u64_attribute(row, "size"),
            live: row.attribute("live") == Some("true"),
            responsible_caller: row
                .attribute("responsible-caller")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            responsible_library: row
                .attribute("responsible-library")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        });
    }
    Ok(rows)
}

fn render_allocations_diagnosis(
    metadata: &TraceMetadata,
    stats: &[AllocationStatRow],
    rows: &[AllocationListRow],
) -> String {
    let mut output = String::new();
    render_trace_header(&mut output, metadata);

    let overall = stats
        .iter()
        .find(|row| row.category == "All Heap & Anonymous VM")
        .or_else(|| {
            stats
                .iter()
                .find(|row| row.category == "All Heap Allocations")
        })
        .or_else(|| stats.iter().find(|row| row.category == "All VM Regions"));
    if let Some(overall) = overall {
        let _ = writeln!(
            output,
            "Live bytes: {}  Live allocations: {}",
            format_bytes(overall.persistent_bytes),
            overall.count_persistent
        );
        let _ = writeln!(
            output,
            "Transient bytes: {}  Transient allocations: {}",
            format_bytes(overall.transient_bytes),
            overall.count_transient
        );
        let _ = writeln!(
            output,
            "Total bytes: {}  Allocation events: {}",
            format_bytes(overall.total_bytes),
            overall.count_events
        );
    }

    let live_rows = stats
        .iter()
        .filter(|row| !is_aggregate_allocation_category(&row.category) && row.persistent_bytes > 0)
        .collect::<Vec<_>>();
    let transient_rows = stats
        .iter()
        .filter(|row| !is_aggregate_allocation_category(&row.category) && row.transient_bytes > 0)
        .collect::<Vec<_>>();

    output.push('\n');
    output.push_str("LIVE BY CATEGORY\n");
    if live_rows.is_empty() {
        output.push_str("  No live allocation categories found.\n");
    } else {
        let mut live_rows = live_rows;
        live_rows.sort_by(|left, right| {
            right
                .persistent_bytes
                .cmp(&left.persistent_bytes)
                .then_with(|| left.category.cmp(&right.category))
        });
        for row in live_rows.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
            let _ = writeln!(
                output,
                "  {:>10}  {:>6}  {}",
                format_bytes(row.persistent_bytes),
                row.count_persistent,
                row.category
            );
        }
    }

    output.push('\n');
    output.push_str("TRANSIENT BY CATEGORY\n");
    if transient_rows.is_empty() {
        output.push_str("  No transient allocation categories found.\n");
    } else {
        let mut transient_rows = transient_rows;
        transient_rows.sort_by(|left, right| {
            right
                .transient_bytes
                .cmp(&left.transient_bytes)
                .then_with(|| left.category.cmp(&right.category))
        });
        for row in transient_rows.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
            let _ = writeln!(
                output,
                "  {:>10}  {:>6}  {}",
                format_bytes(row.transient_bytes),
                row.count_transient,
                row.category
            );
        }
    }

    let mut caller_totals = HashMap::<AllocationCallerKey, (u64, u64)>::new();
    for row in rows.iter().filter(|row| row.live) {
        let Some(caller) = row
            .responsible_caller
            .as_deref()
            .filter(|caller| allocation_caller_is_useful(caller))
        else {
            continue;
        };
        let key = AllocationCallerKey {
            library: row
                .responsible_library
                .clone()
                .filter(|library| !library.is_empty())
                .unwrap_or_else(|| metadata.process_name.clone()),
            caller: caller.to_owned(),
        };
        let entry = caller_totals.entry(key).or_insert((0, 0));
        entry.0 += row.size_bytes;
        entry.1 += 1;
    }

    if !caller_totals.is_empty() {
        output.push('\n');
        output.push_str("LIVE BY RESPONSIBLE CALLER\n");
        let mut caller_totals = caller_totals.into_iter().collect::<Vec<_>>();
        caller_totals.sort_by(|left, right| {
            right
                .1
                .0
                .cmp(&left.1.0)
                .then_with(|| left.0.library.cmp(&right.0.library))
                .then_with(|| left.0.caller.cmp(&right.0.caller))
        });
        for (key, (bytes, count)) in caller_totals.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
            let _ = writeln!(
                output,
                "  {:>10}  {:>6}  {}  {}",
                format_bytes(bytes),
                count,
                key.library,
                key.caller
            );
        }
    }

    output
}

fn render_trace_header(output: &mut String, metadata: &TraceMetadata) {
    let _ = writeln!(
        output,
        "Process: {}  Duration: {:.1}s  Template: {}",
        display_field(&metadata.process_name),
        metadata.duration_s,
        display_field(&metadata.template_name)
    );
    if let (Some(platform), Some(device_name)) = (
        metadata.device_platform.as_deref(),
        metadata.device_name.as_deref(),
    ) {
        let _ = writeln!(output, "Target: {platform}  {device_name}");
    }
}

fn display_field(value: &str) -> &str {
    if value.is_empty() { "<unknown>" } else { value }
}

fn summarize_time_profile(samples: &[TraceSample]) -> TimeProfileSummary {
    let mut summary = TimeProfileSummary {
        sample_count: samples.len(),
        total_weight_ns: samples.iter().map(|sample| sample.weight_ns).sum(),
        ..TimeProfileSummary::default()
    };

    for sample in samples {
        let user_frames = sample
            .frames
            .iter()
            .filter(|frame| frame.is_user)
            .collect::<Vec<_>>();
        if user_frames.is_empty() {
            continue;
        }

        let useful_frames = user_frames
            .iter()
            .copied()
            .filter(|frame| frame.is_symbolicated && !looks_like_runtime_internal(&frame.key.name))
            .collect::<Vec<_>>();
        if useful_frames.is_empty() {
            summary.unsymbolicated_user_weight_ns += sample.weight_ns;
            continue;
        }

        *summary
            .self_time
            .entry(useful_frames[0].key.clone())
            .or_default() += sample.weight_ns;

        let mut seen = HashSet::new();
        for frame in &useful_frames {
            if seen.insert(frame.key.clone()) {
                *summary.total_time.entry(frame.key.clone()).or_default() += sample.weight_ns;
            }
        }

        let stack_key = useful_frames
            .iter()
            .take(DIAGNOSIS_STACK_DEPTH)
            .map(|frame| frame.key.clone())
            .collect::<Vec<_>>();
        *summary.stack_time.entry(stack_key).or_default() += sample.weight_ns;
    }

    summary
}

fn top_frame_weights(weights: &HashMap<FrameKey, u64>) -> Vec<(FrameKey, u64)> {
    let mut items = weights
        .iter()
        .map(|(frame, weight)| (frame.clone(), *weight))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.binary_name.cmp(&right.0.binary_name))
            .then_with(|| left.0.name.cmp(&right.0.name))
    });
    items
}

fn parse_u64_attribute<'a, 'input>(node: Node<'a, 'input>, attribute: &str) -> u64 {
    node.attribute(attribute)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
}

fn is_aggregate_allocation_category(category: &str) -> bool {
    category == "destroyed event" || category.starts_with("All ")
}

fn allocation_caller_is_useful(caller: &str) -> bool {
    let caller = caller.trim();
    !caller.is_empty()
        && caller != "<Call stack limit reached>"
        && caller != "<unknown>"
        && !looks_like_runtime_internal(caller)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{bytes} {}", UNITS[unit_index])
    } else {
        format!("{value:.1} {}", UNITS[unit_index])
    }
}

fn resolve_row_weight<'a, 'input>(
    row: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
) -> u64 {
    child_element(row, "weight")
        .map(|weight| resolve_ref(weight, registry))
        .and_then(|weight| weight.text())
        .and_then(|weight| weight.parse::<u64>().ok())
        .unwrap_or(1_000_000)
}

fn resolve_row_backtrace<'a, 'input>(
    row: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
) -> Option<Node<'a, 'input>> {
    let tagged_backtrace = child_element(row, "tagged-backtrace")
        .or_else(|| child_element(row, "backtrace"))
        .map(|node| resolve_ref(node, registry))?;
    if tagged_backtrace.has_tag_name("backtrace") {
        return Some(tagged_backtrace);
    }
    child_element(tagged_backtrace, "backtrace").map(|node| resolve_ref(node, registry))
}

fn extract_frames<'a, 'input>(
    backtrace: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
    metadata: &TraceMetadata,
) -> Vec<TraceFrame> {
    backtrace
        .children()
        .filter(|node| node.has_tag_name("frame"))
        .map(|frame| resolve_ref(frame, registry))
        .map(|frame| build_frame(frame, registry, metadata))
        .collect()
}

fn build_frame<'a, 'input>(
    frame: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
    metadata: &TraceMetadata,
) -> TraceFrame {
    let frame_name = frame.attribute("name").unwrap_or("<unknown>").to_owned();
    let address = frame.attribute("addr").map(str::to_owned);
    let binary = child_element(frame, "binary").map(|binary| resolve_ref(binary, registry));
    let binary_path = binary
        .and_then(|binary| binary.attribute("path"))
        .map(str::to_owned);
    let binary_name = binary
        .and_then(|binary| binary.attribute("name"))
        .map(str::to_owned)
        .or_else(|| {
            classify_user_frame(
                &frame_name,
                address.as_deref(),
                binary_path.as_deref(),
                metadata,
            )
            .then(|| metadata.process_name.clone())
        })
        .unwrap_or_else(|| "<unknown>".to_owned());
    let is_symbolicated = !frame_name.starts_with("0x")
        && frame_name != "<deduplicated_symbol>"
        && frame_name != "<unknown>";
    let is_user = classify_user_frame(
        &frame_name,
        address.as_deref(),
        binary_path.as_deref(),
        metadata,
    );

    TraceFrame {
        key: FrameKey {
            binary_name,
            name: frame_name,
        },
        is_user,
        is_symbolicated,
    }
}

fn classify_user_frame(
    frame_name: &str,
    address: Option<&str>,
    binary_path: Option<&str>,
    metadata: &TraceMetadata,
) -> bool {
    if let Some(binary_path) = binary_path {
        if is_system_binary_path(binary_path) {
            return false;
        }
        if let Some(process_path) = metadata.process_path.as_deref() {
            if binary_path == process_path {
                return true;
            }
            if let Some(bundle_root) = bundle_root(process_path)
                && binary_path.starts_with(bundle_root)
            {
                return true;
            }
        }
        if binary_path.contains(".app/") || binary_path.ends_with(".app") {
            return true;
        }
        if binary_path.contains("/DerivedData/") || binary_path.contains("/Build/Products/") {
            return true;
        }
        return !binary_path.is_empty();
    }

    let address_like = address.unwrap_or(frame_name);
    looks_like_user_address(address_like)
}

fn bundle_root(path: &str) -> Option<&str> {
    path.find(".app").map(|index| &path[..index + 4])
}

fn is_system_binary_path(path: &str) -> bool {
    path.starts_with("/System/")
        || path.starts_with("/usr/lib/")
        || path.contains("/System/Library/")
        || path.contains("/usr/lib/system/")
        || path.contains("/Symbols/System/")
        || path.contains("/Symbols/usr/lib/")
}

fn looks_like_user_address(value: &str) -> bool {
    value.starts_with("0x10") || value.starts_with("0x11") || value.starts_with("0x12")
}

fn looks_like_runtime_internal(name: &str) -> bool {
    name.starts_with("__swift_")
        || name.starts_with("_swift_")
        || name.starts_with("swift_")
        || name.starts_with("__objc_")
        || name.starts_with("DYLD-STUB$$")
}

fn resolve_ref<'a, 'input>(
    node: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
) -> Node<'a, 'input> {
    node.attribute("ref")
        .and_then(|reference| registry.get(reference).copied())
        .unwrap_or(node)
}

fn child_element<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name(name))
}

fn child_text<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<String> {
    child_element(node, name)
        .and_then(|child| child.text())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_allocations_list, parse_allocations_statistics, parse_time_profile_samples,
        parse_trace_metadata, render_allocations_diagnosis, render_time_profile_diagnosis,
    };

    const SAMPLE_TOC_XML: &str = r#"<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" name="Example Mac"/>
        <process name="Orbit"/>
      </target>
      <summary>
        <duration>6.0</duration>
        <template-name>Time Profiler</template-name>
      </summary>
    </info>
    <processes>
      <process name="Orbit" path="/Applications/Orbit.app/Contents/MacOS/Orbit"/>
    </processes>
    <data>
      <table schema="time-profile"/>
    </data>
  </run>
</trace-toc>"#;

    const SAMPLE_TIME_PROFILE_XML: &str = r#"<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/data[1]/table[1]'>
    <schema name="time-profile"/>
    <row>
      <weight id="1">3000000</weight>
      <tagged-backtrace id="2">
        <backtrace id="3">
          <frame id="4" name="heavyWork()" addr="0x102000100">
            <binary id="5" name="Orbit" path="/Applications/Orbit.app/Contents/MacOS/Orbit"/>
          </frame>
          <frame id="6" name="main" addr="0x102000050">
            <binary ref="5"/>
          </frame>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <weight ref="1"/>
      <tagged-backtrace id="7">
        <backtrace id="8">
          <frame id="9" name="sin" addr="0x180000100">
            <binary id="10" name="libsystem_m.dylib" path="/usr/lib/system/libsystem_m.dylib"/>
          </frame>
          <frame ref="4"/>
          <frame ref="6"/>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <weight id="11">1000000</weight>
      <tagged-backtrace id="12">
        <backtrace id="13">
          <frame id="14" name="0x102000200" addr="0x102000200"/>
          <frame ref="6"/>
        </backtrace>
      </tagged-backtrace>
    </row>
  </node>
</trace-query-result>"#;

    const SAMPLE_ALLOCATIONS_TOC_XML: &str = r#"<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" name="Example Mac"/>
        <process name="Orbit"/>
      </target>
      <summary>
        <duration>5.0</duration>
        <template-name>Allocations</template-name>
      </summary>
    </info>
    <tracks>
      <track name="Allocations">
        <details>
          <detail name="Statistics" kind="table"/>
          <detail name="Allocations List" kind="table"/>
        </details>
      </track>
    </tracks>
  </run>
</trace-toc>"#;

    const SAMPLE_ALLOCATIONS_STATISTICS_XML: &str = r#"<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/tracks[1]/track[1]/details[1]/detail[1]'>
    <row category="All Heap &amp; Anonymous VM" persistent-bytes="33782272" count-persistent="1161" total-bytes="34183680" transient-bytes="401408" count-events="1183" count-transient="6" count-total="1167"/>
    <row category="All Heap Allocations" persistent-bytes="33782272" count-persistent="1161" total-bytes="33790464" transient-bytes="8192" count-events="1175" count-transient="2" count-total="1163"/>
    <row category="All Anonymous VM" persistent-bytes="0" count-persistent="0" total-bytes="393216" transient-bytes="393216" count-events="8" count-transient="4" count-total="4"/>
    <row category="Malloc 256.0 KiB" persistent-bytes="33554432" count-persistent="128" total-bytes="33554432" transient-bytes="0" count-events="128" count-transient="0" count-total="128"/>
    <row category="Malloc 48 Bytes" persistent-bytes="8208" count-persistent="171" total-bytes="8208" transient-bytes="0" count-events="171" count-transient="0" count-total="171"/>
    <row category="VM: Anonymous VM" persistent-bytes="0" count-persistent="0" total-bytes="393216" transient-bytes="393216" count-events="8" count-transient="4" count-total="4"/>
  </node>
</trace-query-result>"#;

    const SAMPLE_ALLOCATIONS_LIST_XML: &str = r#"<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/tracks[1]/track[1]/details[1]/detail[2]'>
    <row address="0x10133c000" category="Malloc 256.0 KiB" live="true" responsible-caller="allocateChunk()" responsible-library="Orbit" size="262144"/>
    <row address="0x10137c000" category="Malloc 256.0 KiB" live="true" responsible-caller="allocateChunk()" responsible-library="Orbit" size="262144"/>
    <row address="0x10139c000" category="Malloc 48 Bytes" live="true" responsible-caller="bootstrap()" responsible-library="Orbit" size="48"/>
    <row address="0x10139c100" category="VM: Anonymous VM" live="false" responsible-caller="&lt;Call stack limit reached&gt;" responsible-library="" size="393216"/>
  </node>
</trace-query-result>"#;

    #[test]
    fn summarizes_time_profile_xml_into_hotspots() {
        let metadata = parse_trace_metadata(SAMPLE_TOC_XML).unwrap();
        let samples = parse_time_profile_samples(SAMPLE_TIME_PROFILE_XML, &metadata).unwrap();
        let summary = render_time_profile_diagnosis(&metadata, &samples);

        assert!(summary.contains("Process: Orbit"));
        assert!(summary.contains("Template: Time Profiler"));
        assert!(summary.contains("Samples: 3"));
        assert!(summary.contains("Orbit  heavyWork()"));
        assert!(summary.contains("Orbit  main"));
        assert!(summary.contains("main > heavyWork()"));
    }

    #[test]
    fn summarizes_allocations_xml_into_memory_diagnosis() {
        let metadata = parse_trace_metadata(SAMPLE_ALLOCATIONS_TOC_XML).unwrap();
        let stats = parse_allocations_statistics(SAMPLE_ALLOCATIONS_STATISTICS_XML).unwrap();
        let rows = parse_allocations_list(SAMPLE_ALLOCATIONS_LIST_XML).unwrap();
        let summary = render_allocations_diagnosis(&metadata, &stats, &rows);

        assert!(summary.contains("Template: Allocations"));
        assert!(summary.contains("Live bytes: 32.2 MiB"));
        assert!(summary.contains("LIVE BY CATEGORY"));
        assert!(summary.contains("Malloc 256.0 KiB"));
        assert!(summary.contains("TRANSIENT BY CATEGORY"));
        assert!(summary.contains("VM: Anonymous VM"));
        assert!(summary.contains("LIVE BY RESPONSIBLE CALLER"));
        assert!(summary.contains("allocateChunk()"));
    }
}
