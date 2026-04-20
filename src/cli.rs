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
    "Select a platform when Orbi cannot infer one from the manifest or current command.";
const PLATFORM_ARG_LONG_HELP: &str = "Select a platform when Orbi cannot infer one from the manifest or current command.\n\nCommon values:\n  ios: iPhone and iPad app workflows\n  macos: Mac app workflows\n  tvos: Apple TV workflows\n  visionos: visionOS workflows\n  watchos: watch app and watch extension workflows";
const DISTRIBUTION_ARG_HELP: &str =
    "Select the packaging and signing mode for the build or submission.";
const DISTRIBUTION_ARG_LONG_HELP: &str = "Select the packaging and signing mode for the build or submission.\n\nValues:\n  development: local development and debugging\n  ad-hoc: signed device distribution outside the App Store\n  app-store: App Store and TestFlight upload artifacts\n  developer-id: signed `.dmg` for notarized macOS distribution outside the Mac App Store\n  mac-app-store: signed `.app` bundle for Mac App Store upload";
const TRACE_ARG_HELP: &str = "Collect a CPU or memory trace while the command runs.";

#[derive(Debug, Parser)]
#[command(name = "orbi")]
#[command(about = "Manifest-first Apple app build, run, test, and signing CLI")]
#[command(arg_required_else_help = true)]
#[command(styles = CLAP_STYLING)]
#[command(
    long_about = "Orbi reads app intent from `orbi.json`.\n\nUse the JSON schema to understand manifest fields. Use CLI help for workflows and command behavior. `orbi init` also writes an informational `_description` field that points back here.\n\nEvery command supports `--help` for detailed flags, arguments, and examples. For example: `orbi build --help`, `orbi test --help`, `orbi ui init --help`.\n\nUI test flows are JSON files with `$schema`; use `orbi ui init` to scaffold them.",
    after_help = "Scenarios:\n  Recommended UI Workflow:\n    Write Swift and optional backend unit tests:\n      orbi test\n\n    Check that the interface looks right with a SwiftUI preview screenshot:\n      orbi preview list --platform ios\n      orbi preview shot Basic --platform ios\n\n    Write UI test flows:\n      orbi ui init Tests/UI/login.json\n\n    Run UI tests normally:\n      orbi test --ui --platform ios\n      orbi test --ui --platform macos\n      orbi test --ui --platform macos --flow onboarding-provider-setup\n\n    Run a final trace pass:\n      orbi test --ui --platform ios --trace\n      orbi test --ui --platform macos --trace\n      orbi test --ui --platform macos --trace --flow onboarding-provider-setup\n\n    Inspect recorded traces:\n      orbi inspect-trace .orbi/artifacts/profiles/run.trace\n\n  Development:\n    Create a new project:\n      orbi init\n\n    Run the app in common modes:\n      orbi run --platform ios --simulator\n      orbi run --platform ios --device --debug\n      orbi run --platform macos\n\n    Check formatting and project semantics:\n      orbi format\n      orbi format --write\n      orbi lint\n\n  Build And Submit:\n    Build local development artifacts:\n      orbi build --platform ios --distribution development\n\n    Build release artifacts:\n      orbi build --platform ios --distribution app-store --release\n      orbi build --platform macos --distribution developer-id --release\n      orbi build --platform macos --distribution mac-app-store --release\n\n    Submit a built artifact:\n      orbi submit --platform ios --wait\n      orbi submit --receipt .orbi/receipts/<receipt>.json --wait"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        help = "Use a specific `orbi.json` instead of auto-discovery."
    )]
    pub manifest: Option<PathBuf>,

    #[arg(
        long,
        global = true,
        help = "Deep-merge `orbi.<env>.json` on top of the base manifest."
    )]
    pub env: Option<String>,

    #[arg(
        long,
        global = true,
        help = "Fail instead of prompting when Orbi needs an explicit choice."
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
    /// Create a new Orbi scaffold and starter `orbi.json`.
    Init(InitArgs),
    /// Validate manifest structure, sources, dependencies, and project semantics.
    Lint(LintArgs),
    /// Check or rewrite formatting using Orbi-owned style settings.
    Format(FormatArgs),
    /// Run unit tests, UI flows, or profiling sessions declared in the manifest.
    Test(TestArgs),
    /// Inspect and render SwiftUI previews.
    Preview(PreviewArgs),
    /// Inspect automation targets and run direct UI actions.
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
    /// Remove local and/or remote Orbi-managed state.
    Clean(CleanArgs),
    /// App Store Connect auth, device, signing, and submission workflows backed by embedded `asc` config.
    Asc(Box<AscArgs>),
}

#[derive(Debug, Args)]
#[command(about = "Create a new Orbi project scaffold.")]
pub struct InitArgs {}

#[derive(Debug, Args)]
#[command(
    about = "Validate manifest structure, dependency state, and project semantics.",
    after_help = "Examples:\n  orbi lint\n  orbi lint --platform ios"
)]
pub struct LintArgs {
    #[arg(long, value_enum, help = "Validate one platform explicitly.")]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args)]
#[command(
    about = "Check or rewrite formatting using Orbi-owned style settings.",
    after_help = "Examples:\n  orbi format\n  orbi format --write"
)]
pub struct FormatArgs {
    #[arg(long, help = "Rewrite files in place instead of reporting diffs.")]
    pub write: bool,
}

#[derive(Debug, Args)]
#[command(
    about = "Run unit tests, UI flows, or profiling sessions declared in the manifest.",
    long_about = "By default `orbi test` runs the manifest's `tests.unit` suite.\n\nUse `--ui` to run `tests.ui`. UI test flows are JSON files with `$schema`; use `orbi ui init` when you need a starter flow file.",
    after_help = "Examples:\n  orbi test\n  orbi test --ui --platform ios\n  orbi test --ui --platform macos --flow onboarding-provider-setup\n  orbi test --ui --platform macos --focus\n  orbi test --trace\n  orbi ui init Tests/UI/login.json"
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

    #[arg(
        long,
        requires = "ui",
        help = "Best-effort: bring the automation target to the foreground after each `launchApp`."
    )]
    pub focus: bool,
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
        help = "Write the rendered PNG to this path instead of Orbi's default artifacts directory."
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
    about = "Inspect automation targets and run direct UI actions.",
    long_about = "Use `orbi ui init` to scaffold a `tests.ui` flow file.\n\nUse `orbi ui clean-trace-temp` when you want to reclaim disk space from stale Instruments temp traces left by previous runs.\n\nUse the other subcommands when you need to inspect a running automation target, debug selectors, or mutate simulator state.",
    after_help = "Common commands:\n  orbi ui init Tests/UI/login.json\n  orbi ui clean-trace-temp\n  orbi ui tap --platform ios --text Continue\n  orbi ui swipe --platform ios --direction left\n  orbi ui dump-tree --platform ios\n  orbi ui describe-point --platform ios --x 140 --y 142\n  orbi ui doctor --platform macos"
)]
pub struct UiArgs {
    #[command(subcommand)]
    pub command: UiCommand,
}

#[derive(Debug, Subcommand)]
pub enum UiCommand {
    /// Write a starter JSON UI flow file with `$schema`.
    Init(UiInitArgs),
    /// Check automation prerequisites for the selected platform.
    Doctor(UiDoctorArgs),
    /// Remove stale Instruments temp traces from the current user's temp directory.
    CleanTraceTemp(UiCleanTraceTempArgs),
    /// Dump the current accessibility tree as JSON.
    DumpTree(UiDumpTreeArgs),
    /// Inspect the accessibility element at a specific point.
    DescribePoint(UiDescribePointArgs),
    /// Bring the current automation target to the foreground.
    Focus(UiFocusArgs),
    /// Launch the manifest app or an explicit bundle id.
    LaunchApp(UiLaunchAppArgs),
    /// Stop the manifest app or an explicit bundle id.
    StopApp(UiAppTargetArgs),
    /// Kill the manifest app or an explicit bundle id.
    KillApp(UiAppTargetArgs),
    /// Clear the manifest app's installed simulator state.
    ClearState(UiAppTargetArgs),
    /// Clear the target runtime keychain.
    ClearKeychain(UiPlatformOnlyArgs),
    /// Tap an element by accessibility text and/or identifier.
    #[command(alias = "tap-on")]
    Tap(UiSelectorActionArgs),
    /// Hover over an element by accessibility text and/or identifier.
    #[command(alias = "hover-on")]
    Hover(UiSelectorActionArgs),
    /// Right-click an element by accessibility text and/or identifier.
    #[command(alias = "right-click-on")]
    RightClick(UiSelectorActionArgs),
    /// Tap a point like `140,142` or `50%,80%`.
    #[command(alias = "tap-on-point")]
    TapPoint(UiTapPointArgs),
    /// Double-tap an element by accessibility text and/or identifier.
    #[command(alias = "double-tap-on")]
    DoubleTap(UiSelectorActionArgs),
    /// Long-press an element by accessibility text and/or identifier.
    #[command(alias = "long-press-on")]
    LongPress(UiLongPressArgs),
    /// Swipe by direction or between explicit points.
    Swipe(UiSwipeArgs),
    /// Swipe on an element in one direction.
    SwipeOn(UiSwipeOnArgs),
    /// Drag from one element to another.
    #[command(alias = "drag-and-drop")]
    Drag(UiDragArgs),
    /// Scroll in one direction.
    Scroll(UiScrollArgs),
    /// Scroll within an element in one direction.
    ScrollOn(UiScrollOnArgs),
    /// Scroll until an element becomes visible.
    ScrollUntilVisible(UiScrollUntilVisibleArgs),
    /// Type text into the focused control.
    InputText(UiInputTextArgs),
    /// Delete characters from the focused control.
    EraseText(UiEraseTextArgs),
    /// Press a named key with optional modifiers.
    PressKey(UiPressKeyArgs),
    /// Press a raw key code with optional modifiers.
    PressKeyCode(UiPressKeyCodeArgs),
    /// Press a sequence of raw key codes.
    KeySequence(UiKeySequenceArgs),
    /// Press a simulator or device hardware button.
    PressButton(UiPressButtonArgs),
    /// Select a menu item path such as `File > New Window`.
    SelectMenuItem(UiSelectMenuItemArgs),
    /// Hide the software keyboard.
    HideKeyboard(UiPlatformOnlyArgs),
    /// Assert that an element is visible.
    AssertVisible(UiSelectorActionArgs),
    /// Assert that an element is not visible.
    AssertNotVisible(UiSelectorActionArgs),
    /// Wait until an element becomes visible and/or invisible.
    #[command(alias = "extended-wait-until")]
    WaitUntil(UiWaitUntilArgs),
    /// Wait for animations to settle.
    WaitForAnimationToEnd(UiWaitForAnimationToEndArgs),
    /// Capture a screenshot into Orbi's artifacts directory.
    TakeScreenshot(UiTakeScreenshotArgs),
    /// Stream simulator or automation logs.
    Logs(UiLogsArgs),
    /// Import media into the target runtime.
    AddMedia(UiAddMediaArgs),
    /// Open a URL or deep link in the target runtime.
    #[command(alias = "open-link")]
    Open(UiOpenArgs),
    /// Override the runtime location.
    SetLocation(UiSetLocationArgs),
    /// Apply simulator or runtime permissions for an app.
    SetPermissions(UiSetPermissionsArgs),
    /// Travel through a sequence of coordinates.
    Travel(UiTravelArgs),
    /// Install a test dylib into the simulator runtime.
    InstallDylib(UiInstallDylibArgs),
    /// Run Instruments against the selected runtime.
    Instruments(UiInstrumentsArgs),
    /// Overwrite the simulator contacts database.
    UpdateContacts(UiUpdateContactsArgs),
    /// Inspect or delete captured crash logs.
    Crash(UiCrashArgs),
}

#[derive(Debug, Args, Clone)]
pub struct UiPlatformOnlyArgs {
    #[arg(
        long,
        value_enum,
        help = PLATFORM_ARG_HELP,
        long_help = PLATFORM_ARG_LONG_HELP
    )]
    pub platform: Option<TargetPlatform>,
}

#[derive(Debug, Args, Clone)]
pub struct UiSelectorArgs {
    #[arg(long, help = "Match the accessibility text or label.")]
    pub text: Option<String>,

    #[arg(long, help = "Match the accessibility identifier.")]
    pub id: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiAppTargetArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, help = "Override the default manifest app bundle identifier.")]
    pub app_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiSelectorActionArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[command(flatten)]
    pub selector: UiSelectorArgs,
}

#[derive(Debug, Args)]
pub struct UiLaunchAppArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, help = "Override the default manifest app bundle identifier.")]
    pub app_id: Option<String>,

    #[arg(long, help = "Clear installed app data before launch.")]
    pub clear_state: bool,

    #[arg(long, help = "Clear keychain entries before launch.")]
    pub clear_keychain: bool,

    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        help = "Stop the target before launching it again."
    )]
    pub stop_app: bool,

    #[arg(
        long = "arg",
        help = "Repeat `--arg key=value` to pass launch arguments."
    )]
    pub arguments: Vec<String>,

    #[arg(
        long = "permission",
        help = "Repeat `--permission name=allow|deny|unset` to set launch permissions."
    )]
    pub permissions: Vec<String>,

    #[arg(
        long,
        help = "Best-effort: bring the automation target to the foreground after `launch-app`."
    )]
    pub focus: bool,
}

#[derive(Debug, Args)]
pub struct UiTapPointArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(help = "Point expression like `140,142` or `50%,80%`.")]
    pub point: String,
}

#[derive(Debug, Args)]
pub struct UiLongPressArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[command(flatten)]
    pub selector: UiSelectorArgs,

    #[arg(long, help = "Press duration like `1500ms` or `1.5s`.")]
    pub duration: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiSwipeArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(
        long,
        value_enum,
        help = "Swipe in one direction using Orbi's default path."
    )]
    pub direction: Option<UiSwipeDirectionArg>,

    #[arg(long, help = "Explicit start point like `90%,50%`.")]
    pub start: Option<String>,

    #[arg(long, help = "Explicit end point like `10%,50%`.")]
    pub end: Option<String>,

    #[arg(long, help = "Swipe duration like `500ms` or `0.5s`.")]
    pub duration: Option<String>,

    #[arg(
        long,
        help = "Pointer delta in points between intermediate swipe samples."
    )]
    pub delta: Option<u32>,
}

#[derive(Debug, Args)]
pub struct UiSwipeOnArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[command(flatten)]
    pub selector: UiSelectorArgs,

    #[arg(long, value_enum, help = "Swipe direction.")]
    pub direction: UiSwipeDirectionArg,

    #[arg(long, help = "Swipe duration like `500ms` or `0.5s`.")]
    pub duration: Option<String>,

    #[arg(
        long,
        help = "Pointer delta in points between intermediate swipe samples."
    )]
    pub delta: Option<u32>,
}

#[derive(Debug, Args)]
pub struct UiDragArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long = "from-text", help = "Source accessibility text or label.")]
    pub from_text: Option<String>,

    #[arg(long = "from-id", help = "Source accessibility identifier.")]
    pub from_id: Option<String>,

    #[arg(long = "to-text", help = "Destination accessibility text or label.")]
    pub to_text: Option<String>,

    #[arg(long = "to-id", help = "Destination accessibility identifier.")]
    pub to_id: Option<String>,

    #[arg(long, help = "Drag duration like `650ms` or `0.65s`.")]
    pub duration: Option<String>,

    #[arg(
        long,
        help = "Pointer delta in points between intermediate drag samples."
    )]
    pub delta: Option<u32>,
}

#[derive(Debug, Args)]
pub struct UiScrollArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, value_enum, default_value = "down", help = "Scroll direction.")]
    pub direction: UiSwipeDirectionArg,
}

#[derive(Debug, Args)]
pub struct UiScrollOnArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[command(flatten)]
    pub selector: UiSelectorArgs,

    #[arg(long, value_enum, default_value = "down", help = "Scroll direction.")]
    pub direction: UiSwipeDirectionArg,
}

#[derive(Debug, Args)]
pub struct UiScrollUntilVisibleArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[command(flatten)]
    pub selector: UiSelectorArgs,

    #[arg(long, value_enum, default_value = "down", help = "Scroll direction.")]
    pub direction: UiSwipeDirectionArg,

    #[arg(long, help = "Maximum wait like `20s` or `20000ms`.")]
    pub timeout: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiInputTextArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(help = "Text to type into the focused control.")]
    pub text: String,
}

#[derive(Debug, Args)]
pub struct UiEraseTextArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, default_value_t = 50, help = "Characters to delete.")]
    pub characters: u32,
}

#[derive(Debug, Args)]
pub struct UiPressKeyArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(help = "Named key like `ENTER`, `LEFT`, or one character.")]
    pub key: String,

    #[arg(
        long = "modifier",
        value_enum,
        help = "Repeat to add key modifiers like `--modifier command`."
    )]
    pub modifiers: Vec<UiKeyModifierArg>,
}

#[derive(Debug, Args)]
pub struct UiPressKeyCodeArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(help = "Raw platform key code.")]
    pub keycode: u32,

    #[arg(long, help = "Press duration like `200ms` or `0.2s`.")]
    pub duration: Option<String>,

    #[arg(
        long = "modifier",
        value_enum,
        help = "Repeat to add key modifiers like `--modifier control`."
    )]
    pub modifiers: Vec<UiKeyModifierArg>,
}

#[derive(Debug, Args)]
pub struct UiKeySequenceArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(required = true, help = "One or more raw platform key codes.")]
    pub keycodes: Vec<u32>,
}

#[derive(Debug, Args)]
pub struct UiPressButtonArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(value_enum, help = "Hardware button to press.")]
    pub button: UiHardwareButtonArg,

    #[arg(long, help = "Press duration like `500ms` or `0.5s`.")]
    pub duration: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiSelectMenuItemArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(help = "Menu path like `File > New Window`.")]
    pub path: String,
}

#[derive(Debug, Args)]
pub struct UiWaitUntilArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(
        long = "visible-text",
        help = "Require this accessibility text to appear."
    )]
    pub visible_text: Option<String>,

    #[arg(
        long = "visible-id",
        help = "Require this accessibility identifier to appear."
    )]
    pub visible_id: Option<String>,

    #[arg(
        long = "not-visible-text",
        help = "Require this accessibility text to disappear."
    )]
    pub not_visible_text: Option<String>,

    #[arg(
        long = "not-visible-id",
        help = "Require this accessibility identifier to disappear."
    )]
    pub not_visible_id: Option<String>,

    #[arg(long, help = "Maximum wait like `10s` or `10000ms`.")]
    pub timeout: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiWaitForAnimationToEndArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, help = "Maximum wait like `5s` or `5000ms`.")]
    pub timeout: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiTakeScreenshotArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(help = "Optional screenshot name or relative artifact path.")]
    pub name: Option<String>,
}

#[derive(Debug, Args)]
pub struct UiSetLocationArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, help = "Latitude in decimal degrees.")]
    pub latitude: f64,

    #[arg(long, help = "Longitude in decimal degrees.")]
    pub longitude: f64,
}

#[derive(Debug, Args)]
pub struct UiSetPermissionsArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(long, help = "Override the default manifest app bundle identifier.")]
    pub app_id: Option<String>,

    #[arg(
        long = "permission",
        required = true,
        help = "Repeat `--permission name=allow|deny|unset`."
    )]
    pub permissions: Vec<String>,
}

#[derive(Debug, Args)]
pub struct UiTravelArgs {
    #[command(flatten)]
    pub runtime: UiPlatformOnlyArgs,

    #[arg(
        long = "point",
        required = true,
        help = "Repeat `--point lat,lon` for at least two coordinates."
    )]
    pub points: Vec<String>,

    #[arg(long, help = "Meters per second.")]
    pub speed: Option<f64>,
}

#[derive(Debug, Args)]
#[command(
    about = "Write a starter JSON UI flow file.",
    long_about = "Write a starter `tests.ui` flow as JSON with the required `$schema` and `steps` keys.\n\nBy default Orbi infers `appId` from the manifest bundle identifier and `name` from the file stem.",
    after_help = "Examples:\n  orbi ui init Tests/UI/login.json\n  orbi ui init Tests/UI/login.json --name Login\n  orbi ui init Tests/UI/login.json --app-id dev.orbi.example.app"
)]
pub struct UiInitArgs {
    #[arg(help = "Path to the JSON flow file to create, relative to the project root.")]
    pub path: PathBuf,

    #[arg(
        long,
        help = "Override the default flow name. Defaults to the file stem."
    )]
    pub name: Option<String>,

    #[arg(
        long,
        help = "Override the default app bundle identifier from the manifest."
    )]
    pub app_id: Option<String>,

    #[arg(long, help = "Overwrite the destination file if it already exists.")]
    pub force: bool,
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
#[command(
    about = "Remove stale `xctrace` / Instruments `.ktrace` temp files from the current user's temp directory.",
    long_about = "Orbi's macOS trace runs use `xctrace`, which can leave large `instruments*.ktrace` files behind in the current user's temp directory after interrupted sessions.\n\nThis command removes those temp files from `std::env::temp_dir()`. By default it only removes files older than one hour. Pass `--all` to remove every matching temp trace in that directory.",
    after_help = "Examples:\n  orbi ui clean-trace-temp\n  orbi ui clean-trace-temp --stale-minutes 15\n  orbi ui clean-trace-temp --all"
)]
pub struct UiCleanTraceTempArgs {
    #[arg(
        long,
        conflicts_with = "stale_minutes",
        help = "Remove every matching `instruments*.ktrace` file in the current temp directory, regardless of age."
    )]
    pub all: bool,

    #[arg(long, help = "Remove only temp traces older than this many minutes.")]
    pub stale_minutes: Option<u64>,
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
        help = "Extra log-tool arguments forwarded after Orbi attaches to the selected target."
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
        help = "Extra arguments forwarded to Instruments after Orbi sets up the run."
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
    after_help = "Examples:\n  orbi run --platform ios --simulator\n  orbi run --platform ios --device --debug\n  orbi run --platform ios --simulator --trace"
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
    after_help = "Examples:\n  orbi build --platform ios --distribution development\n  orbi build --platform ios --distribution app-store --release\n  orbi build --platform macos --distribution developer-id --release\n  orbi build --platform macos --distribution mac-app-store --release"
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
        help = "Write the produced artifact to this path instead of Orbi's default receipt location."
    )]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(
    about = "Upload a previously built artifact to Apple services.",
    long_about = "Use `submit` only when the user explicitly wants a real remote submission. Orbi can derive the latest matching receipt, or you can pass one explicitly with `--receipt`.",
    after_help = "Examples:\n  orbi submit --platform ios --wait\n  orbi submit --receipt .orbi/receipts/<receipt>.json --wait"
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
        help = "Submit this explicit receipt instead of picking the latest matching one from `.orbi/receipts`."
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
    about = "Remove local and/or remote Orbi-managed state.",
    long_about = "`orbi clean` is intentionally destructive. Use `--all` only when you mean to remove both local Orbi state and Orbi-managed remote Apple state."
)]
pub struct CleanArgs {
    #[arg(
        long,
        conflicts_with_all = ["apple", "all"],
        help = "Remove only local Orbi state such as `.orbi/` artifacts and caches."
    )]
    pub local: bool,

    #[arg(
        long,
        conflicts_with_all = ["local", "all"],
        help = "Remove only Orbi-managed remote Apple state."
    )]
    pub apple: bool,

    #[arg(
        long,
        conflicts_with_all = ["local", "apple"],
        help = "Remove both local Orbi state and Orbi-managed remote Apple state."
    )]
    pub all: bool,
}

#[derive(Debug, Args)]
#[command(
    about = "App Store Connect auth, device, signing, and submission workflows backed by the embedded `asc` section in `orbi.json`.",
    after_help = "More help:\n  Use `orbi asc <command> --help` for flags, arguments, and command-specific examples.\n  For example:\n    orbi asc device add --help\n    orbi asc submit --help\n    orbi asc signing merge --help\n\nCommon workflows:\n  Check the embedded ASC config:\n    orbi asc validate\n    orbi asc plan\n\n  Apply ASC-managed signing state:\n    orbi asc apply\n\n  Add the current Mac as a development device and refresh profiles:\n    orbi asc device add-local --current-mac --apply\n\n  Register an iPhone and refresh profiles:\n    orbi asc device add --name \"My iPhone\" --apply\n\n  Print Xcode-style build settings from installed profiles:\n    orbi asc signing print-build-settings\n\n  Submit an artifact directly through ASC:\n    orbi asc submit --file build/MyApp.ipa\n\n  Notarize a Developer ID artifact:\n    orbi asc notarize --file build/MyApp.dmg"
)]
#[command(arg_required_else_help = true)]
pub struct AscArgs {
    #[command(subcommand)]
    pub command: AscCommand,
}

#[derive(Debug, Subcommand)]
pub enum AscCommand {
    Auth {
        #[command(subcommand)]
        command: AscAuthCommand,
    },
    Device {
        #[command(subcommand)]
        command: AscDeviceCommand,
    },
    Validate,
    Plan,
    Apply,
    Revoke(AscRevokeArgs),
    Submit(AscDirectSubmitArgs),
    Notarize(AscNotarizeArgs),
    Signing {
        #[command(subcommand)]
        command: AscSigningCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AscAuthCommand {
    Import,
}

#[derive(Debug, Subcommand)]
pub enum AscDeviceCommand {
    Add(AscDeviceAddArgs),
    AddLocal(AscDeviceAddLocalArgs),
}

#[derive(Debug, Args)]
pub struct AscDeviceAddArgs {
    #[arg(long, value_name = "NAME")]
    pub name: String,
    #[arg(long, value_name = "LOGICAL_ID")]
    pub id: Option<String>,
    #[arg(long, value_enum)]
    pub family: Option<AscDeviceFamily>,
    #[arg(long, default_value_t = false)]
    pub apply: bool,
    #[arg(long, default_value_t = 300)]
    pub timeout_seconds: u64,
}

#[derive(Debug, Args)]
pub struct AscDeviceAddLocalArgs {
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,
    #[arg(long, value_name = "LOGICAL_ID")]
    pub id: Option<String>,
    #[arg(long, default_value_t = false)]
    pub current_mac: bool,
    #[arg(long, value_enum)]
    pub family: Option<AscDeviceFamily>,
    #[arg(long, value_name = "UDID")]
    pub udid: Option<String>,
    #[arg(long, default_value_t = false)]
    pub apply: bool,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum AscDeviceFamily {
    #[value(name = "ios")]
    Ios,
    #[value(name = "ipados")]
    Ipados,
    #[value(name = "watchos")]
    Watchos,
    #[value(name = "tvos")]
    Tvos,
    #[value(name = "visionos")]
    Visionos,
    #[value(name = "macos")]
    Macos,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum AscRevokeTarget {
    Dev,
    Release,
    All,
}

#[derive(Debug, Args)]
pub struct AscRevokeArgs {
    #[arg(value_enum)]
    pub target: AscRevokeTarget,
}

#[derive(Debug, Args)]
pub struct AscDirectSubmitArgs {
    #[arg(long, value_name = "FILE")]
    pub file: PathBuf,
    #[arg(long = "bundle-id", value_name = "LOGICAL_ID")]
    pub bundle_id: Option<String>,
}

#[derive(Debug, Args)]
pub struct AscNotarizeArgs {
    #[arg(long, value_name = "FILE")]
    pub file: PathBuf,
}

#[derive(Debug, Subcommand)]
pub enum AscSigningCommand {
    Import,
    PrintBuildSettings,
    Merge(AscSigningMergeArgs),
}

#[derive(Debug, Args)]
pub struct AscSigningMergeArgs {
    #[arg(long, value_name = "FILE")]
    pub base: PathBuf,
    #[arg(long, value_name = "FILE")]
    pub ours: PathBuf,
    #[arg(long, value_name = "FILE")]
    pub theirs: PathBuf,
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
        help = "Signed `.dmg` for notarized macOS distribution outside the Mac App Store."
    )]
    DeveloperId,
    #[value(
        name = "mac-app-store",
        help = "Signed `.app` bundle for Mac App Store upload."
    )]
    MacAppStore,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum UiSwipeDirectionArg {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum UiKeyModifierArg {
    Command,
    Shift,
    Option,
    Control,
    Function,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum UiHardwareButtonArg {
    ApplePay,
    Home,
    Lock,
    SideButton,
    Siri,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Cli;

    #[test]
    fn top_level_help_stays_focused_on_core_workflow() {
        let mut command = Cli::command();
        let help = command.render_long_help().to_string();

        assert!(help.contains("orbi ui init Tests/UI/login.json"));
        assert!(help.contains("orbi preview shot Basic --platform ios"));
        assert!(help.contains("orbi test --ui --platform macos --flow onboarding-provider-setup"));
        assert!(
            help.contains(
                "orbi test --ui --platform macos --trace --flow onboarding-provider-setup"
            )
        );
        assert!(help.contains("orbi submit --platform ios --wait"));
        assert!(help.contains("Every command supports `--help`"));
        assert!(help.contains("Recommended UI Workflow:"));
        assert!(help.contains("Write Swift and optional backend unit tests"));
        assert!(
            help.contains("Check that the interface looks right with a SwiftUI preview screenshot")
        );
        assert!(help.contains("Run a final trace pass"));
        assert!(help.contains("orbi inspect-trace .orbi/artifacts/profiles/run.trace"));
        assert!(!help.contains("Common commands:"));
        assert!(!help.contains("query the UI test dialect"));
        assert!(!help.contains("\n  bsp"));
    }

    #[test]
    fn ui_help_includes_init_subcommand() {
        let mut command = Cli::command();
        let ui = command.find_subcommand_mut("ui").unwrap();
        let help = ui.render_long_help().to_string();

        assert!(help.contains("init"));
        assert!(help.contains("tap"));
        assert!(help.contains("swipe"));
        assert!(help.contains("clean-trace-temp"));
    }

    #[test]
    fn build_help_explains_distribution_choices() {
        let mut command = Cli::command();
        let build = command.find_subcommand_mut("build").unwrap();
        let help = build.render_long_help().to_string();

        assert!(help.contains("Select the packaging and signing mode"));
        assert!(help.contains("App Store and TestFlight upload artifacts"));
        assert!(help.contains("developer-id"));
    }

    #[test]
    fn asc_help_surfaces_workflows_and_command_help_hint() {
        let mut command = Cli::command();
        let asc = command.find_subcommand_mut("asc").unwrap();
        let help = asc.render_long_help().to_string();

        assert!(help.contains("App Store Connect auth, device, signing, and submission workflows"));
        assert!(help.contains("More help:"));
        assert!(help.contains("Common workflows:"));
        assert!(help.contains("orbi asc validate"));
        assert!(help.contains("orbi asc apply"));
        assert!(help.contains("orbi asc device add --name \"My iPhone\" --apply"));
        assert!(help.contains("Use `orbi asc <command> --help`"));
        assert!(help.contains("orbi asc submit --help"));
    }

    #[test]
    fn ui_open_help_describes_the_url_argument() {
        let mut command = Cli::command();
        let ui = command.find_subcommand_mut("ui").unwrap();
        let open = ui.find_subcommand_mut("open").unwrap();
        let help = open.render_long_help().to_string();

        assert!(help.contains("URL or deep link to open"));
        assert!(help.contains("Select a platform when Orbi cannot infer one"));
    }
}
