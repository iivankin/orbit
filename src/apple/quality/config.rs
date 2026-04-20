use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::apple::manifest::{FormatQualityManifest, LintQualityManifest};
use crate::context::ProjectContext;

const ORBI_DEFAULT_SWIFT_FORMAT_INDENT_WIDTH: u64 = 4;

pub(super) struct IgnoreMatcher {
    root: PathBuf,
    globs: GlobSet,
}

impl IgnoreMatcher {
    pub(super) fn is_ignored(&self, path: &Path) -> bool {
        self.globs.is_match(normalize_match_path(&self.root, path))
    }
}

pub(super) struct LintQualityConfig {
    ignore_matcher: Option<IgnoreMatcher>,
    pub(super) configuration_json: Option<String>,
}

impl LintQualityConfig {
    pub(super) fn ignore_matcher(&self) -> Option<&IgnoreMatcher> {
        self.ignore_matcher.as_ref()
    }
}

pub(super) fn lint_quality_config(project: &ProjectContext) -> Result<LintQualityConfig> {
    let quality = &project.resolved_manifest.quality.lint;
    let configuration_json = if quality.rules.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&quality.rules).context("failed to serialize Orbi lint rules")?)
    };
    Ok(LintQualityConfig {
        ignore_matcher: build_ignore_matcher(&project.root, quality)?,
        configuration_json,
    })
}

pub(super) fn format_ignore_matcher(project: &ProjectContext) -> Result<Option<IgnoreMatcher>> {
    build_ignore_matcher(&project.root, &project.resolved_manifest.quality.format)
}

pub(super) fn format_configuration_json(
    project: &ProjectContext,
    files: &[PathBuf],
) -> Result<Option<String>> {
    let quality = &project.resolved_manifest.quality.format;
    let mut configuration = load_swift_format_configuration(project.root.as_path())?;
    apply_orbi_format_defaults(&mut configuration);

    if quality.editorconfig
        && let Some(settings) = resolve_editorconfig_settings(project.root.as_path(), files)?
    {
        apply_editorconfig_to_swift_format(&mut configuration, &settings);
    }

    apply_orbi_format_rules(&mut configuration, &quality.rules);

    if configuration.is_empty() {
        return Ok(None);
    }

    Ok(Some(
        serde_json::to_string(&JsonValue::Object(configuration))
            .context("failed to serialize Orbi formatter configuration")?,
    ))
}

fn apply_orbi_format_defaults(configuration: &mut JsonMap<String, JsonValue>) {
    // Orbi uses a 4-space indentation baseline unless the project overrides it.
    configuration
        .entry("indentation".to_owned())
        .or_insert_with(|| serde_json::json!({ "spaces": ORBI_DEFAULT_SWIFT_FORMAT_INDENT_WIDTH }));
}

fn build_ignore_matcher<T>(root: &Path, config: &T) -> Result<Option<IgnoreMatcher>>
where
    T: IgnorePatterns,
{
    let patterns = config.ignore_patterns();
    if patterns.is_empty() {
        return Ok(None);
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let normalized = normalize_glob_pattern(pattern);
        let glob = GlobBuilder::new(&normalized)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .with_context(|| format!("invalid quality ignore glob `{pattern}`"))?;
        builder.add(glob);
    }
    Ok(Some(IgnoreMatcher {
        root: root.to_path_buf(),
        globs: builder
            .build()
            .context("failed to compile quality ignore globs")?,
    }))
}

trait IgnorePatterns {
    fn ignore_patterns(&self) -> &[String];
}

impl IgnorePatterns for LintQualityManifest {
    fn ignore_patterns(&self) -> &[String] {
        &self.ignore
    }
}

impl IgnorePatterns for FormatQualityManifest {
    fn ignore_patterns(&self) -> &[String] {
        &self.ignore
    }
}

fn normalize_glob_pattern(pattern: &str) -> String {
    let mut normalized = pattern.trim().replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_owned();
    }
    if normalized.ends_with('/') {
        normalized.push_str("**");
    }
    if !normalized.contains('/') && !normalized.starts_with("**/") {
        normalized = format!("**/{normalized}");
    }
    normalized
}

fn normalize_match_path(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

fn load_swift_format_configuration(project_root: &Path) -> Result<JsonMap<String, JsonValue>> {
    let Some(path) = swift_format_configuration_path(project_root) else {
        return Ok(JsonMap::new());
    };

    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: JsonValue = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    match value {
        JsonValue::Object(object) => Ok(object),
        _ => bail!(
            "Swift format configuration `{}` must be a JSON object",
            path.display()
        ),
    }
}

fn swift_format_configuration_path(project_root: &Path) -> Option<PathBuf> {
    [".swift-format", ".swift-format.json"]
        .into_iter()
        .map(|candidate| project_root.join(candidate))
        .find(|path| path.exists())
}

fn apply_orbi_format_rules(
    configuration: &mut JsonMap<String, JsonValue>,
    rules: &std::collections::BTreeMap<String, JsonValue>,
) {
    for (key, value) in rules {
        if key
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase())
        {
            apply_orbi_format_rule(configuration, key, value);
        } else {
            configuration.insert(key.clone(), value.clone());
        }
    }
}

fn apply_orbi_format_rule(
    configuration: &mut JsonMap<String, JsonValue>,
    rule_id: &str,
    value: &JsonValue,
) {
    let rules = configuration
        .entry("rules".to_owned())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    let JsonValue::Object(rule_configuration) = rules else {
        return;
    };

    let parsed = parse_rule_setting(value);
    if let Some(enabled) = parsed.enabled {
        rule_configuration.insert(rule_id.to_owned(), JsonValue::Bool(enabled));
    }
    if let Some(options) = parsed.options {
        configuration.insert(format_rule_property_key(rule_id), options);
    }
}

fn format_rule_property_key(rule_id: &str) -> String {
    let mut characters = rule_id.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    format!("{}{}", first.to_ascii_lowercase(), characters.as_str())
}

struct ParsedRuleSetting<'a> {
    enabled: Option<bool>,
    options: Option<JsonValue>,
    _severity: Option<&'a str>,
}

fn parse_rule_setting(value: &JsonValue) -> ParsedRuleSetting<'_> {
    match value {
        JsonValue::Null => ParsedRuleSetting {
            enabled: Some(false),
            options: None,
            _severity: None,
        },
        JsonValue::Bool(enabled) => ParsedRuleSetting {
            enabled: Some(*enabled),
            options: None,
            _severity: None,
        },
        JsonValue::String(level) => ParsedRuleSetting {
            enabled: Some(!matches!(normalize_rule_level(level), Some(RuleLevel::Off))),
            options: None,
            _severity: normalize_rule_level(level).map(RuleLevel::as_str),
        },
        JsonValue::Array(values) if !values.is_empty() => {
            if let Some(level) = values.first().and_then(JsonValue::as_str)
                && let Some(level) = normalize_rule_level(level)
            {
                return ParsedRuleSetting {
                    enabled: Some(level != RuleLevel::Off),
                    options: values.get(1).cloned(),
                    _severity: Some(level.as_str()),
                };
            }
            ParsedRuleSetting {
                enabled: Some(true),
                options: Some(value.clone()),
                _severity: None,
            }
        }
        _ => ParsedRuleSetting {
            enabled: Some(true),
            options: Some(value.clone()),
            _severity: None,
        },
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RuleLevel {
    Off,
    Warn,
    Error,
}

impl RuleLevel {
    fn as_str(self) -> &'static str {
        match self {
            RuleLevel::Off => "off",
            RuleLevel::Warn => "warn",
            RuleLevel::Error => "error",
        }
    }
}

fn normalize_rule_level(level: &str) -> Option<RuleLevel> {
    match level.trim().to_ascii_lowercase().as_str() {
        "off" => Some(RuleLevel::Off),
        "warn" | "warning" => Some(RuleLevel::Warn),
        "error" | "err" => Some(RuleLevel::Error),
        _ => None,
    }
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
struct EditorConfigSettings {
    indent_style: Option<IndentStyle>,
    indent_size: Option<EditorConfigIndentSize>,
    tab_width: Option<u64>,
    max_line_length: Option<u64>,
}

impl EditorConfigSettings {
    fn apply_property(&mut self, key: &str, value: &str) {
        let normalized_value = value.trim();
        match key {
            "indent_style" => {
                self.indent_style = match normalized_value.to_ascii_lowercase().as_str() {
                    "space" => Some(IndentStyle::Spaces),
                    "tab" => Some(IndentStyle::Tabs),
                    _ => None,
                };
            }
            "indent_size" => {
                self.indent_size = match normalized_value.to_ascii_lowercase().as_str() {
                    "tab" => Some(EditorConfigIndentSize::Tab),
                    _ => normalized_value
                        .parse::<u64>()
                        .ok()
                        .map(EditorConfigIndentSize::Width),
                };
            }
            "tab_width" => {
                self.tab_width = normalized_value.parse::<u64>().ok();
            }
            "max_line_length" => {
                self.max_line_length = normalized_value.parse::<u64>().ok();
            }
            _ => {}
        }
    }

    fn merge(&mut self, other: &EditorConfigSettings) {
        self.indent_style = other.indent_style.or(self.indent_style);
        self.indent_size = other.indent_size.or(self.indent_size);
        self.tab_width = other.tab_width.or(self.tab_width);
        self.max_line_length = other.max_line_length.or(self.max_line_length);
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum IndentStyle {
    Spaces,
    Tabs,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum EditorConfigIndentSize {
    Width(u64),
    Tab,
}

struct EditorConfigSection {
    matcher: GlobSet,
    settings: EditorConfigSettings,
}

fn resolve_editorconfig_settings(
    project_root: &Path,
    files: &[PathBuf],
) -> Result<Option<EditorConfigSettings>> {
    let path = project_root.join(".editorconfig");
    if !path.exists() {
        return Ok(None);
    }

    let sections = parse_editorconfig(&path)?;
    if sections.is_empty() {
        return Ok(None);
    }

    let mut resolved: Option<(PathBuf, EditorConfigSettings)> = None;
    for file in files {
        let relative = normalize_match_path(project_root, file);
        let mut current = EditorConfigSettings::default();
        for section in &sections {
            if section.matcher.is_match(&relative) {
                current.merge(&section.settings);
            }
        }

        match &resolved {
            Some((other_file, other_settings)) if *other_settings != current => {
                bail!(
                    "`.editorconfig` resolves conflicting Swift formatting settings for `{}` and `{}`; Orbi currently requires one shared formatter profile per run",
                    other_file.display(),
                    file.display()
                );
            }
            Some(_) => {}
            None => resolved = Some((file.clone(), current)),
        }
    }

    Ok(resolved.map(|(_, settings)| settings))
}

fn parse_editorconfig(path: &Path) -> Result<Vec<EditorConfigSection>> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut sections = Vec::new();
    let mut current_section: Option<usize> = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        if let Some(pattern) = line
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            sections.push(EditorConfigSection {
                matcher: compile_editorconfig_matcher(pattern)?,
                settings: EditorConfigSettings::default(),
            });
            current_section = Some(sections.len() - 1);
            continue;
        }

        let Some(separator_index) = line.find(['=', ':']) else {
            continue;
        };
        let key = line[..separator_index].trim().to_ascii_lowercase();
        let value = line[separator_index + 1..].trim();
        if let Some(index) = current_section {
            sections[index].settings.apply_property(&key, value);
        }
    }

    Ok(sections)
}

fn compile_editorconfig_matcher(pattern: &str) -> Result<GlobSet> {
    let normalized = normalize_glob_pattern(pattern);
    let glob = GlobBuilder::new(&normalized)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .with_context(|| format!("invalid .editorconfig section `{pattern}`"))?;
    let mut builder = GlobSetBuilder::new();
    builder.add(glob);
    builder
        .build()
        .context("failed to compile .editorconfig matcher")
}

fn apply_editorconfig_to_swift_format(
    configuration: &mut JsonMap<String, JsonValue>,
    settings: &EditorConfigSettings,
) {
    if let Some(line_length) = settings.max_line_length {
        configuration.insert("lineLength".to_owned(), JsonValue::from(line_length));
    }

    match settings.indent_style {
        Some(IndentStyle::Spaces) => {
            let indent_width = settings
                .indent_size
                .and_then(EditorConfigIndentSize::width)
                .or(settings.tab_width);
            if let Some(indent_width) = indent_width {
                configuration.insert(
                    "indentation".to_owned(),
                    serde_json::json!({ "spaces": indent_width }),
                );
                configuration.insert("tabWidth".to_owned(), JsonValue::from(indent_width));
            }
        }
        Some(IndentStyle::Tabs) => {
            configuration.insert("indentation".to_owned(), serde_json::json!({ "tabs": 1 }));
            if let Some(tab_width) = settings
                .tab_width
                .or_else(|| settings.indent_size.and_then(EditorConfigIndentSize::width))
            {
                configuration.insert("tabWidth".to_owned(), JsonValue::from(tab_width));
            }
        }
        None => {
            if let Some(tab_width) = settings.tab_width {
                configuration.insert("tabWidth".to_owned(), JsonValue::from(tab_width));
            }
        }
    }
}

impl EditorConfigIndentSize {
    fn width(self) -> Option<u64> {
        match self {
            EditorConfigIndentSize::Width(width) => Some(width),
            EditorConfigIndentSize::Tab => None,
        }
    }
}
