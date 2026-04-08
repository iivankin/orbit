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

#[derive(Debug, Parser)]
#[command(name = "orbit")]
#[command(about = "Local-first Apple family build and signing CLI")]
#[command(arg_required_else_help = true)]
#[command(styles = CLAP_STYLING)]
#[command(
    after_help = "Examples:\n  orbit init\n  orbit lint\n  orbit lint --platform ios\n  orbit format\n  orbit format --write\n  orbit test\n  orbit test --trace\n  orbit test --ui --platform ios\n  orbit ui dump-tree --platform ios\n  orbit ui describe-point --platform ios --x 140 --y 142\n  orbit ui doctor --platform macos\n  orbit ui open --platform ios https://example.com\n  orbit ui crash --platform ios list\n  orbit deps update\n  orbit deps update OrbitGreeting\n  orbit ide install-build-server\n  orbit ide dump-args\n  orbit ide dump-args --platform ios --file Sources/App/App.swift\n  orbit inspect-trace .orbit/artifacts/profiles/run.trace\n  orbit run --platform ios --simulator\n  orbit run --platform ios --device --trace\n  orbit build --env stage --platform ios --distribution development\n  orbit build --platform ios --distribution development\n  orbit build --platform ios --distribution app-store --release\n  orbit submit --platform ios --wait\n  orbit clean --all\n  orbit apple device list --refresh\n  orbit apple signing export --platform ios --distribution development\n  orbit apple signing import --platform ios --distribution development --p12 ./signing.p12 --password secret"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub manifest: Option<PathBuf>,

    #[arg(long, global = true)]
    pub env: Option<String>,

    #[arg(long, global = true)]
    pub non_interactive: bool,

    #[arg(long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Init(InitArgs),
    Lint(LintArgs),
    Format(FormatArgs),
    Test(TestArgs),
    Ui(UiArgs),
    Deps(DepsArgs),
    Ide(Box<IdeArgs>),
    Bsp(BspArgs),
    InspectTrace(InspectTraceArgs),
    Run(RunArgs),
    Build(BuildArgs),
    Submit(SubmitArgs),
    Clean(CleanArgs),
    Apple(Box<AppleArgs>),
}

#[derive(Debug, Args)]
pub struct InitArgs {}

#[derive(Debug, Args)]
pub struct LintArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
pub struct FormatArgs {
    #[arg(long)]
    pub write: bool,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    #[arg(long)]
    pub ui: bool,

    #[arg(long = "flow")]
    pub flows: Vec<String>,

    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "cpu")]
    pub trace: Option<ProfileKind>,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct UiArgs {
    #[command(subcommand)]
    pub command: UiCommand,
}

#[derive(Debug, Subcommand)]
pub enum UiCommand {
    Doctor(UiDoctorArgs),
    DumpTree(UiDumpTreeArgs),
    DescribePoint(UiDescribePointArgs),
    Focus(UiFocusArgs),
    Logs(UiLogsArgs),
    AddMedia(UiAddMediaArgs),
    Open(UiOpenArgs),
    InstallDylib(UiInstallDylibArgs),
    Instruments(UiInstrumentsArgs),
    UpdateContacts(UiUpdateContactsArgs),
    Crash(UiCrashArgs),
    ResetIdb(UiResetIdbArgs),
}

#[derive(Debug, Args)]
pub struct UiDoctorArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
pub struct UiDumpTreeArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
pub struct UiDescribePointArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long)]
    pub x: f64,

    #[arg(long)]
    pub y: f64,
}

#[derive(Debug, Args)]
pub struct UiFocusArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct UiLogsArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(allow_hyphen_values = true)]
    pub log_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UiAddMediaArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct UiOpenArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    pub url: String,
}

#[derive(Debug, Args)]
pub struct UiInstallDylibArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    pub path: PathBuf,
}

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
pub struct UiInstrumentsArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long)]
    pub template: String,

    #[arg(allow_hyphen_values = true)]
    pub instrument_args: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UiUpdateContactsArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    pub path: PathBuf,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct UiCrashArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[command(subcommand)]
    pub command: UiCrashCommand,
}

#[derive(Debug, Subcommand)]
pub enum UiCrashCommand {
    List(UiCrashListArgs),
    Show(UiCrashShowArgs),
    Delete(UiCrashDeleteArgs),
}

#[derive(Debug, Args)]
pub struct UiCrashListArgs {
    #[arg(long)]
    pub before: Option<String>,

    #[arg(long)]
    pub since: Option<String>,

    #[arg(long)]
    pub bundle_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiCrashShowArgs {
    pub name: String,
}

#[derive(Debug, Args)]
pub struct UiCrashDeleteArgs {
    pub name: Option<String>,

    #[arg(long)]
    pub before: Option<String>,

    #[arg(long)]
    pub since: Option<String>,

    #[arg(long)]
    pub bundle_id: Option<String>,

    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct UiResetIdbArgs {}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct DepsArgs {
    #[command(subcommand)]
    pub command: DepsCommand,
}

#[derive(Debug, Subcommand)]
pub enum DepsCommand {
    Update(DepsUpdateArgs),
}

#[derive(Debug, Args)]
pub struct DepsUpdateArgs {
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
    Cpu,
    Memory,
}

#[derive(Debug, Args)]
#[command(arg_required_else_help = true)]
pub struct IdeArgs {
    #[command(subcommand)]
    pub command: IdeCommand,
}

#[derive(Debug, Subcommand)]
pub enum IdeCommand {
    InstallBuildServer(IdeInstallBuildServerArgs),
    DumpArgs(IdeDumpArgs),
}

#[derive(Debug, Args)]
pub struct IdeInstallBuildServerArgs {}

#[derive(Debug, Args)]
pub struct IdeDumpArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, conflicts_with = "device")]
    pub simulator: bool,

    #[arg(long, conflicts_with = "simulator")]
    pub device: bool,

    #[arg(long)]
    pub device_id: Option<String>,

    #[arg(long)]
    pub debug: bool,

    #[arg(long, value_enum, num_args = 0..=1, default_missing_value = "cpu")]
    pub trace: Option<ProfileKind>,
}

#[derive(Debug, Args)]
pub struct BuildArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub release: bool,

    #[arg(long, conflicts_with = "device")]
    pub simulator: bool,

    #[arg(long, conflicts_with = "simulator")]
    pub device: bool,

    #[arg(long)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SubmitArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub receipt: Option<PathBuf>,

    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    #[arg(long, conflicts_with_all = ["apple", "all"])]
    pub local: bool,

    #[arg(long, conflicts_with_all = ["local", "all"])]
    pub apple: bool,

    #[arg(long, conflicts_with_all = ["local", "apple"])]
    pub all: bool,
}

#[derive(Debug, Args)]
pub struct AppleArgs {
    #[command(subcommand)]
    pub command: AppleCommand,
}

#[derive(Debug, Subcommand)]
pub enum AppleCommand {
    Device {
        #[command(subcommand)]
        command: AppleDeviceCommand,
    },
    Signing {
        #[command(subcommand)]
        command: AppleSigningCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppleDeviceCommand {
    List(ListDevicesArgs),
    Register(RegisterDeviceArgs),
    Import(ImportDevicesArgs),
    Remove(RemoveDeviceArgs),
}

#[derive(Debug, Args)]
pub struct ListDevicesArgs {
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct RegisterDeviceArgs {
    #[arg(long)]
    pub name: Option<String>,

    #[arg(long)]
    pub udid: Option<String>,

    #[arg(long, value_enum, default_value_t = DevicePlatform::Ios)]
    pub platform: DevicePlatform,

    #[arg(long, conflicts_with_all = ["name", "udid"])]
    pub current_machine: bool,
}

#[derive(Debug, Args)]
pub struct ImportDevicesArgs {
    #[arg(long)]
    pub file: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RemoveDeviceArgs {
    #[arg(long)]
    pub id: Option<String>,

    #[arg(long)]
    pub udid: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum DevicePlatform {
    #[value(name = "ios")]
    Ios,
    #[value(name = "macos")]
    MacOs,
    #[value(name = "universal")]
    Universal,
}

#[derive(Debug, Subcommand)]
pub enum AppleSigningCommand {
    Export(SigningExportArgs),
    Import(SigningImportArgs),
}

#[derive(Debug, Args)]
pub struct SigningExportArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SigningImportArgs {
    #[arg(long, value_enum)]
    pub platform: Option<TargetPlatform>,

    #[arg(long, value_enum)]
    pub distribution: Option<DistributionArg>,

    #[arg(long)]
    pub p12: PathBuf,

    #[arg(long)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum TargetPlatform {
    #[value(name = "ios")]
    Ios,
    #[value(name = "macos")]
    Macos,
    #[value(name = "tvos")]
    Tvos,
    #[value(name = "visionos")]
    Visionos,
    #[value(name = "watchos")]
    Watchos,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum DistributionArg {
    #[value(name = "development")]
    Development,
    #[value(name = "ad-hoc")]
    AdHoc,
    #[value(name = "app-store")]
    AppStore,
    #[value(name = "developer-id")]
    DeveloperId,
    #[value(name = "mac-app-store")]
    MacAppStore,
}
