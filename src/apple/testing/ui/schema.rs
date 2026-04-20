pub(crate) const FLOW_SCHEMA_FILENAME: &str = "orbi-ui-test.v1.json";
pub(crate) const FLOW_SCHEMA_URL: &str = concat!(
    "https://orbitstorage.dev/schemas/orbi-ui-test.v1-orbi-",
    env!("CARGO_PKG_VERSION"),
    ".json"
);

pub(crate) fn supports_flow_schema(schema: &str) -> bool {
    schema_matches_file_name(schema_file_name(schema), FLOW_SCHEMA_FILENAME)
}

fn schema_file_name(schema: &str) -> &str {
    schema.rsplit(['/', '\\']).next().unwrap_or(schema)
}

fn schema_matches_file_name(schema_name: &str, local_file_name: &str) -> bool {
    if schema_name == local_file_name {
        return true;
    }

    let Some(local_stem) = local_file_name.strip_suffix(".json") else {
        return false;
    };
    let Some(version_suffix) = schema_name
        .strip_prefix(local_stem)
        .and_then(|value| value.strip_prefix("-orbi-"))
        .and_then(|value| value.strip_suffix(".json"))
    else {
        return false;
    };

    !version_suffix.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{FLOW_SCHEMA_FILENAME, FLOW_SCHEMA_URL, supports_flow_schema};

    #[test]
    fn supports_local_flow_schema_path() {
        assert!(supports_flow_schema(FLOW_SCHEMA_FILENAME));
        assert!(supports_flow_schema(
            "/tmp/.orbi/schemas/orbi-ui-test.v1.json"
        ));
    }

    #[test]
    fn supports_published_version_pinned_flow_schema_url() {
        assert!(supports_flow_schema(FLOW_SCHEMA_URL));
        assert!(supports_flow_schema(
            "https://orbitstorage.dev/schemas/orbi-ui-test.v1-orbi-9.9.9.json"
        ));
    }

    #[test]
    fn rejects_unrelated_schema_name() {
        assert!(!supports_flow_schema("apple-app.v1.json"));
    }
}
