use std::path::Path;

pub(super) fn suggested_product_name(project_root: &Path) -> String {
    let raw_name = project_root
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("OrbiApp");
    pascal_case_words(raw_name, "OrbiApp")
}

pub(super) fn bundle_id_suffix(name: &str) -> String {
    let suffix = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect::<String>();
    if suffix.is_empty() {
        "app".to_owned()
    } else {
        suffix
    }
}

pub(super) fn swift_type_name(name: &str) -> String {
    let mut type_name = pascal_case_words(name, "Orbi");
    if type_name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        type_name.insert_str(0, "Orbi");
    }
    type_name
}

pub(super) fn looks_like_bundle_id(value: &str) -> bool {
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() >= 2
        && parts
            .iter()
            .all(|part| !part.is_empty() && is_bundle_id_component(part))
}

fn pascal_case_words(input: &str, fallback: &str) -> String {
    let mut value = input
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(capitalize_ascii)
        .collect::<String>();
    if value.is_empty() {
        value.push_str(fallback);
    }
    value
}

fn capitalize_ascii(part: &str) -> String {
    let mut characters = part.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    format!(
        "{}{}",
        first.to_ascii_uppercase(),
        characters.as_str().to_ascii_lowercase()
    )
}

fn is_bundle_id_component(value: &str) -> bool {
    value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-')
}
