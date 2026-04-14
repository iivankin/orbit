use std::fmt::Write as _;

use serde_json::{Map as JsonMap, Value as JsonValue, json};

use crate::manifest::ApplePlatform;

const IOS_FLOW_COMMANDS: &[&str] = &[
    "launchApp",
    "stopApp",
    "killApp",
    "clearState",
    "clearKeychain",
    "tapOn",
    "tapOnPoint",
    "doubleTapOn",
    "longPressOn",
    "swipe",
    "swipeOn",
    "scroll",
    "scrollOn",
    "scrollUntilVisible",
    "inputText",
    "pasteText",
    "setClipboard",
    "copyTextFrom",
    "eraseText",
    "pressKey",
    "pressKeyCode",
    "keySequence",
    "pressButton",
    "hideKeyboard",
    "assertVisible",
    "assertNotVisible",
    "extendedWaitUntil",
    "waitForAnimationToEnd",
    "takeScreenshot",
    "startRecording",
    "stopRecording",
    "openLink",
    "setLocation",
    "setPermissions",
    "travel",
    "addMedia",
    "runFlow",
    "repeat",
    "retry",
];

const IOS_HELPER_COMMANDS: &[&str] = &[
    "orbit ui dump-tree --platform ios",
    "orbit ui describe-point --platform ios --x <x> --y <y>",
    "orbit ui focus --platform ios",
    "orbit ui logs --platform ios -- ...",
    "orbit ui open --platform ios <url>",
    "orbit ui add-media --platform ios <path>",
    "orbit ui install-dylib --platform ios <path>",
    "orbit ui instruments --platform ios --template ...",
    "orbit ui update-contacts --platform ios <sqlite>",
    "orbit ui crash --platform ios ...",
];

const MACOS_FLOW_COMMANDS: &[&str] = &[
    "launchApp",
    "stopApp",
    "clearState",
    "tapOn",
    "hoverOn",
    "rightClickOn",
    "dragAndDrop",
    "swipe",
    "scroll",
    "scrollUntilVisible",
    "inputText",
    "pressKey",
    "assertVisible",
    "takeScreenshot",
    "startRecording",
    "stopRecording",
    "openLink",
    "runFlow",
    "repeat",
    "retry",
];

const MACOS_HELPER_COMMANDS: &[&str] = &[
    "orbit ui doctor --platform macos",
    "orbit ui dump-tree --platform macos",
    "orbit ui describe-point --platform macos --x <x> --y <y>",
];

const MACOS_UNSUPPORTED_COMMANDS: &[&str] = &[
    "pressButton",
    "clearKeychain",
    "setLocation",
    "setPermissions",
    "travel",
    "addMedia",
];

pub(crate) fn schema_json(platform: Option<ApplePlatform>) -> JsonValue {
    json!({
        "dialect": "orbit.ui.v1",
        "description": "Orbit-native UI flow dialect used by `tests.ui`.",
        "top_level": {
            "document_shapes": [
                "single YAML command list",
                "config document followed by `---` and a command list",
                "single config document with `steps`"
            ],
            "config_keys": ["appId", "name", "steps"],
            "unknown_config_keys": "rejected"
        },
        "selectors": {
            "forms": [
                "plain string text match",
                "mapping with `text`, `id`, or both"
            ],
            "examples": [
                "Continue",
                { "id": "login-button" },
                { "text": "Ready", "id": "status-label" }
            ]
        },
        "scalars": {
            "duration": {
                "forms": [
                    "integer milliseconds",
                    "string ending in `ms`",
                    "string ending in `s`"
                ],
                "examples": [750, "750ms", "2s"]
            },
            "point": {
                "syntax": "x, y",
                "coordinate_forms": ["absolute number", "percent like `90%`"],
                "examples": ["140, 142", "90%, 50%"]
            }
        },
        "commands": command_specs(),
        "platform_support": platform_support(platform),
        "notes": [
            "Parser support is broader than any single runtime backend.",
            "Use `orbit ui dump-tree` and `orbit ui describe-point` before rewriting selectors.",
            "Use `id` selectors when the app can expose stable accessibility identifiers."
        ]
    })
}

pub(crate) fn schema_text(platform: Option<ApplePlatform>) -> String {
    let schema = schema_json(platform);
    let mut output = String::new();

    let dialect = schema["dialect"].as_str().unwrap_or("orbit.ui.v1");
    let description = schema["description"].as_str().unwrap_or_default();
    writeln!(&mut output, "Dialect: {dialect}").expect("write to string");
    if !description.is_empty() {
        writeln!(&mut output, "{description}").expect("write to string");
    }
    output.push('\n');
    output.push_str("Use `--json` to print the raw machine-readable schema.\n");

    if let Some(notes) = schema["notes"].as_array()
        && !notes.is_empty()
    {
        push_heading(&mut output, "Notes");
        for note in notes {
            if let Some(note) = note.as_str() {
                writeln!(&mut output, "  - {note}").expect("write to string");
            }
        }
    }

    push_heading(&mut output, "Top level");
    push_string_list(
        &mut output,
        "Document shapes",
        schema["top_level"]["document_shapes"]
            .as_array()
            .map(Vec::as_slice),
    );
    push_kv_line(
        &mut output,
        "Config keys",
        join_string_array(
            schema["top_level"]["config_keys"]
                .as_array()
                .map(Vec::as_slice),
        ),
    );
    push_kv_line(
        &mut output,
        "Unknown config keys",
        schema["top_level"]["unknown_config_keys"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    );

    push_heading(&mut output, "Selectors");
    push_string_list(
        &mut output,
        "Forms",
        schema["selectors"]["forms"].as_array().map(Vec::as_slice),
    );
    push_value_examples(
        &mut output,
        "Examples",
        schema["selectors"]["examples"]
            .as_array()
            .map(Vec::as_slice),
    );

    push_heading(&mut output, "Scalars");
    push_scalar_block(&mut output, "duration", &schema["scalars"]["duration"]);
    push_scalar_block(&mut output, "point", &schema["scalars"]["point"]);

    push_heading(&mut output, "Platform support");
    if let Some(support) = schema["platform_support"].as_object() {
        for (platform_name, entry) in support {
            push_subheading(&mut output, platform_name);
            push_kv_line(
                &mut output,
                "Backend status",
                entry["backend_status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            );
            push_string_list(
                &mut output,
                "Flow commands",
                entry["flow_commands"].as_array().map(Vec::as_slice),
            );
            push_string_list(
                &mut output,
                "Helper commands",
                entry["helper_commands"].as_array().map(Vec::as_slice),
            );
            if let Some(unsupported) = entry["unsupported_commands"].as_array() {
                push_string_list(
                    &mut output,
                    "Unsupported commands",
                    Some(unsupported.as_slice()),
                );
            }
            if let Some(notes) = entry["notes"].as_array() {
                push_string_list(&mut output, "Notes", Some(notes.as_slice()));
            }
        }
    }

    push_heading(&mut output, "Commands");
    if let Some(commands) = schema["commands"].as_object() {
        for (name, spec) in commands {
            push_subheading(&mut output, name);
            if let Some(input) = spec["input"].as_str() {
                push_kv_line(&mut output, "Input", input.to_owned());
            }
            if let Some(forms) = spec["forms"].as_array() {
                push_string_list(&mut output, "Forms", Some(forms.as_slice()));
            }
            if let Some(mapping_keys) = spec["mapping_keys"].as_array() {
                push_kv_line(
                    &mut output,
                    "Mapping keys",
                    join_string_array(Some(mapping_keys.as_slice())),
                );
            }
            if let Some(defaults) = spec["defaults"].as_object() {
                push_kv_line(&mut output, "Defaults", format_json_object(defaults));
            }
            if let Some(modifiers) = spec["supported_modifiers"].as_array() {
                push_kv_line(
                    &mut output,
                    "Supported modifiers",
                    join_string_array(Some(modifiers.as_slice())),
                );
            }
            if let Some(states) = spec["supported_states"].as_array() {
                push_kv_line(
                    &mut output,
                    "Supported states",
                    join_string_array(Some(states.as_slice())),
                );
            }
        }
    }

    output.trim_end().to_owned()
}

fn push_heading(output: &mut String, heading: &str) {
    output.push('\n');
    writeln!(output, "{heading}:").expect("write to string");
}

fn push_subheading(output: &mut String, heading: &str) {
    writeln!(output, "  {heading}:").expect("write to string");
}

fn push_kv_line(output: &mut String, key: &str, value: String) {
    if value.is_empty() {
        return;
    }
    writeln!(output, "    {key}: {value}").expect("write to string");
}

fn push_string_list(output: &mut String, label: &str, values: Option<&[JsonValue]>) {
    let Some(values) = values else {
        return;
    };
    if values.is_empty() {
        return;
    }

    writeln!(output, "    {label}:").expect("write to string");
    for value in values {
        if let Some(value) = value.as_str() {
            writeln!(output, "      - {value}").expect("write to string");
        }
    }
}

fn push_value_examples(output: &mut String, label: &str, values: Option<&[JsonValue]>) {
    let Some(values) = values else {
        return;
    };
    if values.is_empty() {
        return;
    }

    writeln!(output, "    {label}:").expect("write to string");
    for value in values {
        writeln!(output, "      - {}", format_inline_value(value)).expect("write to string");
    }
}

fn push_scalar_block(output: &mut String, name: &str, scalar: &JsonValue) {
    writeln!(output, "  {name}:").expect("write to string");
    if let Some(syntax) = scalar["syntax"].as_str() {
        push_kv_line(output, "Syntax", syntax.to_owned());
    }
    if let Some(forms) = scalar["forms"].as_array() {
        push_string_list(output, "Forms", Some(forms.as_slice()));
    }
    if let Some(coordinate_forms) = scalar["coordinate_forms"].as_array() {
        push_string_list(
            output,
            "Coordinate forms",
            Some(coordinate_forms.as_slice()),
        );
    }
    if let Some(examples) = scalar["examples"].as_array() {
        push_value_examples(output, "Examples", Some(examples.as_slice()));
    }
}

fn join_string_array(values: Option<&[JsonValue]>) -> String {
    values
        .into_iter()
        .flat_map(|values| values.iter())
        .filter_map(JsonValue::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_inline_value(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.to_owned(),
        _ => serde_json::to_string(value).expect("serialize schema value"),
    }
}

fn format_json_object(values: &JsonMap<String, JsonValue>) -> String {
    values
        .iter()
        .map(|(key, value)| format!("{key}={}", format_inline_value(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn command_specs() -> JsonValue {
    let mut commands = JsonMap::new();
    commands.insert(
        "launchApp".to_owned(),
        json!({
            "forms": ["bare", "string app id", "mapping"],
            "mapping_keys": ["appId", "clearState", "clearKeychain", "stopApp", "arguments", "permissions"],
            "defaults": {
                "stopApp": true,
                "clearState": false,
                "clearKeychain": false
            }
        }),
    );
    commands.insert(
        "stopApp".to_owned(),
        json!({ "forms": ["bare", "string app id", "mapping with `appId`"] }),
    );
    commands.insert(
        "killApp".to_owned(),
        json!({ "forms": ["bare", "string app id", "mapping with `appId`"] }),
    );
    commands.insert(
        "clearState".to_owned(),
        json!({ "forms": ["bare", "string app id", "mapping with `appId`"] }),
    );
    commands.insert(
        "clearKeychain".to_owned(),
        json!({ "forms": ["bare only"] }),
    );
    commands.insert("tapOn".to_owned(), json!({ "input": "selector" }));
    commands.insert("hoverOn".to_owned(), json!({ "input": "selector" }));
    commands.insert("rightClickOn".to_owned(), json!({ "input": "selector" }));
    commands.insert("tapOnPoint".to_owned(), json!({ "input": "point" }));
    commands.insert("doubleTapOn".to_owned(), json!({ "input": "selector" }));
    commands.insert(
        "longPressOn".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["element", "duration"],
            "defaults": { "duration": "1500ms" }
        }),
    );
    commands.insert(
        "swipe".to_owned(),
        json!({
            "forms": ["direction string", "mapping"],
            "mapping_keys": ["direction", "start", "end", "duration", "delta"]
        }),
    );
    commands.insert(
        "swipeOn".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["element", "direction", "duration", "delta"]
        }),
    );
    commands.insert(
        "dragAndDrop".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["from", "to", "source", "destination", "duration", "delta"]
        }),
    );
    commands.insert("scroll".to_owned(), json!({ "input": "direction string" }));
    commands.insert(
        "scrollOn".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["element", "direction"],
            "defaults": { "direction": "DOWN" }
        }),
    );
    commands.insert(
        "scrollUntilVisible".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["element", "direction", "timeout"],
            "defaults": {
                "direction": "DOWN",
                "timeout": "20s"
            }
        }),
    );
    commands.insert("inputText".to_owned(), json!({ "input": "string" }));
    commands.insert(
        "pasteText".to_owned(),
        json!({ "forms": ["bare", "empty mapping"] }),
    );
    commands.insert("setClipboard".to_owned(), json!({ "input": "string" }));
    commands.insert("copyTextFrom".to_owned(), json!({ "input": "selector" }));
    commands.insert(
        "eraseText".to_owned(),
        json!({
            "forms": ["bare", "integer", "mapping"],
            "mapping_keys": ["characters"],
            "defaults": { "characters": 50 }
        }),
    );
    commands.insert(
        "pressKey".to_owned(),
        json!({
            "forms": ["key string", "mapping"],
            "mapping_keys": ["key", "modifiers"],
            "supported_modifiers": ["COMMAND", "CMD", "SHIFT", "OPTION", "ALT", "CONTROL", "CTRL", "FUNCTION", "FN"]
        }),
    );
    commands.insert(
        "pressKeyCode".to_owned(),
        json!({
            "forms": ["integer", "mapping"],
            "mapping_keys": ["keyCode", "duration", "modifiers"]
        }),
    );
    commands.insert(
        "keySequence".to_owned(),
        json!({ "input": "array of integer key codes" }),
    );
    commands.insert(
        "pressButton".to_owned(),
        json!({
            "forms": ["button string", "mapping"],
            "mapping_keys": ["button", "duration"]
        }),
    );
    commands.insert(
        "selectMenuItem".to_owned(),
        json!({ "forms": ["`Section > Item` string", "mapping with `path` array"] }),
    );
    commands.insert(
        "hideKeyboard".to_owned(),
        json!({ "forms": ["bare", "empty mapping"] }),
    );
    commands.insert("assertVisible".to_owned(), json!({ "input": "selector" }));
    commands.insert(
        "assertNotVisible".to_owned(),
        json!({ "input": "selector" }),
    );
    commands.insert(
        "extendedWaitUntil".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["visible", "notVisible", "timeout"]
        }),
    );
    commands.insert(
        "waitForAnimationToEnd".to_owned(),
        json!({
            "forms": ["bare", "duration"],
            "defaults": { "timeout": "5000ms" }
        }),
    );
    commands.insert(
        "takeScreenshot".to_owned(),
        json!({ "forms": ["bare", "string artifact name"] }),
    );
    commands.insert(
        "startRecording".to_owned(),
        json!({ "forms": ["bare", "string artifact name"] }),
    );
    commands.insert(
        "stopRecording".to_owned(),
        json!({ "forms": ["bare", "empty mapping"] }),
    );
    commands.insert("openLink".to_owned(), json!({ "input": "URL string" }));
    commands.insert(
        "setLocation".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["latitude", "longitude"]
        }),
    );
    commands.insert(
        "setPermissions".to_owned(),
        json!({
            "input": "mapping of permission name to state",
            "supported_states": ["allow", "deny", "unset"]
        }),
    );
    commands.insert(
        "travel".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["direction", "distance", "speed", "latitude", "longitude"]
        }),
    );
    commands.insert(
        "addMedia".to_owned(),
        json!({ "input": "string path or array of paths" }),
    );
    commands.insert(
        "runFlow".to_owned(),
        json!({ "input": "relative YAML path" }),
    );
    commands.insert(
        "repeat".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["times", "commands"]
        }),
    );
    commands.insert(
        "retry".to_owned(),
        json!({
            "input": "mapping",
            "mapping_keys": ["times", "commands"]
        }),
    );
    JsonValue::Object(commands)
}

fn platform_support(platform: Option<ApplePlatform>) -> JsonValue {
    if let Some(platform) = platform {
        return JsonValue::Object(JsonMap::from_iter([(
            platform.to_string(),
            platform_support_entry(platform),
        )]));
    }

    JsonValue::Object(JsonMap::from_iter([
        ("ios".to_owned(), platform_support_entry(ApplePlatform::Ios)),
        (
            "macos".to_owned(),
            platform_support_entry(ApplePlatform::Macos),
        ),
        (
            "tvos".to_owned(),
            platform_support_entry(ApplePlatform::Tvos),
        ),
        (
            "visionos".to_owned(),
            platform_support_entry(ApplePlatform::Visionos),
        ),
        (
            "watchos".to_owned(),
            platform_support_entry(ApplePlatform::Watchos),
        ),
    ]))
}

fn platform_support_entry(platform: ApplePlatform) -> JsonValue {
    match platform {
        ApplePlatform::Ios => json!({
            "backend_status": "implemented",
            "flow_commands": IOS_FLOW_COMMANDS,
            "helper_commands": IOS_HELPER_COMMANDS
        }),
        ApplePlatform::Macos => json!({
            "backend_status": "implemented",
            "flow_commands": MACOS_FLOW_COMMANDS,
            "helper_commands": MACOS_HELPER_COMMANDS,
            "unsupported_commands": MACOS_UNSUPPORTED_COMMANDS,
            "notes": [
                "Modified keyboard shortcuts on macOS are not documented as stable.",
                "macOS `pressKey` supports ENTER, BACKSPACE, ESCAPE/BACK, SPACE, TAB, HOME, arrow keys, and printable characters with a known macOS key code."
            ]
        }),
        ApplePlatform::Tvos | ApplePlatform::Visionos | ApplePlatform::Watchos => json!({
            "backend_status": "not_implemented",
            "flow_commands": [],
            "helper_commands": [],
            "notes": [
                "Orbit accepts these platform values in manifest and CLI selection, but a UI automation backend is not implemented yet."
            ]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{schema_json, schema_text};
    use crate::manifest::ApplePlatform;

    #[test]
    fn schema_lists_top_level_shapes_and_commands() {
        let schema = schema_json(None);

        assert_eq!(schema["dialect"], "orbit.ui.v1");
        assert_eq!(
            schema["top_level"]["config_keys"],
            serde_json::json!(["appId", "name", "steps"])
        );
        assert!(
            schema["commands"]["launchApp"]["mapping_keys"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "permissions")
        );
    }

    #[test]
    fn schema_can_filter_platform_support() {
        let schema = schema_json(Some(ApplePlatform::Ios));
        let support = schema["platform_support"].as_object().unwrap();

        assert_eq!(support.len(), 1);
        assert_eq!(
            support["ios"]["backend_status"],
            serde_json::json!("implemented")
        );
    }

    #[test]
    fn schema_text_surfaces_intro_before_command_details() {
        let text = schema_text(Some(ApplePlatform::Ios));

        assert!(text.starts_with("Dialect: orbit.ui.v1"));
        assert!(text.contains("Use `--json` to print the raw machine-readable schema."));
        assert!(text.contains("\nNotes:\n"));
        assert!(text.contains("\nTop level:\n"));
        assert!(text.contains("\nPlatform support:\n"));
        assert!(text.contains("\nCommands:\n"));
        assert!(text.contains("    Backend status: implemented"));

        let notes = text.find("\nNotes:\n").unwrap();
        let top_level = text.find("\nTop level:\n").unwrap();
        let commands = text.find("\nCommands:\n").unwrap();
        assert!(notes < top_level);
        assert!(top_level < commands);
    }
}
