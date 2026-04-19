use anyhow::{Context, Result, bail};

use crate::apple::runtime;
use crate::apple::testing::ui::{
    UiCommand, UiDragAndDrop, UiElementScroll, UiElementSwipe, UiExtendedWaitUntil,
    UiHardwareButton, UiKeyModifier, UiKeyPress, UiLaunchApp, UiLocationPoint, UiPermissionConfig,
    UiPermissionSetting, UiPermissionState, UiPointExpr, UiPressKey, UiScrollUntilVisible,
    UiSelector, UiSwipe, UiSwipeDirection, UiTravel,
};
use crate::cli::{
    UiAppTargetArgs, UiDragArgs, UiEraseTextArgs, UiHardwareButtonArg, UiInputTextArgs,
    UiKeyModifierArg, UiKeySequenceArgs, UiLaunchAppArgs, UiLongPressArgs, UiPressButtonArgs,
    UiPressKeyArgs, UiPressKeyCodeArgs, UiScrollArgs, UiScrollOnArgs, UiScrollUntilVisibleArgs,
    UiSelectMenuItemArgs, UiSelectorActionArgs, UiSelectorArgs, UiSetLocationArgs,
    UiSetPermissionsArgs, UiSwipeArgs, UiSwipeDirectionArg, UiSwipeOnArgs, UiTakeScreenshotArgs,
    UiTapPointArgs, UiTravelArgs, UiWaitForAnimationToEndArgs, UiWaitUntilArgs,
};
use crate::manifest::ApplePlatform;

pub(super) struct DirectUiCommand {
    pub(super) platform: Option<ApplePlatform>,
    pub(super) focus_after_launch: bool,
    pub(super) command: UiCommand,
}

pub(super) fn launch_app(args: &UiLaunchAppArgs) -> Result<DirectUiCommand> {
    let permissions = if args.permissions.is_empty() {
        None
    } else {
        Some(parse_permissions(args.app_id.clone(), &args.permissions)?)
    };

    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: args.focus,
        command: UiCommand::LaunchApp(UiLaunchApp {
            app_id: args.app_id.clone(),
            clear_state: args.clear_state,
            clear_keychain: args.clear_keychain,
            stop_app: args.stop_app,
            permissions,
            arguments: parse_launch_arguments(&args.arguments)?,
        }),
    })
}

pub(super) fn stop_app(args: &UiAppTargetArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::StopApp(args.app_id.clone()),
    }
}

pub(super) fn kill_app(args: &UiAppTargetArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::KillApp(args.app_id.clone()),
    }
}

pub(super) fn clear_state(args: &UiAppTargetArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::ClearState(args.app_id.clone()),
    }
}

pub(super) fn clear_keychain(platform: Option<ApplePlatform>) -> DirectUiCommand {
    DirectUiCommand {
        platform,
        focus_after_launch: false,
        command: UiCommand::ClearKeychain,
    }
}

pub(super) fn tap(args: &UiSelectorActionArgs) -> Result<DirectUiCommand> {
    selector_action(args, UiCommand::TapOn)
}

pub(super) fn hover(args: &UiSelectorActionArgs) -> Result<DirectUiCommand> {
    selector_action(args, UiCommand::HoverOn)
}

pub(super) fn right_click(args: &UiSelectorActionArgs) -> Result<DirectUiCommand> {
    selector_action(args, UiCommand::RightClickOn)
}

pub(super) fn double_tap(args: &UiSelectorActionArgs) -> Result<DirectUiCommand> {
    selector_action(args, UiCommand::DoubleTapOn)
}

pub(super) fn assert_visible(args: &UiSelectorActionArgs) -> Result<DirectUiCommand> {
    selector_action(args, UiCommand::AssertVisible)
}

pub(super) fn assert_not_visible(args: &UiSelectorActionArgs) -> Result<DirectUiCommand> {
    selector_action(args, UiCommand::AssertNotVisible)
}

pub(super) fn tap_point(args: &UiTapPointArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::TapOnPoint(parse_point_expr(&args.point)?),
    })
}

pub(super) fn long_press(args: &UiLongPressArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::LongPressOn {
            target: selector_from_args(&args.selector, "long-press")?,
            duration_ms: parse_duration_ms(args.duration.as_deref())?.unwrap_or(1_500),
        },
    })
}

pub(super) fn swipe(args: &UiSwipeArgs) -> Result<DirectUiCommand> {
    let command = match (args.direction, args.start.as_deref(), args.end.as_deref()) {
        (Some(direction), None, None) => UiCommand::Swipe(default_swipe_for_direction(direction)?),
        (None, Some(start), Some(end)) => UiCommand::Swipe(UiSwipe {
            start: parse_point_expr(start)?,
            end: parse_point_expr(end)?,
            duration_ms: parse_duration_ms(args.duration.as_deref())?,
            delta: args.delta,
        }),
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            bail!("`swipe` accepts either `--direction` or `--start/--end`, not both");
        }
        _ => bail!("`swipe` requires `--direction` or both `--start` and `--end`"),
    };

    let UiCommand::Swipe(mut swipe) = command else {
        unreachable!("swipe command builder always returns `UiCommand::Swipe`");
    };
    if args.direction.is_some() {
        swipe.duration_ms = parse_duration_ms(args.duration.as_deref())?;
        swipe.delta = args.delta;
    }

    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::Swipe(swipe),
    })
}

pub(super) fn swipe_on(args: &UiSwipeOnArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::SwipeOn(UiElementSwipe {
            target: selector_from_args(&args.selector, "swipe-on")?,
            direction: map_direction(args.direction),
            duration_ms: parse_duration_ms(args.duration.as_deref())?,
            delta: args.delta,
        }),
    })
}

pub(super) fn drag(args: &UiDragArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::DragAndDrop(UiDragAndDrop {
            source: selector_from_parts(
                args.from_text.as_deref(),
                args.from_id.as_deref(),
                "drag source",
            )?,
            destination: selector_from_parts(
                args.to_text.as_deref(),
                args.to_id.as_deref(),
                "drag destination",
            )?,
            duration_ms: parse_duration_ms(args.duration.as_deref())?,
            delta: args.delta,
        }),
    })
}

pub(super) fn scroll(args: &UiScrollArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::Scroll(map_direction(args.direction)),
    }
}

pub(super) fn scroll_on(args: &UiScrollOnArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::ScrollOn(UiElementScroll {
            target: selector_from_args(&args.selector, "scroll-on")?,
            direction: map_direction(args.direction),
        }),
    })
}

pub(super) fn scroll_until_visible(args: &UiScrollUntilVisibleArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::ScrollUntilVisible(UiScrollUntilVisible {
            target: selector_from_args(&args.selector, "scroll-until-visible")?,
            direction: map_direction(args.direction),
            timeout_ms: parse_duration_ms(args.timeout.as_deref())?.unwrap_or(20_000),
        }),
    })
}

pub(super) fn input_text(args: &UiInputTextArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::InputText(args.text.clone()),
    }
}

pub(super) fn erase_text(args: &UiEraseTextArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::EraseText(args.characters),
    }
}

pub(super) fn press_key(args: &UiPressKeyArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::PressKey(UiKeyPress {
            key: parse_press_key(&args.key)?,
            modifiers: args.modifiers.iter().copied().map(map_modifier).collect(),
        }),
    })
}

pub(super) fn press_key_code(args: &UiPressKeyCodeArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::PressKeyCode {
            keycode: args.keycode,
            duration_ms: parse_duration_ms(args.duration.as_deref())?,
            modifiers: args.modifiers.iter().copied().map(map_modifier).collect(),
        },
    })
}

pub(super) fn key_sequence(args: &UiKeySequenceArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::KeySequence(args.keycodes.clone()),
    }
}

pub(super) fn press_button(args: &UiPressButtonArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::PressButton {
            button: map_button(args.button),
            duration_ms: parse_duration_ms(args.duration.as_deref())?,
        },
    })
}

pub(super) fn select_menu_item(args: &UiSelectMenuItemArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::SelectMenuItem(parse_menu_path(&args.path)?),
    })
}

pub(super) fn hide_keyboard(platform: Option<ApplePlatform>) -> DirectUiCommand {
    DirectUiCommand {
        platform,
        focus_after_launch: false,
        command: UiCommand::HideKeyboard,
    }
}

pub(super) fn wait_until(args: &UiWaitUntilArgs) -> Result<DirectUiCommand> {
    let visible = selector_from_parts(
        args.visible_text.as_deref(),
        args.visible_id.as_deref(),
        "wait-until visible selector",
    )
    .ok();
    let not_visible = selector_from_parts(
        args.not_visible_text.as_deref(),
        args.not_visible_id.as_deref(),
        "wait-until hidden selector",
    )
    .ok();
    if visible.is_none() && not_visible.is_none() {
        bail!("`wait-until` requires a visible and/or not-visible selector");
    }

    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::ExtendedWaitUntil(UiExtendedWaitUntil {
            visible,
            not_visible,
            timeout_ms: parse_duration_ms(args.timeout.as_deref())?.unwrap_or(10_000),
        }),
    })
}

pub(super) fn wait_for_animation_to_end(
    args: &UiWaitForAnimationToEndArgs,
) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::WaitForAnimationToEnd(
            parse_duration_ms(args.timeout.as_deref())?.unwrap_or(5_000),
        ),
    })
}

pub(super) fn take_screenshot(args: &UiTakeScreenshotArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::TakeScreenshot(args.name.clone()),
    }
}

pub(super) fn set_location(args: &UiSetLocationArgs) -> DirectUiCommand {
    DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::SetLocation {
            latitude: args.latitude,
            longitude: args.longitude,
        },
    }
}

pub(super) fn set_permissions(args: &UiSetPermissionsArgs) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::SetPermissions(parse_permissions(
            args.app_id.clone(),
            &args.permissions,
        )?),
    })
}

pub(super) fn travel(args: &UiTravelArgs) -> Result<DirectUiCommand> {
    if args.points.len() < 2 {
        bail!("`travel` requires at least two `--point` values");
    }

    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: UiCommand::Travel(UiTravel {
            points: args
                .points
                .iter()
                .map(|point| parse_location_point(point))
                .collect::<Result<Vec<_>>>()?,
            speed_meters_per_second: args.speed,
        }),
    })
}

fn selector_action(
    args: &UiSelectorActionArgs,
    build: impl FnOnce(UiSelector) -> UiCommand,
) -> Result<DirectUiCommand> {
    Ok(DirectUiCommand {
        platform: platform_from_cli(args.runtime.platform),
        focus_after_launch: false,
        command: build(selector_from_args(&args.selector, "selector command")?),
    })
}

fn selector_from_args(args: &UiSelectorArgs, context: &str) -> Result<UiSelector> {
    selector_from_parts(args.text.as_deref(), args.id.as_deref(), context)
}

fn selector_from_parts(text: Option<&str>, id: Option<&str>, context: &str) -> Result<UiSelector> {
    let text = text.map(str::trim).filter(|value| !value.is_empty());
    let id = id.map(str::trim).filter(|value| !value.is_empty());
    if text.is_none() && id.is_none() {
        bail!("`{context}` requires `--text` and/or `--id`");
    }
    Ok(UiSelector {
        text: text.map(str::to_owned),
        id: id.map(str::to_owned),
    })
}

fn parse_launch_arguments(arguments: &[String]) -> Result<Vec<(String, String)>> {
    arguments
        .iter()
        .map(|entry| split_kv(entry, "launch argument"))
        .collect()
}

fn parse_permissions(app_id: Option<String>, permissions: &[String]) -> Result<UiPermissionConfig> {
    let permissions = permissions
        .iter()
        .map(|entry| {
            let (name, state) = split_kv(entry, "permission")?;
            Ok(UiPermissionSetting {
                name,
                state: match state.to_ascii_lowercase().as_str() {
                    "allow" => UiPermissionState::Allow,
                    "deny" => UiPermissionState::Deny,
                    "unset" => UiPermissionState::Unset,
                    other => bail!("unsupported permission state `{other}`"),
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if permissions.is_empty() {
        bail!("at least one permission entry is required");
    }
    Ok(UiPermissionConfig {
        app_id,
        permissions,
    })
}

fn split_kv(entry: &str, kind: &str) -> Result<(String, String)> {
    let (key, value) = entry
        .split_once('=')
        .with_context(|| format!("{kind} `{entry}` must use `key=value`"))?;
    let key = key.trim();
    let value = value.trim();
    if key.is_empty() || value.is_empty() {
        bail!("{kind} `{entry}` must use non-empty `key=value` segments");
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn parse_menu_path(path: &str) -> Result<Vec<String>> {
    let items = path
        .split('>')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if items.is_empty() {
        bail!("menu path must contain at least one segment");
    }
    Ok(items)
}

fn parse_point_expr(value: &str) -> Result<UiPointExpr> {
    let mut segments = value.split(',').map(str::trim);
    let x = segments
        .next()
        .context("point expressions must include an x coordinate")?;
    let y = segments
        .next()
        .context("point expressions must include a y coordinate")?;
    if segments.next().is_some() {
        bail!("point expressions must contain exactly two coordinates");
    }
    Ok(UiPointExpr {
        x: parse_coordinate(x)?,
        y: parse_coordinate(y)?,
    })
}

fn parse_coordinate(value: &str) -> Result<crate::apple::testing::ui::UiCoordinate> {
    let trimmed = value.trim();
    if let Some(percent) = trimmed.strip_suffix('%') {
        return Ok(crate::apple::testing::ui::UiCoordinate::Percent(
            percent
                .trim()
                .parse()
                .with_context(|| format!("invalid percentage coordinate `{trimmed}`"))?,
        ));
    }
    Ok(crate::apple::testing::ui::UiCoordinate::Absolute(
        trimmed
            .parse()
            .with_context(|| format!("invalid absolute coordinate `{trimmed}`"))?,
    ))
}

fn parse_duration_ms(value: Option<&str>) -> Result<Option<u32>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if let Some(ms) = trimmed.strip_suffix("ms") {
        return Ok(Some(
            ms.trim()
                .parse()
                .with_context(|| format!("invalid duration `{trimmed}`"))?,
        ));
    }
    if let Some(seconds) = trimmed.strip_suffix('s') {
        let seconds = seconds
            .trim()
            .parse::<f64>()
            .with_context(|| format!("invalid duration `{trimmed}`"))?;
        if seconds.is_sign_negative() {
            bail!("duration must not be negative");
        }
        return Ok(Some((seconds * 1000.0).round() as u32));
    }
    Ok(Some(trimmed.parse().with_context(|| {
        format!("invalid duration `{trimmed}`")
    })?))
}

fn parse_press_key(value: &str) -> Result<UiPressKey> {
    let token = value.trim();
    let uppercase = token.to_ascii_uppercase();
    match uppercase.as_str() {
        "HOME" => Ok(UiPressKey::Home),
        "LOCK" => Ok(UiPressKey::Lock),
        "ENTER" | "RETURN" => Ok(UiPressKey::Enter),
        "BACKSPACE" | "DELETE" => Ok(UiPressKey::Backspace),
        "ESCAPE" | "ESC" => Ok(UiPressKey::Escape),
        "SPACE" => Ok(UiPressKey::Space),
        "TAB" => Ok(UiPressKey::Tab),
        "VOLUME_UP" => Ok(UiPressKey::VolumeUp),
        "VOLUME_DOWN" => Ok(UiPressKey::VolumeDown),
        "BACK" => Ok(UiPressKey::Back),
        "POWER" => Ok(UiPressKey::Power),
        "LEFT" | "LEFT_ARROW" => Ok(UiPressKey::LeftArrow),
        "RIGHT" | "RIGHT_ARROW" => Ok(UiPressKey::RightArrow),
        "UP" | "UP_ARROW" => Ok(UiPressKey::UpArrow),
        "DOWN" | "DOWN_ARROW" => Ok(UiPressKey::DownArrow),
        _ if token.chars().count() == 1 => Ok(UiPressKey::Character(
            token.chars().next().expect("count already checked"),
        )),
        other => bail!("unsupported `press-key` value `{other}`"),
    }
}

fn parse_location_point(value: &str) -> Result<UiLocationPoint> {
    let (latitude, longitude) = value
        .split_once(',')
        .with_context(|| format!("travel points must be `lat,lon`, got `{value}`"))?;
    Ok(UiLocationPoint {
        latitude: latitude
            .trim()
            .parse()
            .with_context(|| format!("invalid latitude in `{value}`"))?,
        longitude: longitude
            .trim()
            .parse()
            .with_context(|| format!("invalid longitude in `{value}`"))?,
    })
}

fn default_swipe_for_direction(direction: UiSwipeDirectionArg) -> Result<UiSwipe> {
    let (start, end) = match direction {
        UiSwipeDirectionArg::Left => ("90%, 50%", "10%, 50%"),
        UiSwipeDirectionArg::Right => ("10%, 50%", "90%, 50%"),
        UiSwipeDirectionArg::Up => ("50%, 50%", "50%, 10%"),
        UiSwipeDirectionArg::Down => ("50%, 20%", "50%, 90%"),
    };
    Ok(UiSwipe {
        start: parse_point_expr(start)?,
        end: parse_point_expr(end)?,
        duration_ms: None,
        delta: None,
    })
}

fn platform_from_cli(platform: Option<crate::cli::TargetPlatform>) -> Option<ApplePlatform> {
    platform.map(runtime::apple_platform_from_cli)
}

fn map_direction(direction: UiSwipeDirectionArg) -> UiSwipeDirection {
    match direction {
        UiSwipeDirectionArg::Left => UiSwipeDirection::Left,
        UiSwipeDirectionArg::Right => UiSwipeDirection::Right,
        UiSwipeDirectionArg::Up => UiSwipeDirection::Up,
        UiSwipeDirectionArg::Down => UiSwipeDirection::Down,
    }
}

fn map_modifier(modifier: UiKeyModifierArg) -> UiKeyModifier {
    match modifier {
        UiKeyModifierArg::Command => UiKeyModifier::Command,
        UiKeyModifierArg::Shift => UiKeyModifier::Shift,
        UiKeyModifierArg::Option => UiKeyModifier::Option,
        UiKeyModifierArg::Control => UiKeyModifier::Control,
        UiKeyModifierArg::Function => UiKeyModifier::Function,
    }
}

fn map_button(button: UiHardwareButtonArg) -> UiHardwareButton {
    match button {
        UiHardwareButtonArg::ApplePay => UiHardwareButton::ApplePay,
        UiHardwareButtonArg::Home => UiHardwareButton::Home,
        UiHardwareButtonArg::Lock => UiHardwareButton::Lock,
        UiHardwareButtonArg::SideButton => UiHardwareButton::SideButton,
        UiHardwareButtonArg::Siri => UiHardwareButton::Siri,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_menu_path, parse_point_expr, parse_press_key, split_kv};

    #[test]
    fn parses_key_value_pairs() {
        let parsed = split_kv("seedUser=qa@example.com", "launch argument").unwrap();
        assert_eq!(parsed.0, "seedUser");
        assert_eq!(parsed.1, "qa@example.com");
    }

    #[test]
    fn parses_menu_paths() {
        let parsed = parse_menu_path("File > New Window").unwrap();
        assert_eq!(parsed, vec!["File", "New Window"]);
    }

    #[test]
    fn parses_point_expressions() {
        let point = parse_point_expr("50%, 10%").unwrap();
        assert!(matches!(
            point.x,
            crate::apple::testing::ui::UiCoordinate::Percent(50.0)
        ));
        assert!(matches!(
            point.y,
            crate::apple::testing::ui::UiCoordinate::Percent(10.0)
        ));
    }

    #[test]
    fn parses_named_keys() {
        let key = parse_press_key("left").unwrap();
        assert!(matches!(
            key,
            crate::apple::testing::ui::UiPressKey::LeftArrow
        ));
    }
}
