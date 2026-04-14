use std::path::PathBuf;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{Args, Parser, Subcommand, ValueEnum};

pub const CLAP_STYLING: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Blue.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default())
    .valid(AnsiColor::Green.on_default())
    .invalid(AnsiColor::Magenta.on_default().bold())
    .error(AnsiColor::Red.on_default().bold())
    .context(AnsiColor::Yellow.on_default().dimmed())
    .context_value(AnsiColor::Yellow.on_default().italic());

const PLATFORM_ARG_HELP: &str =
    "Select a platform when Orbit cannot infer one from the manifest or current command.";
const PLATFORM_ARG_LONG_HELP: &str = "Select a platform when Orbit cannot infer one from the manifest or current command.\n\nCommon values:\n  ios: iPhone and iPad app workflows\n  macos: Mac app workflows\n  tvos: Apple TV workflows\n  visionos: visionOS workflows\n  watchos: watch app and watch extension workflows";
const DISTRIBUTION_ARG_HELP: &str =
    "Select the packaging and signing mode for the build or submission.";
const DISTRIBUTION_ARG_LONG_HELP: &str = "Select the packaging and signing mode for the build or submission.\n\nValues:\n  development: local development and debugging\n  ad-hoc: signed device distribution outside the App Store\n  app-store: App Store and TestFlight upload artifacts\n  developer-id: notarized macOS distribution outside the Mac App Store\n  mac-app-store: Mac App Store upload artifacts";
const TRACE_ARG_HELP: &str = "Collect a CPU or memory trace while the command runs.";

#[derive(Debug, Parser)]
#[command(name = "orbit")]
#[command(about = "Manifest-first Apple app build, run, test, and signing CLI")]
#[command(arg_required_else_help = true)]
#[command(styles = CLAP_STYLING)]
#[command(
    long_about = "Orbit reads app intent from `orbit.json`.\n\nUse the JSON schema to understand manifest fields. Use CLI help for workflows and command behavior. `orbit init` also writes an informational `_description` field that points back here.\n\nEvery command supports `--help` for detailed flags, arguments, and examples. For example: `orbit build --help`, `orbit test --help`, `orbit ui schema --help`.\n\nUse `orbit ui schema` to inspect the accepted `tests.ui` YAML dialect and backend support.",
    after_help = "Scenarios:\n  Development:\n    Create a new project:\n      orbit init\n\n    Run the app in common modes:\n      orbit run --platform ios --simulator\n      orbit run --platform ios --device --debug\n      orbit run --platform macos\n\n    Capture SwiftUI `#Preview` screenshots:\n      orbit preview list --platform ios\n      orbit preview shot Basic --platform ios\n\n    Check formatting and project semantics:\n      orbit format\n      orbit format --write\n      orbit lint\n\n    Run unit tests:\n      orbit test\n      orbit test --trace\n\n    Run UI tests:\n      orbit test --ui --platform ios\n      orbit test --ui --platform macos\n      orbit test --ui --platform macos --flow onboarding-provider-setup\n\n    Trace UI tests:\n      orbit test --ui --platform ios --trace\n      orbit test --ui --platform macos --trace\n      orbit test --ui --platform macos --trace --flow onboarding-provider-setup\n\n    Inspect recorded traces:\n      orbit inspect-trace .orbit/artifacts/profiles/run.trace\n\n    Inspect the UI test DSL and backend support:\n      orbit ui schema --platform ios\n\n  Build And Submit:\n    Build local development artifacts:\n      orbit build --platform ios --distribution development\n\n    Build release artifacts:\n      orbit build --platform ios --distribution app-store --release\n      orbit build --platform macos --distribution developer-id --release\n\n    Submit a built artifact:\n      orbit submit --platform ios --wait\n      orbit submit --receipt .orbit/receipts/<receipt>.json --wait"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        help = "Use a specific `orbit.json` instead of auto-discovery."
    )]
    pub manifest: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        help = "Deep-merge `orbit.<env>.json` on top of the base manifest."
    )]
    pub env: Option<String>,

    #[arg(
        long,
        global = true,
        help = "Fail instead of prompting when Orbit needs an explicit choice."
    )]
    pub non_interactive: bool,

    #[arg(
        long,
        global = true,
        help = "Print extra diagnostics and underlying tool output."
    )]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a new Orbit scaffold and starter `orbit.json`.
    Init(InitArgs),
    /// Validate manifest structure, sources, dependencies, and project semantics.
    Lint(LintArgs),
    /// Check or rewrite formatting using Orbit-owned style settings.
    Format(FormatArgs),
    /// Run unit tests, UI flows, or profiling sessions declared in the manifest.
    Test(TestArgs),
    /// Inspect and render SwiftUI previews.
    Preview(PreviewArgs),
    /// Inspect automation targets and query the UI test dialect.
    Ui(UiArgs),
    /// Refresh lock state for git-backed dependencies.
    Deps(DepsArgs),
    /// Editor and build-server integration helpers.
    Ide(Box<IdeArgs>),
    #[command(hide = true)]
    Bsp(BspArgs),
    #[command(hide = true)]
    InspectTrace(InspectTraceArgs),
    /// Launch the app on a simulator or device for runtime verification.
    Run(RunArgs),
    /// Produce signed or unsigned build artifacts.
    Build(BuildArgs),
    /// Upload a previously built artifact to Apple services.
    Submit(SubmitArgs),
    /// Remove local and/or remote Orbit-managed state.
    Clean(CleanArgs),
    /// Apple account, device, and signing utilities.
    Apple(Box<AppleArgs>),
}

#[derive(Debug, Args)]
#[command(about = "Create a new Orbit project scaffold.")]
pub struct InitArgs {}

#[derive(Debug, Args)]
#[command(
    about = "Validate manifest structure, dependency state, and project semantics.",
    after_help = "Examples:\n  orbit lint\n  orbit lint --platform ios"
)]
pub struct LintArgs {
    #[arg(long, value_enum, help = "Validate one platform explicitly.")]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
#[command(
    about = "Check or rewrite formatting using Orbit-owned style settings.",
    after_help = "Examples:\n  orbit format\n  orbit format --write"
)]
pub struct FormatArgs {
    #[arg(long, help = "Rewrite files in place instead of reporting diffs.")]
    pub write: bool,
}

#[derive(Debug, Args)]
#[command(
    about = "Run unit tests, UI flows, or profiling sessions declared in the manifest.",
    long_about = "By default `orbit test` runs the manifest's `tests.unit` suite.\n\nUse `--ui` to run `tests.ui`, and use `orbit ui schema` when you need the accepted YAML grammar or backend support matrix.",
    after_help = "Examples:\n  orbit test\n  orbit test --ui --platform ios\n  orbit test --ui --platform macos --flow onboarding-provider-setup\n  orbit test --trace\n  orbit ui schema --platform ios"
)]
pub struct TestArgs {
    #[arg(long, help = "Run `tests.ui` instead of the unit-test suite.")]
    pub ui: bool,

    #[arg(
        long = "flow",
        help = "Run only selected UI flows by configured name, file stem, file name, or path."
    )]
    pub flows: Vec<String>,

    #[arg(
        long,
        value_enum,
        help = "Select a platform when the manifest supports more than one."
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        default_missing_value = "cpu",
        help = "Collect a CPU or memory trace while the test run executes."
    )]
    pub trace: Option<ProfileKind>,
}

#[derive(Debug, Args)]
#[command(about = "Inspect and render SwiftUI previews declared in the app sources.")]
#[command(arg_required_else_help = true)]
pub struct PreviewArgs {
    #[command(subcommand)]
    pub command: PreviewCommand,
}

#[derive(Debug, Subcommand)]
pub enum PreviewCommand {
    /// List discovered previews for the selected platform.
    List(PreviewListArgs),
    /// Render one preview to a PNG screenshot.
    Shot(PreviewShotArgs),
}

#[derive(Debug, Args)]
pub struct PreviewListArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
pub struct PreviewShotArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        help = "Preview name to render. Omit to select interactively when there is more than one."
    )]
    pub preview: Option<String>,

    #[arg(
        long,
        help = "Write the rendered PNG to this path instead of Orbit's default artifacts directory."
    )]
    pub output: Option<PathBuf>,

    #[arg(
        long,
        default_value_t = 750,
        help = "Wait this many milliseconds before capturing the preview screenshot."
    )]
    pub delay_ms: u64,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
#[command(
    about = "Inspect automation targets and query the UI test dialect.",
    long_about = "Use `orbit ui schema` for product-level documentation of the accepted `tests.ui` YAML dialect and backend support.\n\nUse the other subcommands when you need to inspect a running automation target, debug selectors, or mutate simulator state.",
    after_help = "Common commands:\n  orbit ui schema --platform ios\n  orbit ui dump-tree --platform ios\n  orbit ui describe-point --platform ios --x 140 --y 142\n  orbit ui doctor --platform macos"
)]
pub struct UiArgs {
    #[command(subcommand)]
    pub command: UiCommand,
}

#[derive(Debug, Subcommand)]
pub enum UiCommand {
    /// Print the accepted `tests.ui` grammar and backend support matrix.
    Schema(UiSchemaArgs),
    /// Check automation prerequisites for the selected platform.
    Doctor(UiDoctorArgs),
    /// Dump the current accessibility tree as JSON.
    DumpTree(UiDumpTreeArgs),
    /// Inspect the accessibility element at a specific point.
    DescribePoint(UiDescribePointArgs),
    /// Bring the current automation target to the foreground.
    Focus(UiFocusArgs),
    /// Stream simulator or automation logs.
    Logs(UiLogsArgs),
    /// Import media into the target runtime.
    AddMedia(UiAddMediaArgs),
    /// Open a URL or deep link in the target runtime.
    Open(UiOpenArgs),
    /// Install a test dylib into the simulator runtime.
    InstallDylib(UiInstallDylibArgs),
    /// Run Instruments against the selected runtime.
    Instruments(UiInstrumentsArgs),
    /// Overwrite the simulator contacts database.
    UpdateContacts(UiUpdateContactsArgs),
    /// Inspect or delete captured crash logs.
    Crash(UiCrashArgs),
    #[command(hide = true)]
    ResetIdb(UiResetIdbArgs),
}

#[derive(Debug, Args)]
#[command(
    about = "Print the accepted `tests.ui` grammar and backend support matrix.",
    long_about = "Print product-level documentation for the accepted `tests.ui` YAML dialect and backend support.\n\nBy default Orbit prints a human-readable CLI view. Pass `--json` for the raw machine-readable schema.",
    after_help = "Examples:\n  orbit ui schema\n  orbit ui schema --platform ios\n  orbit ui schema --platform ios --json"
)]
pub struct UiSchemaArgs {
    #[arg(
        long,
        value_enum,
        help = "Filter backend support details to one platform."
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        help = "Print the raw machine-readable schema JSON instead of the human-readable CLI view."
    )]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct UiDoctorArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
pub struct UiDumpTreeArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
pub struct UiDescribePointArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        help = "Horizontal coordinate in the target window or simulator."
    )]
    pub x: f64,

    #[arg(long, help = "Vertical coordinate in the target window or simulator.")]
    pub y: f64,
}

#[derive(Debug, Args)]
pub struct UiFocusArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct UiLogsArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        allow_hyphen_values = true,
        help = "Extra log-tool arguments forwarded after Orbit attaches to the selected target."
    )]
    pub log_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UiAddMediaArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        required = true,
        help = "One or more media files to import into the target runtime."
    )]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct UiOpenArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(help = "URL or deep link to open in the selected runtime.")]
    pub url: String,
}

#[derive(Debug, Args)]
pub struct UiInstallDylibArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(help = "Path to the dylib to install into the simulator runtime.")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct UiInstrumentsArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(long, help = "Instruments template name, for example `Time Profiler`.")]
    pub template: String,

    #[arg(
        allow_hyphen_values = true,
        help = "Extra arguments forwarded to Instruments after Orbit sets up the run."
    )]
    pub instrument_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UiUpdateContactsArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(help = "Path to the contacts SQLite database to import.")]
    pub path: PathBuf,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct UiCrashArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[command(subcommand)]
    pub command: UiCrashCommand,
}

#[derive(Debug, Subcommand)]
pub enum UiCrashCommand {
    /// List matching crash logs.
    List(UiCrashListArgs),
    /// Print one crash log.
    Show(UiCrashShowArgs),
    /// Delete one or more crash logs.
    Delete(UiCrashDeleteArgs),
}

#[derive(Debug, Args)]
pub struct UiCrashListArgs {
    #[arg(
        long,
        help = "Only include crashes older than this timestamp or date expression."
    )]
    pub before: Option<String>,

    #[arg(
        long,
        help = "Only include crashes newer than this timestamp or date expression."
    )]
    pub since: Option<String>,

    #[arg(long, help = "Limit results to one app bundle identifier.")]
    pub bundle_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiCrashShowArgs {
    #[arg(help = "Crash log file name to print.")]
    pub name: String,
}

#[derive(Debug, Args)]
pub struct UiCrashDeleteArgs {
    #[arg(help = "Crash log file name to delete. Omit when using range filters or `--all`.")]
    pub name: Option<String>,

    #[arg(
        long,
        help = "Delete crashes older than this timestamp or date expression."
    )]
    pub before: Option<String>,

    #[arg(
        long,
        help = "Delete crashes newer than this timestamp or date expression."
    )]
    pub since: Option<String>,

    #[arg(long, help = "Delete crashes only for this app bundle identifier.")]
    pub bundle_id: Option<String>,

    #[arg(
        long,
        help = "Delete every matching crash log instead of requiring an explicit name."
    )]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct UiResetIdbArgs {}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
#[command(about = "Refresh lock state for git-backed dependencies.")]
pub struct DepsArgs {
    #[command(subcommand)]
    pub command: DepsCommand,
}

#[derive(Debug, Subcommand)]
pub enum DepsCommand {
    /// Update all git-backed dependencies or one named dependency.
    Update(DepsUpdateArgs),
}

#[derive(Debug, Args)]
pub struct DepsUpdateArgs {
    #[arg(
        help = "Optional dependency name to update. Omit to refresh every git-backed dependency."
    )]
    pub dependency: Option<String>,
}

#[derive(Debug, Args)]
pub struct BspArgs {}

#[derive(Debug, Args)]
pub struct InspectTraceArgs {
    pub trace: PathBuf,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ProfileKind {
    #[value(help = "Time profile focused on CPU work.")]
    Cpu,
    #[value(help = "Allocation profile focused on memory behavior.")]
    Memory,
}

#[derive(Debug, Args)]
#[command(about = "Editor integration helpers and build-server metadata.")]
#[command(arg_required_else_help = true)]
pub struct IdeArgs {
    #[command(subcommand)]
    pub command: IdeCommand,
}

#[derive(Debug, Subcommand)]
pub enum IdeCommand {
    /// Install Build Server Protocol connection files for the project.
    InstallBuildServer(IdeInstallBuildServerArgs),
    /// Print compiler arguments for one source file or semantic build unit.
    DumpArgs(IdeDumpArgs),
}

#[derive(Debug, Args)]
pub struct IdeInstallBuildServerArgs {}

#[derive(Debug, Args)]
pub struct IdeDumpArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        help = "Restrict the output to one source file instead of the whole semantic unit."
    )]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(
    about = "Launch the app on a simulator or device for runtime verification.",
    after_help = "Examples:\n  orbit run --platform ios --simulator\n  orbit run --platform ios --device --debug\n  orbit run --platform ios --simulator --trace"
)]
pub struct RunArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        conflicts_with = "device",
        help = "Launch on a simulator instead of a physical device."
    )]
    pub simulator: bool,

    #[arg(
        long,
        conflicts_with = "simulator",
        help = "Launch on a connected physical device."
    )]
    pub device: bool,

    #[arg(
        long,
        help = "Select one specific physical device identifier when multiple devices are available."
    )]
    pub device_id: Option<String>,

    #[arg(long, help = "Attach a debugger to the launched process.")]
    pub debug: bool,

    #[arg(
        long,
        value_enum,
        num_args = 0..=1,
        default_missing_value = "cpu",
        help = TRACE_ARG_HELP
    )]
    pub trace: Option<ProfileKind>,
}

#[derive(Debug, Args)]
#[command(
    about = "Produce signed or unsigned build artifacts.",
    after_help = "Examples:\n  orbit build --platform ios --distribution development\n  orbit build --platform ios --distribution app-store --release\n  orbit build --platform macos --distribution developer-id --release"
)]
pub struct BuildArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        value_enum,
        help = DISTRIBUTION_ARG_HELP,
        long_help = DISTRIBUTION_ARG_LONG_HELP
    )]
    pub distribution: Option<DistributionArg>,

    #[arg(long, help = "Use Release instead of Debug configuration.")]
    pub release: bool,

    #[arg(
        long,
        conflicts_with = "device",
        help = "Build for a simulator runtime instead of a physical device."
    )]
    pub simulator: bool,

    #[arg(
        long,
        conflicts_with = "simulator",
        help = "Build for a connected physical device runtime."
    )]
    pub device: bool,

    #[arg(
        long,
        help = "Write the produced artifact to this path instead of Orbit's default receipt location."
    )]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(
    about = "Upload a previously built artifact to Apple services.",
    long_about = "Use `submit` only when the user explicitly wants a real remote submission. Orbit can derive the latest matching receipt, or you can pass one explicitly with `--receipt`.",
    after_help = "Examples:\n  orbit submit --platform ios --wait\n  orbit submit --receipt .orbit/receipts/<receipt>.json --wait"
)]
pub struct SubmitArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        value_enum,
        help = DISTRIBUTION_ARG_HELP,
        long_help = DISTRIBUTION_ARG_LONG_HELP
    )]
    pub distribution: Option<DistributionArg>,

    #[arg(
        long,
        help = "Submit this explicit receipt instead of picking the latest matching one from `.orbit/receipts`."
    )]
    pub receipt: Option<PathBuf>,

    #[arg(
        long,
        help = "Wait for the remote submission job to finish instead of returning immediately."
    )]
    pub wait: bool,
}

#[derive(Debug, Args)]
#[command(
    about = "Remove local and/or remote Orbit-managed state.",
    long_about = "`orbit clean` is intentionally destructive. Use `--all` only when you mean to remove both local Orbit state and Orbit-managed remote Apple state."
)]
pub struct CleanArgs {
    #[arg(
        long,
        conflicts_with_all = ["apple", "all"],
        help = "Remove only local Orbit state such as `.orbit/` artifacts and caches."
    )]
    pub local: bool,

    #[arg(
        long,
        conflicts_with_all = ["local", "all"],
        help = "Remove only Orbit-managed remote Apple state."
    )]
    pub apple: bool,

    #[arg(
        long,
        conflicts_with_all = ["local", "apple"],
        help = "Remove both local Orbit state and Orbit-managed remote Apple state."
    )]
    pub all: bool,
}

#[derive(Debug, Args)]
#[command(about = "Apple account, device, and signing utilities.")]
#[command(arg_required_else_help = true)]
pub struct AppleArgs {
    #[command(subcommand)]
    pub command: AppleCommand,
}

#[derive(Debug, Subcommand)]
pub enum AppleCommand {
    /// List, register, import, or remove Apple devices.
    Device {
        #[command(subcommand)]
        command: AppleDeviceCommand,
    },
    /// Export or import signing material managed by Orbit.
    Signing {
        #[command(subcommand)]
        command: AppleSigningCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppleDeviceCommand {
    /// List devices visible to the current Apple account selection.
    List(ListDevicesArgs),
    /// Register one device with Apple Developer.
    Register(RegisterDeviceArgs),
    /// Import devices from a CSV file.
    Import(ImportDevicesArgs),
    /// Remove one registered device.
    Remove(RemoveDeviceArgs),
}

#[derive(Debug, Args)]
pub struct ListDevicesArgs {
    #[arg(long, help = "Refresh remote device state before printing the list.")]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct RegisterDeviceArgs {
    #[arg(long, help = "Human-readable device name to register.")]
    pub name: Option<String>,

    #[arg(long, help = "Device UDID to register.")]
    pub udid: Option<String>,

    #[arg(
        long,
        value_enum,
        default_value_t = DevicePlatform::Ios,
        help = "Declared device family for the registration request."
    )]
    pub platform: DevicePlatform,

    #[arg(
        long,
        conflicts_with_all = ["name", "udid"],
        help = "Register the current machine instead of passing an explicit name and UDID."
    )]
    pub current_machine: bool,
}

#[derive(Debug, Args)]
pub struct ImportDevicesArgs {
    #[arg(
        long,
        help = "CSV file to import. Omit to use Orbit's default import source when available."
    )]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RemoveDeviceArgs {
    #[arg(long, help = "Orbit or Apple device identifier to remove.")]
    pub id: Option<String>,

    #[arg(long, help = "Raw device UDID to remove.")]
    pub udid: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum DevicePlatform {
    #[value(name = "ios", help = "iPhone and iPad devices.")]
    Ios,
    #[value(
        name = "macos",
        help = "Mac hardware registered as a development device."
    )]
    MacOs,
    #[value(
        name = "universal",
        help = "Device usable across Apple platform families."
    )]
    Universal,
}

#[derive(Debug, Subcommand)]
pub enum AppleSigningCommand {
    /// Export signing material to a local directory.
    Export(SigningExportArgs),
    /// Import signing material from a PKCS#12 archive.
    Import(SigningImportArgs),
}

#[derive(Debug, Args)]
pub struct SigningExportArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        value_enum,
        help = DISTRIBUTION_ARG_HELP,
        long_help = DISTRIBUTION_ARG_LONG_HELP
    )]
    pub distribution: Option<DistributionArg>,

    #[arg(
        long,
        help = "Directory where Orbit should write the exported certificates, profiles, and metadata."
    )]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SigningImportArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,

    #[arg(
        long,
        value_enum,
        help = DISTRIBUTION_ARG_HELP,
        long_help = DISTRIBUTION_ARG_LONG_HELP
    )]
    pub distribution: Option<DistributionArg>,

    #[arg(
        long,
        help = "Path to the PKCS#12 archive that contains the signing certificate and private key."
    )]
    pub p12: PathBuf,

    #[arg(
        long,
        help = "Password used to decrypt the PKCS#12 archive, if it is encrypted."
    )]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum TargetPlatform {
    #[value(name = "ios", help = "iPhone and iPad app workflows.")]
    Ios,
    #[value(name = "macos", help = "Mac app workflows.")]
    Macos,
    #[value(name = "tvos", help = "Apple TV app workflows.")]
    Tvos,
    #[value(name = "visionos", help = "visionOS app workflows.")]
    Visionos,
    #[value(name = "watchos", help = "watchOS app and extension workflows.")]
    Watchos,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum DistributionArg {
    #[value(name = "development", help = "Local development and debugging builds.")]
    Development,
    #[value(
        name = "ad-hoc",
        help = "Signed device distribution outside the App Store."
    )]
    AdHoc,
    #[value(
        name = "app-store",
        help = "App Store and TestFlight upload artifacts."
    )]
    AppStore,
    #[value(
        name = "developer-id",
        help = "Notarized macOS distribution outside the Mac App Store."
    )]
    DeveloperId,
    #[value(name = "mac-app-store", help = "Mac App Store upload artifacts.")]
    MacAppStore,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn top_level_help_stays_focused_on_core_workflow() {
        let mut command = Cli::command();
        let help = command.render_long_help().to_string();

        assert!(help.contains("orbit ui schema --platform ios"));
        assert!(help.contains("orbit preview shot Basic --platform ios"));
        assert!(help.contains("orbit test --ui --platform macos --flow onboarding-provider-setup"));
        assert!(
            help.contains(
                "orbit test --ui --platform macos --trace --flow onboarding-provider-setup"
            )
        );
        assert!(help.contains("orbit submit --platform ios --wait"));
        assert!(help.contains("Every command supports `--help`"));
        assert!(help.contains("Capture SwiftUI `#Preview` screenshots"));
        assert!(help.contains("orbit inspect-trace .orbit/artifacts/profiles/run.trace"));
        assert!(!help.contains("Common commands:"));
        assert!(!help.contains("\n  bsp"));
    }

    #[test]
    fn ui_help_includes_schema_subcommand() {
        let mut command = Cli::command();
        let ui = command.find_subcommand_mut("ui").unwrap();
        let help = ui.render_long_help().to_string();

        assert!(help.contains("schema"));
        assert!(help.contains("tests.ui"));
    }

    #[test]
    fn ui_schema_help_mentions_json_output() {
        let mut command = Cli::command();
        let ui = command.find_subcommand_mut("ui").unwrap();
        let schema = ui.find_subcommand_mut("schema").unwrap();
        let help = schema.render_long_help().to_string();

        assert!(help.contains("human-readable CLI view"));
        assert!(help.contains("--json"));
    }

    #[test]
    fn build_help_explains_distribution_choices() {
        let mut command = Cli::command();
        let build = command.find_subcommand_mut("build").unwrap();
        let help = build.render_long_help().to_string();

        assert!(help.contains("Select the packaging and signing mode"));
        assert!(help.contains("App Store and TestFlight upload artifacts"));
    }

    #[test]
    fn ui_open_help_describes_the_url_argument() {
        let mut command = Cli::command();
        let ui = command.find_subcommand_mut("ui").unwrap();
        let open = ui.find_subcommand_mut("open").unwrap();
        let help = open.render_long_help().to_string();

        assert!(help.contains("URL or deep link to open"));
        assert!(help.contains("Select a platform when Orbit cannot infer one"));
    }
}
