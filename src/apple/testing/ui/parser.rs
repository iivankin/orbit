use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;
use yaml_rust2::Yaml;
use yaml_rust2::yaml::Hash as YamlHash;

use super::schema::{FLOW_SCHEMA_FILENAME, supports_flow_schema};
use super::{
    UiCommand, UiCoordinate, UiDragAndDrop, UiElementScroll, UiElementSwipe, UiExtendedWaitUntil,
    UiFlow, UiFlowConfig, UiHardwareButton, UiKeyModifier, UiKeyPress, UiLaunchApp,
    UiLocationPoint, UiPermissionConfig, UiPermissionSetting, UiPermissionState, UiPointExpr,
    UiPressKey, UiScrollUntilVisible, UiSelector, UiSwipe, UiSwipeDirection, UiTravel,
};

pub fn parse_ui_flow(path: &Path) -> Result<UiFlow> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let document: JsonValue = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    let JsonValue::Object(root) = &document else {
        bail!(
            "`{}` must contain a single JSON object with `$schema` and `steps`",
            path.display()
        );
    };
    let schema = root
        .get("$schema")
        .context(format!("`{}` must declare `$schema`", path.display()))?
        .as_str()
        .context("`$schema` must be a string")?;
    if !supports_flow_schema(schema) {
        bail!(
            "unsupported UI flow schema `{schema}`; expected a local schema path ending with `{}` or a version-pinned published Orbi schema URL from `https://orbitstorage.dev/schemas/`",
            FLOW_SCHEMA_FILENAME
        );
    }

    let Yaml::Hash(root) = json_to_yaml(&document)? else {
        unreachable!("JSON objects always convert to YAML hashes");
    };
    let config = parse_config(&root)?;
    let Some(commands_value) = get_optional(&root, "steps") else {
        bail!("`{}` must declare `steps`", path.display());
    };
    let Yaml::Array(commands) = commands_value else {
        bail!("`steps` must be a JSON array");
    };
    let commands = parse_commands(commands)?;

    if commands.is_empty() {
        bail!("`{}` does not contain any UI commands", path.display());
    }

    Ok(UiFlow {
        path: path.to_path_buf(),
        config,
        commands,
    })
}

fn parse_config(map: &YamlHash) -> Result<UiFlowConfig> {
    let mut config = UiFlowConfig::default();
    for (key, value) in map {
        let key = yaml_string(key).context("flow configuration keys must be strings")?;
        match key {
            "$schema" => {}
            "appId" => config.app_id = Some(yaml_string(value)?.to_owned()),
            "name" => config.name = Some(yaml_string(value)?.to_owned()),
            "steps" => {}
            other => bail!("unsupported flow configuration key `{other}`"),
        }
    }
    Ok(config)
}

fn parse_commands(commands: &[Yaml]) -> Result<Vec<UiCommand>> {
    commands.iter().map(parse_command).collect()
}

fn parse_command(command: &Yaml) -> Result<UiCommand> {
    match command {
        Yaml::String(value) => parse_bare_command(value),
        Yaml::Hash(map) => parse_mapping_command(map),
        other => bail!("unsupported command form `{other:?}`"),
    }
}

fn parse_bare_command(value: &str) -> Result<UiCommand> {
    match value {
        "launchApp" => Ok(UiCommand::LaunchApp(UiLaunchApp {
            stop_app: true,
            ..UiLaunchApp::default()
        })),
        "stopApp" => Ok(UiCommand::StopApp(None)),
        "killApp" => Ok(UiCommand::KillApp(None)),
        "clearState" => Ok(UiCommand::ClearState(None)),
        "clearKeychain" => Ok(UiCommand::ClearKeychain),
        "pasteText" => Ok(UiCommand::PasteText),
        "eraseText" => Ok(UiCommand::EraseText(50)),
        "hideKeyboard" => Ok(UiCommand::HideKeyboard),
        "stopRecording" => Ok(UiCommand::StopRecording),
        "waitForAnimationToEnd" => Ok(UiCommand::WaitForAnimationToEnd(5_000)),
        other => bail!("unsupported bare command `{other}`"),
    }
}

fn parse_mapping_command(map: &YamlHash) -> Result<UiCommand> {
    if map.len() != 1 {
        bail!("command mappings must contain exactly one key");
    }
    let (key, value) = map.iter().next().expect("map length already checked");
    let key = yaml_string(key).context("command names must be strings")?;

    match key {
        "launchApp" => parse_launch_app(value),
        "stopApp" => parse_app_target_command("stopApp", value).map(UiCommand::StopApp),
        "killApp" => parse_app_target_command("killApp", value).map(UiCommand::KillApp),
        "clearState" => parse_app_target_command("clearState", value).map(UiCommand::ClearState),
        "tapOn" => Ok(UiCommand::TapOn(parse_selector(value)?)),
        "hoverOn" => Ok(UiCommand::HoverOn(parse_selector(value)?)),
        "rightClickOn" => Ok(UiCommand::RightClickOn(parse_selector(value)?)),
        "tapOnPoint" => Ok(UiCommand::TapOnPoint(parse_point_expr(value)?)),
        "doubleTapOn" => Ok(UiCommand::DoubleTapOn(parse_selector(value)?)),
        "longPressOn" => parse_long_press(value),
        "swipe" => parse_swipe(value),
        "swipeOn" => parse_swipe_on(value),
        "dragAndDrop" => parse_drag_and_drop(value),
        "scroll" => parse_scroll(value),
        "scrollOn" => parse_scroll_on(value),
        "scrollUntilVisible" => parse_scroll_until_visible(value),
        "inputText" => Ok(UiCommand::InputText(yaml_string(value)?.to_owned())),
        "pasteText" => parse_paste_text(value),
        "setClipboard" => Ok(UiCommand::SetClipboard(yaml_string(value)?.to_owned())),
        "copyTextFrom" => Ok(UiCommand::CopyTextFrom(parse_selector(value)?)),
        "eraseText" => parse_erase_text(value),
        "pressKey" => Ok(UiCommand::PressKey(parse_press_key(value)?)),
        "pressKeyCode" => parse_press_key_code(value),
        "keySequence" => parse_key_sequence(value),
        "pressButton" => parse_press_button(value),
        "selectMenuItem" => parse_select_menu_item(value),
        "hideKeyboard" => parse_hide_keyboard(value),
        "assertVisible" => Ok(UiCommand::AssertVisible(parse_selector(value)?)),
        "assertNotVisible" => Ok(UiCommand::AssertNotVisible(parse_selector(value)?)),
        "extendedWaitUntil" => parse_extended_wait_until(value),
        "waitForAnimationToEnd" => parse_wait_for_animation_to_end(value),
        "takeScreenshot" => Ok(UiCommand::TakeScreenshot(parse_artifact_name(
            "takeScreenshot",
            value,
        )?)),
        "startRecording" => Ok(UiCommand::StartRecording(parse_artifact_name(
            "startRecording",
            value,
        )?)),
        "stopRecording" => parse_stop_recording(value),
        "openLink" => Ok(UiCommand::OpenLink(yaml_string(value)?.to_owned())),
        "setLocation" => parse_set_location(value),
        "setPermissions" => parse_set_permissions(None, value),
        "travel" => parse_travel(value),
        "addMedia" => parse_add_media(value),
        "runFlow" => Ok(UiCommand::RunFlow(PathBuf::from(yaml_string(value)?))),
        "repeat" => parse_counted_block("repeat", value)
            .map(|(times, commands)| UiCommand::Repeat { times, commands }),
        "retry" => parse_counted_block("retry", value)
            .map(|(times, commands)| UiCommand::Retry { times, commands }),
        other => bail!("unsupported UI command `{other}`"),
    }
}

fn parse_swipe(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(direction) => Ok(UiCommand::Swipe(default_swipe_for_direction(direction)?)),
        Yaml::Hash(map) => {
            if let Some(direction) = get_optional(map, "direction") {
                let mut swipe = default_swipe_for_direction(yaml_string(direction)?)?;
                hydrate_swipe_options(map, &mut swipe)?;
                return Ok(UiCommand::Swipe(swipe));
            }
            let start = parse_point_expr(required_field(map, "start")?)?;
            let end = parse_point_expr(required_field(map, "end")?)?;
            let mut swipe = UiSwipe {
                start,
                end,
                duration_ms: None,
                delta: None,
            };
            hydrate_swipe_options(map, &mut swipe)?;
            Ok(UiCommand::Swipe(swipe))
        }
        _ => bail!("`swipe` expects either a direction string or a mapping"),
    }
}

fn parse_launch_app(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(app_id) => Ok(UiCommand::LaunchApp(UiLaunchApp {
            app_id: Some(app_id.to_owned()),
            stop_app: true,
            ..UiLaunchApp::default()
        })),
        Yaml::Hash(map) => {
            let app_id = get_optional(map, "appId")
                .map(yaml_string)
                .transpose()?
                .map(str::to_owned);
            let clear_state = get_optional(map, "clearState")
                .map(yaml_bool)
                .transpose()?
                .unwrap_or(false);
            let clear_keychain = get_optional(map, "clearKeychain")
                .map(yaml_bool)
                .transpose()?
                .unwrap_or(false);
            let stop_app = get_optional(map, "stopApp")
                .map(yaml_bool)
                .transpose()?
                .unwrap_or(true);
            let permissions = get_optional(map, "permissions")
                .map(|permissions| parse_permissions_map(app_id.clone(), permissions))
                .transpose()?;
            let arguments = get_optional(map, "arguments")
                .map(parse_launch_arguments)
                .transpose()?
                .unwrap_or_default();

            Ok(UiCommand::LaunchApp(UiLaunchApp {
                app_id,
                clear_state,
                clear_keychain,
                stop_app,
                permissions,
                arguments,
            }))
        }
        _ => bail!("`launchApp` expects either a string or a mapping"),
    }
}

fn parse_app_target_command(kind: &str, value: &Yaml) -> Result<Option<String>> {
    match value {
        Yaml::String(app_id) => Ok(Some(app_id.to_owned())),
        Yaml::Hash(map) => Ok(get_optional(map, "appId")
            .map(yaml_string)
            .transpose()?
            .map(str::to_owned)),
        _ => bail!("`{kind}` expects either a string or a mapping"),
    }
}

fn parse_long_press(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(_) => Ok(UiCommand::LongPressOn {
            target: parse_selector(value)?,
            duration_ms: 1_500,
        }),
        Yaml::Hash(map) => {
            let target = if let Some(target) = get_optional(map, "element") {
                parse_selector(target)?
            } else {
                bail!("`longPressOn` expects `element`");
            };
            let duration_ms = get_optional(map, "duration")
                .map(parse_duration_ms)
                .transpose()?
                .unwrap_or(1_500);
            Ok(UiCommand::LongPressOn {
                target,
                duration_ms,
            })
        }
        _ => bail!("`longPressOn` expects either a string or a mapping"),
    }
}

fn parse_select_menu_item(value: &Yaml) -> Result<UiCommand> {
    fn validate_items(items: Vec<String>) -> Result<UiCommand> {
        let items = items
            .into_iter()
            .map(|item| item.trim().to_owned())
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>();
        if items.is_empty() {
            bail!("`selectMenuItem` requires at least one menu label");
        }
        Ok(UiCommand::SelectMenuItem(items))
    }

    match value {
        Yaml::String(path) => validate_items(path.split('>').map(str::to_owned).collect()),
        Yaml::Array(items) => validate_items(
            items
                .iter()
                .map(|item| Ok(yaml_string(item)?.to_owned()))
                .collect::<Result<Vec<_>>>()?,
        ),
        Yaml::Hash(map) => match get_optional(map, "path") {
            Some(path) => parse_select_menu_item(path),
            None => bail!("`selectMenuItem` expects a string, sequence, or `{{ path: ... }}`"),
        },
        _ => bail!("`selectMenuItem` expects a string, sequence, or mapping"),
    }
}

fn parse_scroll(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(direction) => Ok(UiCommand::Scroll(parse_swipe_direction(&Yaml::String(
            direction.to_owned(),
        ))?)),
        Yaml::Hash(map) => Ok(UiCommand::Scroll(parse_swipe_direction(required_field(
            map,
            "direction",
        )?)?)),
        _ => bail!("`scroll` expects either a direction string or a mapping"),
    }
}

fn parse_swipe_on(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Hash(map) = value else {
        bail!("`swipeOn` expects a mapping");
    };
    let target = if let Some(target) = get_optional(map, "element") {
        parse_selector(target)?
    } else {
        bail!("`swipeOn` expects `element`");
    };
    let direction = parse_swipe_direction(required_field(map, "direction")?)?;
    let duration_ms = get_optional(map, "duration")
        .map(parse_duration_ms)
        .transpose()?;
    let delta = get_optional(map, "delta").map(yaml_u32).transpose()?;
    Ok(UiCommand::SwipeOn(UiElementSwipe {
        target,
        direction,
        duration_ms,
        delta,
    }))
}

fn parse_drag_and_drop(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Hash(map) = value else {
        bail!("`dragAndDrop` expects a mapping");
    };
    let source = get_optional(map, "from")
        .or_else(|| get_optional(map, "source"))
        .map(parse_selector)
        .transpose()?
        .context("`dragAndDrop` expects `from`")?;
    let destination = get_optional(map, "to")
        .or_else(|| get_optional(map, "destination"))
        .map(parse_selector)
        .transpose()?
        .context("`dragAndDrop` expects `to`")?;
    Ok(UiCommand::DragAndDrop(UiDragAndDrop {
        source,
        destination,
        duration_ms: get_optional(map, "duration")
            .map(parse_duration_ms)
            .transpose()?,
        delta: get_optional(map, "delta").map(yaml_u32).transpose()?,
    }))
}

fn parse_scroll_on(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Hash(map) = value else {
        bail!("`scrollOn` expects a mapping");
    };
    let target = if let Some(target) = get_optional(map, "element") {
        parse_selector(target)?
    } else {
        bail!("`scrollOn` expects `element`");
    };
    let direction = get_optional(map, "direction")
        .map(parse_swipe_direction)
        .transpose()?
        .unwrap_or(UiSwipeDirection::Down);
    Ok(UiCommand::ScrollOn(UiElementScroll { target, direction }))
}

fn hydrate_swipe_options(map: &YamlHash, swipe: &mut UiSwipe) -> Result<()> {
    if let Some(duration) = get_optional(map, "duration") {
        swipe.duration_ms = Some(parse_duration_ms(duration)?);
    }
    if let Some(delta) = get_optional(map, "delta") {
        swipe.delta = Some(yaml_u32(delta)?);
    }
    Ok(())
}

fn parse_scroll_until_visible(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(_) => Ok(UiCommand::ScrollUntilVisible(UiScrollUntilVisible {
            target: parse_selector(value)?,
            direction: UiSwipeDirection::Down,
            timeout_ms: 20_000,
        })),
        Yaml::Hash(map) => {
            let target = if let Some(target) = get_optional(map, "element") {
                parse_selector(target)?
            } else if let Some(target) = get_optional(map, "text") {
                parse_selector(target)?
            } else {
                bail!("`scrollUntilVisible` expects `element` or `text`");
            };
            let direction = get_optional(map, "direction")
                .map(parse_swipe_direction)
                .transpose()?
                .unwrap_or(UiSwipeDirection::Down);
            let timeout_ms = get_optional(map, "timeout")
                .map(parse_duration_ms)
                .transpose()?
                .unwrap_or(20_000);
            Ok(UiCommand::ScrollUntilVisible(UiScrollUntilVisible {
                target,
                direction,
                timeout_ms,
            }))
        }
        _ => bail!("`scrollUntilVisible` expects either a string or a mapping"),
    }
}

fn parse_paste_text(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::Null => Ok(UiCommand::PasteText),
        Yaml::Hash(_) => Ok(UiCommand::PasteText),
        _ => bail!("`pasteText` does not accept an inline value"),
    }
}

fn parse_erase_text(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::Integer(_) => Ok(UiCommand::EraseText(yaml_u32(value)?)),
        Yaml::String(_) => Ok(UiCommand::EraseText(yaml_u32(value)?)),
        Yaml::Hash(map) => {
            let characters = get_optional(map, "characters")
                .map(yaml_u32)
                .transpose()?
                .unwrap_or(50);
            Ok(UiCommand::EraseText(characters))
        }
        _ => bail!("`eraseText` expects an integer or mapping"),
    }
}

fn parse_hide_keyboard(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::Null | Yaml::Hash(_) => Ok(UiCommand::HideKeyboard),
        _ => bail!("`hideKeyboard` does not accept an inline value"),
    }
}

fn parse_stop_recording(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::Null | Yaml::Hash(_) => Ok(UiCommand::StopRecording),
        _ => bail!("`stopRecording` does not accept an inline value"),
    }
}

fn parse_selector(value: &Yaml) -> Result<UiSelector> {
    match value {
        Yaml::String(text) => Ok(UiSelector {
            text: Some(text.to_owned()),
            id: None,
        }),
        Yaml::Hash(map) => {
            let text = get_optional(map, "text")
                .map(yaml_string)
                .transpose()?
                .map(str::to_owned);
            let id = get_optional(map, "id")
                .map(yaml_string)
                .transpose()?
                .map(str::to_owned);
            if text.is_none() && id.is_none() {
                bail!("selector mappings currently support `text` and/or `id`");
            }
            Ok(UiSelector { text, id })
        }
        _ => bail!("selectors must be strings or mappings"),
    }
}

fn parse_press_key(value: &Yaml) -> Result<UiKeyPress> {
    match value {
        Yaml::String(_) => Ok(UiKeyPress::plain(parse_key_token(value)?)),
        Yaml::Hash(map) => Ok(UiKeyPress {
            key: parse_key_token(required_field(map, "key")?)?,
            modifiers: parse_key_modifiers(get_optional(map, "modifiers"))?,
        }),
        _ => bail!("`pressKey` expects a string or mapping"),
    }
}

fn parse_press_key_code(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::Integer(_) | Yaml::String(_) => Ok(UiCommand::PressKeyCode {
            keycode: yaml_u32(value)?,
            duration_ms: None,
            modifiers: Vec::new(),
        }),
        Yaml::Hash(map) => Ok(UiCommand::PressKeyCode {
            keycode: yaml_u32(required_field(map, "keyCode")?)?,
            duration_ms: get_optional(map, "duration")
                .map(parse_duration_ms)
                .transpose()?,
            modifiers: parse_key_modifiers(get_optional(map, "modifiers"))?,
        }),
        _ => bail!("`pressKeyCode` expects an integer or mapping"),
    }
}

fn parse_key_token(value: &Yaml) -> Result<UiPressKey> {
    let token = yaml_string(value)?.trim();
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
        other => bail!("unsupported `pressKey` value `{other}`"),
    }
}

fn parse_key_modifiers(value: Option<&Yaml>) -> Result<Vec<UiKeyModifier>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };

    match value {
        Yaml::Array(values) => values.iter().map(parse_key_modifier).collect(),
        Yaml::String(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(|entry| parse_key_modifier(&Yaml::String(entry.to_owned())))
            .collect(),
        _ => bail!("`modifiers` must be a string or sequence"),
    }
}

fn parse_key_modifier(value: &Yaml) -> Result<UiKeyModifier> {
    match yaml_string(value)?.trim().to_ascii_uppercase().as_str() {
        "COMMAND" | "CMD" => Ok(UiKeyModifier::Command),
        "SHIFT" => Ok(UiKeyModifier::Shift),
        "OPTION" | "ALT" => Ok(UiKeyModifier::Option),
        "CONTROL" | "CTRL" => Ok(UiKeyModifier::Control),
        "FUNCTION" | "FN" => Ok(UiKeyModifier::Function),
        other => bail!("unsupported keyboard modifier `{other}`"),
    }
}

fn parse_key_sequence(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Array(values) = value else {
        bail!("`keySequence` expects a sequence of integer keycodes");
    };
    if values.is_empty() {
        bail!("`keySequence` must not be empty");
    }
    Ok(UiCommand::KeySequence(
        values.iter().map(yaml_u32).collect::<Result<Vec<_>>>()?,
    ))
}

fn parse_press_button(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(_) => Ok(UiCommand::PressButton {
            button: parse_hardware_button(value)?,
            duration_ms: None,
        }),
        Yaml::Hash(map) => Ok(UiCommand::PressButton {
            button: parse_hardware_button(required_field(map, "button")?)?,
            duration_ms: get_optional(map, "duration")
                .map(parse_duration_ms)
                .transpose()?,
        }),
        _ => bail!("`pressButton` expects a string or mapping"),
    }
}

fn parse_hardware_button(value: &Yaml) -> Result<UiHardwareButton> {
    match yaml_string(value)?.to_ascii_uppercase().as_str() {
        "APPLE_PAY" => Ok(UiHardwareButton::ApplePay),
        "HOME" => Ok(UiHardwareButton::Home),
        "LOCK" => Ok(UiHardwareButton::Lock),
        "SIDE_BUTTON" => Ok(UiHardwareButton::SideButton),
        "SIRI" => Ok(UiHardwareButton::Siri),
        other => bail!("unsupported `pressButton` value `{other}`"),
    }
}

fn parse_extended_wait_until(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Hash(map) = value else {
        bail!("`extendedWaitUntil` expects a mapping");
    };
    let visible = get_optional(map, "visible")
        .map(parse_selector)
        .transpose()?;
    let not_visible = get_optional(map, "notVisible")
        .or_else(|| get_optional(map, "not_visible"))
        .map(parse_selector)
        .transpose()?;
    if visible.is_none() && not_visible.is_none() {
        bail!("`extendedWaitUntil` expects `visible` or `notVisible`");
    }
    let timeout_ms = get_optional(map, "timeout")
        .map(parse_duration_ms)
        .transpose()?
        .unwrap_or(10_000);
    Ok(UiCommand::ExtendedWaitUntil(UiExtendedWaitUntil {
        visible,
        not_visible,
        timeout_ms,
    }))
}

fn parse_wait_for_animation_to_end(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::Null => Ok(UiCommand::WaitForAnimationToEnd(5_000)),
        Yaml::String(_) | Yaml::Integer(_) => {
            Ok(UiCommand::WaitForAnimationToEnd(parse_duration_ms(value)?))
        }
        Yaml::Hash(map) => {
            let timeout_ms = get_optional(map, "timeout")
                .map(parse_duration_ms)
                .transpose()?
                .unwrap_or(5_000);
            Ok(UiCommand::WaitForAnimationToEnd(timeout_ms))
        }
        _ => bail!("`waitForAnimationToEnd` expects a duration or mapping"),
    }
}

fn parse_artifact_name(kind: &str, value: &Yaml) -> Result<Option<String>> {
    match value {
        Yaml::String(value) => Ok(Some(value.to_owned())),
        Yaml::Null => Ok(None),
        Yaml::Hash(map) => Ok(get_optional(map, "path")
            .or_else(|| get_optional(map, "name"))
            .map(yaml_string)
            .transpose()?
            .map(str::to_owned)),
        _ => bail!("`{kind}` expects a string, null, or mapping"),
    }
}

fn parse_set_permissions(app_id: Option<String>, value: &Yaml) -> Result<UiCommand> {
    Ok(UiCommand::SetPermissions(parse_permissions_map(
        app_id, value,
    )?))
}

fn parse_permissions_map(app_id: Option<String>, value: &Yaml) -> Result<UiPermissionConfig> {
    let Yaml::Hash(map) = value else {
        bail!("permissions must be a mapping");
    };
    let app_id = get_optional(map, "appId")
        .map(yaml_string)
        .transpose()?
        .map(str::to_owned)
        .or(app_id);
    let permissions_value = get_optional(map, "permissions").unwrap_or(value);
    let Yaml::Hash(permission_map) = permissions_value else {
        bail!("`permissions` must be a mapping");
    };
    let mut permissions = Vec::new();
    for (key, value) in permission_map {
        let name = yaml_string(key)?.to_owned();
        let state = match yaml_string(value)?.to_ascii_lowercase().as_str() {
            "allow" => UiPermissionState::Allow,
            "deny" => UiPermissionState::Deny,
            "unset" => UiPermissionState::Unset,
            other => bail!("unsupported permission state `{other}`"),
        };
        permissions.push(UiPermissionSetting { name, state });
    }
    if permissions.is_empty() {
        bail!("permissions mapping must not be empty");
    }
    Ok(UiPermissionConfig {
        app_id,
        permissions,
    })
}

fn parse_launch_arguments(value: &Yaml) -> Result<Vec<(String, String)>> {
    let Yaml::Hash(map) = value else {
        bail!("`launchApp.arguments` expects a mapping");
    };
    let mut arguments = Vec::new();
    for (key, value) in map {
        let key = yaml_string(key)?.to_owned();
        arguments.push((key, yaml_scalar_to_string(value)?));
    }
    Ok(arguments)
}

fn yaml_scalar_to_string(value: &Yaml) -> Result<String> {
    match value {
        Yaml::String(value) => Ok(value.to_owned()),
        Yaml::Integer(value) => Ok(value.to_string()),
        Yaml::Real(value) => Ok(value.to_owned()),
        Yaml::Boolean(value) => Ok(value.to_string()),
        Yaml::Null => Ok("null".to_owned()),
        _ => bail!("expected a scalar launch argument value"),
    }
}

fn parse_travel(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Hash(map) = value else {
        bail!("`travel` expects a mapping");
    };
    let points_value = required_field(map, "points")?;
    let Yaml::Array(points_value) = points_value else {
        bail!("`travel.points` must be a sequence");
    };
    if points_value.len() < 2 {
        bail!("`travel.points` requires at least two coordinates");
    }
    let points = points_value
        .iter()
        .map(parse_location_point)
        .collect::<Result<Vec<_>>>()?;
    let speed_meters_per_second = get_optional(map, "speed").map(yaml_f64).transpose()?;
    Ok(UiCommand::Travel(UiTravel {
        points,
        speed_meters_per_second,
    }))
}

fn parse_add_media(value: &Yaml) -> Result<UiCommand> {
    match value {
        Yaml::String(path) => Ok(UiCommand::AddMedia(vec![PathBuf::from(path)])),
        Yaml::Array(paths) => {
            let media_paths = paths
                .iter()
                .map(|path| Ok(PathBuf::from(yaml_string(path)?)))
                .collect::<Result<Vec<_>>>()?;
            if media_paths.is_empty() {
                bail!("`addMedia` must include at least one file");
            }
            Ok(UiCommand::AddMedia(media_paths))
        }
        _ => bail!("`addMedia` expects a path string or a sequence of paths"),
    }
}

fn parse_location_point(value: &Yaml) -> Result<UiLocationPoint> {
    match value {
        Yaml::String(point) => {
            let (latitude, longitude) = point
                .split_once(',')
                .context("travel points must be `lat,lon` strings")?;
            Ok(UiLocationPoint {
                latitude: latitude
                    .trim()
                    .parse()
                    .with_context(|| format!("invalid latitude in `{point}`"))?,
                longitude: longitude
                    .trim()
                    .parse()
                    .with_context(|| format!("invalid longitude in `{point}`"))?,
            })
        }
        Yaml::Hash(map) => Ok(UiLocationPoint {
            latitude: yaml_f64(required_field(map, "latitude")?)?,
            longitude: yaml_f64(required_field(map, "longitude")?)?,
        }),
        _ => bail!("travel points must be strings or mappings"),
    }
}

fn parse_set_location(value: &Yaml) -> Result<UiCommand> {
    let Yaml::Hash(map) = value else {
        bail!("`setLocation` expects a mapping with `latitude` and `longitude`");
    };
    Ok(UiCommand::SetLocation {
        latitude: yaml_f64(required_field(map, "latitude")?)?,
        longitude: yaml_f64(required_field(map, "longitude")?)?,
    })
}

fn parse_point_expr(value: &Yaml) -> Result<UiPointExpr> {
    let text = yaml_string(value)?;
    let mut segments = text.split(',').map(str::trim);
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

fn parse_coordinate(value: &str) -> Result<UiCoordinate> {
    let trimmed = value.trim();
    if let Some(percent) = trimmed.strip_suffix('%') {
        let percent = percent
            .trim()
            .parse::<f64>()
            .with_context(|| format!("invalid percentage coordinate `{trimmed}`"))?;
        return Ok(UiCoordinate::Percent(percent));
    }
    let absolute = trimmed
        .parse::<f64>()
        .with_context(|| format!("invalid absolute coordinate `{trimmed}`"))?;
    Ok(UiCoordinate::Absolute(absolute))
}

fn default_swipe_for_direction(direction: &str) -> Result<UiSwipe> {
    let (start, end) = match direction.to_ascii_uppercase().as_str() {
        "LEFT" => ("90%, 50%", "10%, 50%"),
        "RIGHT" => ("10%, 50%", "90%, 50%"),
        "UP" => ("50%, 50%", "50%, 10%"),
        "DOWN" => ("50%, 20%", "50%, 90%"),
        other => bail!("unsupported swipe direction `{other}`"),
    };
    Ok(UiSwipe {
        start: parse_point_expr(&Yaml::String(start.to_owned()))?,
        end: parse_point_expr(&Yaml::String(end.to_owned()))?,
        duration_ms: None,
        delta: None,
    })
}

fn parse_swipe_direction(value: &Yaml) -> Result<UiSwipeDirection> {
    match yaml_string(value)?.to_ascii_uppercase().as_str() {
        "LEFT" => Ok(UiSwipeDirection::Left),
        "RIGHT" => Ok(UiSwipeDirection::Right),
        "UP" => Ok(UiSwipeDirection::Up),
        "DOWN" => Ok(UiSwipeDirection::Down),
        other => bail!("unsupported swipe direction `{other}`"),
    }
}

fn parse_duration_ms(value: &Yaml) -> Result<u32> {
    if let Some(integer) = value.as_i64() {
        return u32::try_from(integer)
            .with_context(|| "expected a non-negative integer".to_owned());
    }
    let text = yaml_string(value)?;
    if let Some(ms) = text.strip_suffix("ms") {
        return ms
            .trim()
            .parse::<u32>()
            .with_context(|| format!("invalid duration `{text}`"));
    }
    if let Some(seconds) = text.strip_suffix('s') {
        let seconds = seconds
            .trim()
            .parse::<f64>()
            .with_context(|| format!("invalid duration `{text}`"))?;
        if seconds.is_sign_negative() {
            bail!("duration must not be negative");
        }
        return Ok((seconds * 1000.0).round() as u32);
    }
    text.parse::<u32>()
        .with_context(|| format!("invalid duration `{text}`"))
}

fn parse_counted_block(kind: &str, value: &Yaml) -> Result<(u32, Vec<UiCommand>)> {
    let Yaml::Hash(map) = value else {
        bail!("`{kind}` expects a mapping");
    };
    let times = yaml_u32(required_field(map, "times")?)?;
    let commands_value = required_field(map, "commands")?;
    let Yaml::Array(commands) = commands_value else {
        bail!("`{kind}.commands` must be a sequence");
    };
    Ok((times, parse_commands(commands)?))
}

fn required_field<'a>(map: &'a YamlHash, key: &str) -> Result<&'a Yaml> {
    get_optional(map, key).with_context(|| format!("missing required field `{key}`"))
}

fn get_optional<'a>(map: &'a YamlHash, key: &str) -> Option<&'a Yaml> {
    map.get(&Yaml::String(key.to_owned()))
}

fn yaml_string(value: &Yaml) -> Result<&str> {
    value
        .as_str()
        .with_context(|| "expected a string".to_owned())
}

fn yaml_u32(value: &Yaml) -> Result<u32> {
    let integer = match value {
        Yaml::String(value) => value
            .parse::<i64>()
            .with_context(|| format!("expected an integer, got `{value}`"))?,
        _ => value
            .as_i64()
            .with_context(|| "expected an integer".to_owned())?,
    };
    u32::try_from(integer).with_context(|| "expected a non-negative integer".to_owned())
}

fn yaml_f64(value: &Yaml) -> Result<f64> {
    if let Some(value) = value.as_f64() {
        return Ok(value);
    }
    if let Some(value) = value.as_i64() {
        return Ok(value as f64);
    }
    bail!("expected a number")
}

fn yaml_bool(value: &Yaml) -> Result<bool> {
    if let Some(value) = value.as_bool() {
        return Ok(value);
    }
    if let Some(value) = value.as_str() {
        return match value.to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => bail!("expected a boolean"),
        };
    }
    bail!("expected a boolean")
}

fn json_to_yaml(value: &JsonValue) -> Result<Yaml> {
    Ok(match value {
        JsonValue::Null => Yaml::Null,
        JsonValue::Bool(value) => Yaml::Boolean(*value),
        JsonValue::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Yaml::Integer(integer)
            } else if let Some(integer) = value.as_u64() {
                Yaml::Integer(
                    i64::try_from(integer).context("JSON integer exceeded the supported range")?,
                )
            } else {
                Yaml::Real(value.to_string())
            }
        }
        JsonValue::String(value) => Yaml::String(value.clone()),
        JsonValue::Array(values) => Yaml::Array(
            values
                .iter()
                .map(json_to_yaml)
                .collect::<Result<Vec<_>>>()?,
        ),
        JsonValue::Object(map) => {
            let mut yaml_map = YamlHash::new();
            for (key, value) in map {
                yaml_map.insert(Yaml::String(key.clone()), json_to_yaml(value)?);
            }
            Yaml::Hash(yaml_map)
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        UiCommand, UiDragAndDrop, UiElementScroll, UiElementSwipe, UiHardwareButton, UiKeyModifier,
        UiKeyPress, UiLaunchApp, UiPressKey, UiScrollUntilVisible, UiSelector, UiSwipe,
        UiSwipeDirection, parse_ui_flow,
    };

    #[test]
    fn parses_json_ui_flow() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"appId\": \"dev.orbi.fixture\",\n  \"name\": \"Login\",\n  \"steps\": [\n    \"launchApp\",\n    {\n      \"tapOn\": \"Continue\"\n    },\n    {\n      \"retry\": {\n        \"times\": 2,\n        \"commands\": [\n          {\n            \"assertVisible\": \"Welcome\"\n          }\n        ]\n      }\n    }\n  ]\n}\n",
        )
        .unwrap();

        let flow = parse_ui_flow(&path).unwrap();
        assert_eq!(flow.config.app_id.as_deref(), Some("dev.orbi.fixture"));
        assert_eq!(flow.config.name.as_deref(), Some("Login"));
        assert!(matches!(
            flow.commands[0],
            UiCommand::LaunchApp(UiLaunchApp { stop_app: true, .. })
        ));
        assert!(matches!(
            flow.commands[1],
            UiCommand::TapOn(UiSelector {
                text: Some(ref value),
                id: None,
            }) if value == "Continue"
        ));
        assert!(matches!(
            flow.commands[2],
            UiCommand::Retry { times: 2, .. }
        ));
    }

    #[test]
    fn rejects_unknown_config_keys() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"env\": {\n    \"A\": \"B\"\n  },\n  \"steps\": [\"launchApp\"]\n}\n",
        )
        .unwrap();

        let error = parse_ui_flow(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unsupported flow configuration key")
        );
    }

    #[test]
    fn rejects_missing_schema() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(&path, "{\n  \"steps\": [\"launchApp\"]\n}\n").unwrap();

        let error = parse_ui_flow(&path).unwrap_err();
        assert!(error.to_string().contains("must declare `$schema`"));
    }

    #[test]
    fn parses_swipe_direction_and_coordinate_forms() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"swipe\": \"LEFT\"\n    },\n    {\n      \"swipe\": {\n        \"start\": \"90%, 50%\",\n        \"end\": \"10%, 50%\",\n        \"duration\": \"800ms\",\n        \"delta\": 5\n      }\n    }\n  ]\n}\n",
        )
        .unwrap();

        let flow = parse_ui_flow(&path).unwrap();
        assert!(matches!(flow.commands[0], UiCommand::Swipe(_)));
        assert!(matches!(
            flow.commands[1],
            UiCommand::Swipe(UiSwipe {
                duration_ms: Some(800),
                delta: Some(5),
                ..
            })
        ));
    }

    #[test]
    fn parses_scroll_until_visible_mapping() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"scrollUntilVisible\": {\n        \"element\": {\n          \"text\": \"Ready\"\n        },\n        \"direction\": \"DOWN\",\n        \"timeout\": \"3s\"\n      }\n    }\n  ]\n}\n",
        )
        .unwrap();

        let flow = parse_ui_flow(&path).unwrap();
        assert!(matches!(
            &flow.commands[0],
            UiCommand::ScrollUntilVisible(UiScrollUntilVisible {
                target,
                direction: UiSwipeDirection::Down,
                timeout_ms: 3000,
            }) if target.text.as_deref() == Some("Ready")
        ));
    }

    #[test]
    fn parses_long_press_and_scroll_commands() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"hoverOn\": {\n        \"id\": \"hover-target\"\n      }\n    },\n    {\n      \"rightClickOn\": {\n        \"id\": \"context-target\"\n      }\n    },\n    {\n      \"doubleTapOn\": \"Continue\"\n    },\n    {\n      \"longPressOn\": {\n        \"element\": \"Continue\",\n        \"duration\": \"1200ms\"\n      }\n    },\n    {\n      \"swipeOn\": {\n        \"element\": {\n          \"id\": \"pager\"\n        },\n        \"direction\": \"LEFT\",\n        \"duration\": \"650ms\",\n        \"delta\": 4\n      }\n    },\n    {\n      \"dragAndDrop\": {\n        \"from\": {\n          \"id\": \"drag-source\"\n        },\n        \"to\": {\n          \"id\": \"drop-target\"\n        },\n        \"duration\": \"800ms\",\n        \"delta\": 3\n      }\n    },\n    {\n      \"scroll\": \"DOWN\"\n    },\n    {\n      \"scrollOn\": {\n        \"element\": {\n          \"id\": \"feed\"\n        },\n        \"direction\": \"UP\"\n      }\n    },\n    {\n      \"selectMenuItem\": \"Automation > Trigger Shortcut\"\n    },\n    \"killApp\"\n  ]\n}\n",
        )
        .unwrap();

        let flow = parse_ui_flow(&path).unwrap();
        assert!(matches!(
            &flow.commands[0],
            UiCommand::HoverOn(UiSelector {
                text: None,
                id: Some(target),
            }) if target == "hover-target"
        ));
        assert!(matches!(
            &flow.commands[1],
            UiCommand::RightClickOn(UiSelector {
                text: None,
                id: Some(target),
            }) if target == "context-target"
        ));
        assert!(matches!(
            &flow.commands[2],
            UiCommand::DoubleTapOn(UiSelector {
                text: Some(target),
                id: None,
            }) if target == "Continue"
        ));
        assert!(matches!(
            &flow.commands[3],
            UiCommand::LongPressOn {
                target,
                duration_ms: 1200,
            } if target.text.as_deref() == Some("Continue")
        ));
        assert!(matches!(
            &flow.commands[4],
            UiCommand::SwipeOn(UiElementSwipe {
                target,
                direction: UiSwipeDirection::Left,
                duration_ms: Some(650),
                delta: Some(4),
            }) if target.id.as_deref() == Some("pager")
        ));
        assert!(matches!(
            &flow.commands[5],
            UiCommand::DragAndDrop(UiDragAndDrop {
                source,
                destination,
                duration_ms: Some(800),
                delta: Some(3),
            }) if source.id.as_deref() == Some("drag-source")
                && destination.id.as_deref() == Some("drop-target")
        ));
        assert!(matches!(
            &flow.commands[6],
            UiCommand::Scroll(UiSwipeDirection::Down)
        ));
        assert!(matches!(
            &flow.commands[7],
            UiCommand::ScrollOn(UiElementScroll {
                target,
                direction: UiSwipeDirection::Up,
            }) if target.id.as_deref() == Some("feed")
        ));
        assert!(matches!(
            &flow.commands[8],
            UiCommand::SelectMenuItem(path)
            if path == &vec!["Automation".to_owned(), "Trigger Shortcut".to_owned()]
        ));
        assert!(matches!(&flow.commands[9], UiCommand::KillApp(None)));
    }

    #[test]
    fn rejects_scroll_on_without_element() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"scrollOn\": {\n        \"direction\": \"DOWN\"\n      }\n    }\n  ]\n}\n",
        )
        .unwrap();

        let error = parse_ui_flow(&path).unwrap_err();
        assert!(error.to_string().contains("`scrollOn` expects `element`"));
    }

    #[test]
    fn rejects_swipe_on_without_mapping() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"swipeOn\": \"LEFT\"\n    }\n  ]\n}\n",
        )
        .unwrap();

        let error = parse_ui_flow(&path).unwrap_err();
        assert!(error.to_string().contains("`swipeOn` expects a mapping"));
    }

    #[test]
    fn rejects_drag_and_drop_without_to() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"dragAndDrop\": {\n        \"from\": {\n          \"id\": \"drag-source\"\n        }\n      }\n    }\n  ]\n}\n",
        )
        .unwrap();

        let error = parse_ui_flow(&path).unwrap_err();
        assert!(error.to_string().contains("`dragAndDrop` expects `to`"));
    }

    #[test]
    fn parses_launch_permissions_and_wait_commands() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("flow.json");
        fs::write(
            &path,
            "{\n  \"$schema\": \"/tmp/.orbi/schemas/orbi-ui-test.v1.json\",\n  \"steps\": [\n    {\n      \"launchApp\": {\n        \"appId\": \"dev.orbi.fixture\",\n        \"stopApp\": false,\n        \"clearState\": true,\n        \"clearKeychain\": true,\n        \"arguments\": {\n          \"onboardingComplete\": true,\n          \"seedUser\": \"qa@example.com\"\n        },\n        \"permissions\": {\n          \"location\": \"allow\",\n          \"photos\": \"deny\"\n        }\n      }\n    },\n    {\n      \"tapOnPoint\": \"140, 142\"\n    },\n    {\n      \"pressButton\": {\n        \"button\": \"SIRI\",\n        \"duration\": \"500ms\"\n      }\n    },\n    {\n      \"setClipboard\": \"copied value\"\n    },\n    {\n      \"copyTextFrom\": {\n        \"id\": \"email-value\"\n      }\n    },\n    {\n      \"pasteText\": {}\n    },\n    {\n      \"eraseText\": 6\n    },\n    {\n      \"pressKey\": {\n        \"key\": \"K\",\n        \"modifiers\": [\"COMMAND\", \"SHIFT\"]\n      }\n    },\n    {\n      \"pressKeyCode\": {\n        \"keyCode\": 41,\n        \"duration\": \"200ms\",\n        \"modifiers\": \"CONTROL\"\n      }\n    },\n    {\n      \"keySequence\": [4, 5, 6]\n    },\n    \"hideKeyboard\",\n    {\n      \"extendedWaitUntil\": {\n        \"visible\": {\n          \"text\": \"Ready\"\n        },\n        \"timeout\": \"2s\"\n      }\n    },\n    {\n      \"waitForAnimationToEnd\": {\n        \"timeout\": \"750ms\"\n      }\n    },\n    {\n      \"addMedia\": [\"../Fixtures/cat.jpg\"]\n    },\n    {\n      \"startRecording\": \"login-clip\"\n    },\n    \"stopRecording\",\n    {\n      \"travel\": {\n        \"points\": [\"55.7558,37.6173\", \"55.7568,37.6183\"],\n        \"speed\": 42\n      }\n    }\n  ]\n}\n",
        )
        .unwrap();

        let flow = parse_ui_flow(&path).unwrap();
        assert!(matches!(
            &flow.commands[0],
            UiCommand::LaunchApp(UiLaunchApp {
                app_id: Some(app_id),
                clear_state: true,
                clear_keychain: true,
                stop_app: false,
                permissions: Some(_),
                arguments,
            }) if app_id == "dev.orbi.fixture" && arguments.len() == 2
        ));
        assert!(matches!(&flow.commands[1], UiCommand::TapOnPoint(_)));
        assert!(matches!(
            &flow.commands[2],
            UiCommand::PressButton {
                button: UiHardwareButton::Siri,
                duration_ms: Some(500),
            }
        ));
        assert!(
            matches!(&flow.commands[3], UiCommand::SetClipboard(value) if value == "copied value")
        );
        assert!(matches!(
            &flow.commands[4],
            UiCommand::CopyTextFrom(UiSelector {
                text: None,
                id: Some(id),
            }) if id == "email-value"
        ));
        assert!(matches!(&flow.commands[5], UiCommand::PasteText));
        assert!(matches!(&flow.commands[6], UiCommand::EraseText(6)));
        assert!(matches!(
            &flow.commands[7],
            UiCommand::PressKey(UiKeyPress {
                key: UiPressKey::Character('K'),
                modifiers,
            }) if modifiers == &vec![UiKeyModifier::Command, UiKeyModifier::Shift]
        ));
        assert!(matches!(
            &flow.commands[8],
            UiCommand::PressKeyCode {
                keycode: 41,
                duration_ms: Some(200),
                modifiers,
            } if modifiers == &vec![UiKeyModifier::Control]
        ));
        assert!(matches!(
            &flow.commands[9],
            UiCommand::KeySequence(sequence) if sequence == &vec![4, 5, 6]
        ));
        assert!(matches!(&flow.commands[10], UiCommand::HideKeyboard));
        assert!(matches!(
            &flow.commands[11],
            UiCommand::ExtendedWaitUntil(_)
        ));
        assert!(matches!(
            &flow.commands[12],
            UiCommand::WaitForAnimationToEnd(750)
        ));
        assert!(matches!(&flow.commands[13], UiCommand::AddMedia(paths) if paths.len() == 1));
        assert!(
            matches!(&flow.commands[14], UiCommand::StartRecording(Some(path)) if path == "login-clip")
        );
        assert!(matches!(&flow.commands[15], UiCommand::StopRecording));
        assert!(matches!(&flow.commands[16], UiCommand::Travel(_)));
    }
}
