use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct UiFlow {
    pub path: PathBuf,
    pub config: UiFlowConfig,
    pub commands: Vec<UiCommand>,
}

#[derive(Debug, Clone, Default)]
pub struct UiFlowConfig {
    pub app_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    LaunchApp(UiLaunchApp),
    StopApp(Option<String>),
    KillApp(Option<String>),
    ClearState(Option<String>),
    ClearKeychain,
    TapOn(UiSelector),
    HoverOn(UiSelector),
    RightClickOn(UiSelector),
    TapOnPoint(UiPointExpr),
    DoubleTapOn(UiSelector),
    LongPressOn {
        target: UiSelector,
        duration_ms: u32,
    },
    Swipe(UiSwipe),
    SwipeOn(UiElementSwipe),
    DragAndDrop(UiDragAndDrop),
    Scroll(UiSwipeDirection),
    ScrollOn(UiElementScroll),
    ScrollUntilVisible(UiScrollUntilVisible),
    InputText(String),
    PasteText,
    SetClipboard(String),
    CopyTextFrom(UiSelector),
    EraseText(u32),
    PressKey(UiKeyPress),
    PressKeyCode {
        keycode: u32,
        duration_ms: Option<u32>,
        modifiers: Vec<UiKeyModifier>,
    },
    KeySequence(Vec<u32>),
    PressButton {
        button: UiHardwareButton,
        duration_ms: Option<u32>,
    },
    SelectMenuItem(Vec<String>),
    HideKeyboard,
    AssertVisible(UiSelector),
    AssertNotVisible(UiSelector),
    ExtendedWaitUntil(UiExtendedWaitUntil),
    WaitForAnimationToEnd(u32),
    TakeScreenshot(Option<String>),
    StartRecording(Option<String>),
    StopRecording,
    OpenLink(String),
    SetLocation {
        latitude: f64,
        longitude: f64,
    },
    SetPermissions(UiPermissionConfig),
    Travel(UiTravel),
    AddMedia(Vec<PathBuf>),
    RunFlow(PathBuf),
    Repeat {
        times: u32,
        commands: Vec<UiCommand>,
    },
    Retry {
        times: u32,
        commands: Vec<UiCommand>,
    },
}

#[derive(Debug, Clone)]
pub struct UiSwipe {
    pub start: UiPointExpr,
    pub end: UiPointExpr,
    pub duration_ms: Option<u32>,
    pub delta: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UiElementSwipe {
    pub target: UiSelector,
    pub direction: UiSwipeDirection,
    pub duration_ms: Option<u32>,
    pub delta: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UiElementScroll {
    pub target: UiSelector,
    pub direction: UiSwipeDirection,
}

#[derive(Debug, Clone)]
pub struct UiDragAndDrop {
    pub source: UiSelector,
    pub destination: UiSelector,
    pub duration_ms: Option<u32>,
    pub delta: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct UiPointExpr {
    pub x: UiCoordinate,
    pub y: UiCoordinate,
}

#[derive(Debug, Clone, Copy)]
pub enum UiCoordinate {
    Absolute(f64),
    Percent(f64),
}

#[derive(Debug, Clone, Copy)]
pub enum UiSwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone)]
pub struct UiScrollUntilVisible {
    pub target: UiSelector,
    pub direction: UiSwipeDirection,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone, Default)]
pub struct UiLaunchApp {
    pub app_id: Option<String>,
    pub clear_state: bool,
    pub clear_keychain: bool,
    pub stop_app: bool,
    pub permissions: Option<UiPermissionConfig>,
    pub arguments: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct UiExtendedWaitUntil {
    pub visible: Option<UiSelector>,
    pub not_visible: Option<UiSelector>,
    pub timeout_ms: u32,
}

#[derive(Debug, Clone)]
pub struct UiPermissionConfig {
    pub app_id: Option<String>,
    pub permissions: Vec<UiPermissionSetting>,
}

#[derive(Debug, Clone)]
pub struct UiPermissionSetting {
    pub name: String,
    pub state: UiPermissionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPermissionState {
    Allow,
    Deny,
    Unset,
}

#[derive(Debug, Clone)]
pub struct UiSelector {
    pub text: Option<String>,
    pub id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UiKeyPress {
    pub key: UiPressKey,
    pub modifiers: Vec<UiKeyModifier>,
}

impl UiKeyPress {
    pub fn plain(key: UiPressKey) -> Self {
        Self {
            key,
            modifiers: Vec::new(),
        }
    }

    pub(crate) fn summary(&self) -> String {
        if self.modifiers.is_empty() {
            return self.key.summary();
        }

        let modifiers = self
            .modifiers
            .iter()
            .map(|modifier| modifier.summary())
            .collect::<Vec<_>>()
            .join("+");
        format!("{modifiers}+{}", self.key.summary())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiKeyModifier {
    Command,
    Shift,
    Option,
    Control,
    Function,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiPressKey {
    Home,
    Lock,
    Enter,
    Backspace,
    Escape,
    Space,
    VolumeUp,
    VolumeDown,
    Tab,
    Back,
    Power,
    LeftArrow,
    RightArrow,
    UpArrow,
    DownArrow,
    Character(char),
}

#[derive(Debug, Clone, Copy)]
pub enum UiHardwareButton {
    ApplePay,
    Home,
    Lock,
    SideButton,
    Siri,
}

#[derive(Debug, Clone)]
pub struct UiTravel {
    pub points: Vec<UiLocationPoint>,
    pub speed_meters_per_second: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct UiCrashQuery {
    pub before: Option<String>,
    pub since: Option<String>,
    pub bundle_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct UiCrashDeleteRequest {
    pub name: Option<String>,
    pub before: Option<String>,
    pub since: Option<String>,
    pub bundle_id: Option<String>,
    pub delete_all: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct UiLocationPoint {
    pub latitude: f64,
    pub longitude: f64,
}

impl UiCommand {
    pub(crate) fn summary(&self) -> String {
        match self {
            UiCommand::LaunchApp(command) => match command.app_id.as_deref() {
                Some(app_id) => format!("launchApp {app_id}"),
                None => "launchApp".to_owned(),
            },
            UiCommand::StopApp(app_id) => match app_id.as_deref() {
                Some(app_id) => format!("stopApp {app_id}"),
                None => "stopApp".to_owned(),
            },
            UiCommand::KillApp(app_id) => match app_id.as_deref() {
                Some(app_id) => format!("killApp {app_id}"),
                None => "killApp".to_owned(),
            },
            UiCommand::ClearState(app_id) => match app_id.as_deref() {
                Some(app_id) => format!("clearState {app_id}"),
                None => "clearState".to_owned(),
            },
            UiCommand::ClearKeychain => "clearKeychain".to_owned(),
            UiCommand::TapOn(target) => format!("tapOn {}", target.summary()),
            UiCommand::HoverOn(target) => format!("hoverOn {}", target.summary()),
            UiCommand::RightClickOn(target) => format!("rightClickOn {}", target.summary()),
            UiCommand::TapOnPoint(_) => "tapOnPoint".to_owned(),
            UiCommand::DoubleTapOn(target) => format!("doubleTapOn {}", target.summary()),
            UiCommand::LongPressOn { target, .. } => {
                format!("longPressOn {}", target.summary())
            }
            UiCommand::Swipe(_) => "swipe".to_owned(),
            UiCommand::SwipeOn(command) => {
                format!(
                    "swipeOn {} {:?}",
                    command.target.summary(),
                    command.direction
                )
            }
            UiCommand::DragAndDrop(command) => format!(
                "dragAndDrop {} -> {}",
                command.source.summary(),
                command.destination.summary()
            ),
            UiCommand::Scroll(direction) => format!("scroll {:?}", direction),
            UiCommand::ScrollOn(command) => {
                format!(
                    "scrollOn {} {:?}",
                    command.target.summary(),
                    command.direction
                )
            }
            UiCommand::ScrollUntilVisible(command) => {
                format!("scrollUntilVisible {}", command.target.summary())
            }
            UiCommand::InputText(text) => format!("inputText {}", preview_text(text)),
            UiCommand::PasteText => "pasteText".to_owned(),
            UiCommand::SetClipboard(text) => format!("setClipboard {}", preview_text(text)),
            UiCommand::CopyTextFrom(selector) => {
                format!("copyTextFrom {}", selector.summary())
            }
            UiCommand::EraseText(count) => format!("eraseText {count}"),
            UiCommand::PressKey(key) => format!("pressKey {}", key.summary()),
            UiCommand::PressKeyCode {
                keycode, modifiers, ..
            } => {
                if modifiers.is_empty() {
                    format!("pressKeyCode {keycode}")
                } else {
                    let modifiers = modifiers
                        .iter()
                        .map(|modifier| modifier.summary())
                        .collect::<Vec<_>>()
                        .join("+");
                    format!("pressKeyCode {modifiers}+{keycode}")
                }
            }
            UiCommand::KeySequence(keycodes) => format!("keySequence {}", keycodes.len()),
            UiCommand::PressButton { button, .. } => {
                format!("pressButton {}", button.summary())
            }
            UiCommand::SelectMenuItem(path) => {
                format!("selectMenuItem {}", path.join(" > "))
            }
            UiCommand::HideKeyboard => "hideKeyboard".to_owned(),
            UiCommand::AssertVisible(target) => {
                format!("assertVisible {}", target.summary())
            }
            UiCommand::AssertNotVisible(target) => {
                format!("assertNotVisible {}", target.summary())
            }
            UiCommand::ExtendedWaitUntil(command) => {
                if let Some(selector) = command.visible.as_ref() {
                    format!("extendedWaitUntil visible {}", selector.summary())
                } else if let Some(selector) = command.not_visible.as_ref() {
                    format!("extendedWaitUntil notVisible {}", selector.summary())
                } else {
                    "extendedWaitUntil".to_owned()
                }
            }
            UiCommand::WaitForAnimationToEnd(timeout_ms) => {
                format!("waitForAnimationToEnd {timeout_ms}ms")
            }
            UiCommand::TakeScreenshot(name) => match name {
                Some(name) => format!("takeScreenshot {name}"),
                None => "takeScreenshot".to_owned(),
            },
            UiCommand::StartRecording(path) => match path {
                Some(path) => format!("startRecording {path}"),
                None => "startRecording".to_owned(),
            },
            UiCommand::StopRecording => "stopRecording".to_owned(),
            UiCommand::OpenLink(url) => format!("openLink {url}"),
            UiCommand::SetLocation {
                latitude,
                longitude,
            } => format!("setLocation {latitude},{longitude}"),
            UiCommand::SetPermissions(command) => match command.app_id.as_deref() {
                Some(app_id) => format!("setPermissions {app_id}"),
                None => "setPermissions".to_owned(),
            },
            UiCommand::Travel(command) => format!("travel {}", command.points.len()),
            UiCommand::AddMedia(paths) => format!("addMedia {}", paths.len()),
            UiCommand::RunFlow(path) => format!("runFlow {}", path.display()),
            UiCommand::Repeat { times, .. } => format!("repeat {times}"),
            UiCommand::Retry { times, .. } => format!("retry {times}"),
        }
    }
}

impl UiSelector {
    pub(crate) fn summary(&self) -> String {
        match (self.text.as_deref(), self.id.as_deref()) {
            (Some(text), Some(id)) => format!("text={text}, id={id}"),
            (Some(text), None) => text.to_owned(),
            (None, Some(id)) => format!("id={id}"),
            (None, None) => "<selector>".to_owned(),
        }
    }
}

impl UiPressKey {
    pub(crate) fn summary(self) -> String {
        match self {
            UiPressKey::Home => "HOME".to_owned(),
            UiPressKey::Lock => "LOCK".to_owned(),
            UiPressKey::Enter => "ENTER".to_owned(),
            UiPressKey::Backspace => "BACKSPACE".to_owned(),
            UiPressKey::Escape => "ESCAPE".to_owned(),
            UiPressKey::Space => "SPACE".to_owned(),
            UiPressKey::VolumeUp => "VOLUME_UP".to_owned(),
            UiPressKey::VolumeDown => "VOLUME_DOWN".to_owned(),
            UiPressKey::Tab => "TAB".to_owned(),
            UiPressKey::Back => "BACK".to_owned(),
            UiPressKey::Power => "POWER".to_owned(),
            UiPressKey::LeftArrow => "LEFT".to_owned(),
            UiPressKey::RightArrow => "RIGHT".to_owned(),
            UiPressKey::UpArrow => "UP".to_owned(),
            UiPressKey::DownArrow => "DOWN".to_owned(),
            UiPressKey::Character(character) => character.to_ascii_uppercase().to_string(),
        }
    }
}

impl UiKeyModifier {
    pub(crate) fn summary(&self) -> &'static str {
        match self {
            UiKeyModifier::Command => "COMMAND",
            UiKeyModifier::Shift => "SHIFT",
            UiKeyModifier::Option => "OPTION",
            UiKeyModifier::Control => "CONTROL",
            UiKeyModifier::Function => "FUNCTION",
        }
    }
}

impl UiHardwareButton {
    pub(crate) fn summary(self) -> &'static str {
        match self {
            UiHardwareButton::ApplePay => "APPLE_PAY",
            UiHardwareButton::Home => "HOME",
            UiHardwareButton::Lock => "LOCK",
            UiHardwareButton::SideButton => "SIDE_BUTTON",
            UiHardwareButton::Siri => "SIRI",
        }
    }
}

fn preview_text(value: &str) -> String {
    const LIMIT: usize = 24;
    if value.chars().count() <= LIMIT {
        format!("\"{value}\"")
    } else {
        let preview = value.chars().take(LIMIT).collect::<String>();
        format!("\"{preview}...\"")
    }
}
