use std::fs;
use std::path::{Path, PathBuf};

pub fn create_watch_workspace(root: &Path) -> PathBuf {
    create_workspace(
        root,
        "watch-workspace",
        &[
            (
                "Sources/App/App.swift",
                "import SwiftUI\n@main struct ExampleIOSApp: App { var body: some Scene { WindowGroup { Text(\"Phone\") } } }\n",
            ),
            (
                "Sources/WatchApp/App.swift",
                "import SwiftUI\n@main struct ExampleWatchApp: App { var body: some Scene { WindowGroup { Text(\"Watch\") } } }\n",
            ),
            (
                "Sources/WatchExtension/Extension.swift",
                "import SwiftUI\n@main struct ExampleWatchExtension: App { var body: some Scene { WindowGroup { Text(\"Ext\") } } }\n",
            ),
        ],
        &serde_json::json!({
            "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
            "name": "WatchFixture",
            "bundle_id": "dev.orbit.fixture.watch",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0",
                "watchos": "11.0"
            },
            "sources": ["Sources/App"],
            "watch": {
                "sources": ["Sources/WatchApp"],
                "extension": {
                    "sources": ["Sources/WatchExtension"],
                    "entry": {
                        "class": "WatchExtensionDelegate"
                    }
                }
            }
        }),
    )
}

pub fn create_signing_workspace(root: &Path) -> PathBuf {
    create_workspace(
        root,
        "signing-workspace",
        &[(
            "Sources/App/App.swift",
            "import SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(\"App\") } } }\n",
        )],
        &serde_json::json!({
            "$schema": "https://orbit.dev/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture",
            "version": "0.1.0",
            "build": 1,
            "team_id": "TEAM123456",
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"]
        }),
    )
}

fn create_workspace(
    root: &Path,
    name: &str,
    source_files: &[(&str, &str)],
    manifest: &serde_json::Value,
) -> PathBuf {
    let workspace = root.join(name);
    for (relative_path, contents) in source_files {
        let path = workspace.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fs::write(
        workspace.join("orbit.json"),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
    workspace
}
