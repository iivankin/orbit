use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct GitSwiftPackageFixture {
    pub remote_url: String,
    pub initial_revision: String,
    pub latest_revision: String,
}

pub struct SemverGitSwiftPackageFixture {
    pub remote_url: String,
    pub initial_revision: String,
    pub matching_revision: String,
    pub non_matching_revision: String,
}

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
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
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
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
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

pub fn create_mixed_language_workspace(root: &Path) -> PathBuf {
    create_workspace(
        root,
        "mixed-language-workspace",
        &[
            (
                "Sources/App/App.swift",
                "import SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(\"App\") } } }\n",
            ),
            (
                "Sources/App/Bridge.m",
                "#import \"Bridge.h\"\nint orbit_add(int a, int b) { return a + b; }\n",
            ),
            ("Sources/App/Bridge.h", "int orbit_add(int a, int b);\n"),
        ],
        &serde_json::json!({
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture.mixed",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"]
        }),
    )
}

pub fn create_swift_package_workspace(root: &Path) -> PathBuf {
    create_workspace(
        root,
        "swift-package-workspace",
        &[
            (
                "Sources/App/App.swift",
                "import OrbitPkg\nimport SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(orbitMessage()) } } }\n",
            ),
            (
                "Packages/OrbitPkg/Package.swift",
                "// fixture handled by swift package dump-package mock\n",
            ),
            (
                "Packages/OrbitPkg/Sources/OrbitPkg/OrbitPkg.swift",
                "public func orbitMessage() -> String { \"Pkg\" }\n",
            ),
        ],
        &serde_json::json!({
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture.package",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "dependencies": {
                "OrbitPkg": {
                    "path": "Packages/OrbitPkg"
                }
            }
        }),
    )
}

pub fn create_xcframework_workspace(root: &Path) -> PathBuf {
    create_workspace(
        root,
        "xcframework-workspace",
        &[
            (
                "Sources/App/App.swift",
                "import SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(\"App\") } } }\n",
            ),
            (
                "Vendor/VendorSDK.xcframework/Info.plist",
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>AvailableLibraries</key>
  <array>
    <dict>
      <key>LibraryIdentifier</key>
      <string>ios-arm64_x86_64-simulator</string>
      <key>LibraryPath</key>
      <string>libVendorSDK.a</string>
      <key>SupportedPlatform</key>
      <string>ios</string>
      <key>SupportedPlatformVariant</key>
      <string>simulator</string>
      <key>SupportedArchitectures</key>
      <array>
        <string>arm64</string>
        <string>x86_64</string>
      </array>
    </dict>
  </array>
</dict>
</plist>
"#,
            ),
            (
                "Vendor/VendorSDK.xcframework/ios-arm64_x86_64-simulator/libVendorSDK.a",
                "archive",
            ),
        ],
        &serde_json::json!({
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture.xcframework",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "dependencies": {
                "VendorSDK": {
                    "xcframework": "Vendor/VendorSDK.xcframework",
                    "embed": false
                }
            }
        }),
    )
}

pub fn create_git_swift_package_workspace(root: &Path) -> (PathBuf, GitSwiftPackageFixture) {
    let package_repo = root.join("orbitpkg-remote");
    fs::create_dir_all(package_repo.join("Sources/OrbitPkg")).unwrap();
    fs::write(
        package_repo.join("Package.swift"),
        "// fixture handled by swift package dump-package mock\n",
    )
    .unwrap();
    fs::write(
        package_repo.join("Sources/OrbitPkg/OrbitPkg.swift"),
        "public func orbitMessage() -> String { \"Pkg v1\" }\n",
    )
    .unwrap();

    run_git(root, ["init", package_repo.to_str().unwrap()]);
    run_git_in(&package_repo, ["add", "."]);
    run_git_commit(&package_repo, "Initial package revision");
    let initial_revision = git_output(&package_repo, ["rev-parse", "HEAD"]);

    fs::write(
        package_repo.join("Sources/OrbitPkg/OrbitPkg.swift"),
        "public func orbitMessage() -> String { \"Pkg v2\" }\n",
    )
    .unwrap();
    run_git_in(&package_repo, ["add", "."]);
    run_git_commit(&package_repo, "Update package revision");
    let latest_revision = git_output(&package_repo, ["rev-parse", "HEAD"]);

    let workspace = create_workspace(
        root,
        "git-swift-package-workspace",
        &[(
            "Sources/App/App.swift",
            "import OrbitPkg\nimport SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(orbitMessage()) } } }\n",
        )],
        &serde_json::json!({
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture.gitpackage",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "dependencies": {
                "OrbitPkg": {
                    "git": package_repo.to_string_lossy(),
                    "revision": initial_revision
                }
            }
        }),
    );

    (
        workspace,
        GitSwiftPackageFixture {
            remote_url: package_repo.to_string_lossy().into_owned(),
            initial_revision,
            latest_revision,
        },
    )
}

pub fn create_semver_git_swift_package_workspace(
    root: &Path,
) -> (PathBuf, SemverGitSwiftPackageFixture) {
    let package_repo = root.join("orbitpkg-semver-remote");
    fs::create_dir_all(package_repo.join("Sources/OrbitPkg")).unwrap();
    fs::write(
        package_repo.join("Package.swift"),
        "// fixture handled by swift package dump-package mock\n",
    )
    .unwrap();

    run_git(root, ["init", package_repo.to_str().unwrap()]);

    fs::write(
        package_repo.join("Sources/OrbitPkg/OrbitPkg.swift"),
        "public func orbitMessage() -> String { \"Pkg 1.0.0\" }\n",
    )
    .unwrap();
    run_git_in(&package_repo, ["add", "."]);
    run_git_commit(&package_repo, "Initial semver package revision");
    let initial_revision = git_output(&package_repo, ["rev-parse", "HEAD"]);
    run_git_in(&package_repo, ["tag", "v1.0.0"]);

    fs::write(
        package_repo.join("Sources/OrbitPkg/OrbitPkg.swift"),
        "public func orbitMessage() -> String { \"Pkg 1.2.0\" }\n",
    )
    .unwrap();
    run_git_in(&package_repo, ["add", "."]);
    run_git_commit(&package_repo, "Matching semver package revision");
    let matching_revision = git_output(&package_repo, ["rev-parse", "HEAD"]);
    run_git_in(&package_repo, ["tag", "v1.2.0"]);

    fs::write(
        package_repo.join("Sources/OrbitPkg/OrbitPkg.swift"),
        "public func orbitMessage() -> String { \"Pkg 2.0.0\" }\n",
    )
    .unwrap();
    run_git_in(&package_repo, ["add", "."]);
    run_git_commit(&package_repo, "Non matching semver package revision");
    let non_matching_revision = git_output(&package_repo, ["rev-parse", "HEAD"]);
    run_git_in(&package_repo, ["tag", "v2.0.0"]);

    let workspace = create_workspace(
        root,
        "semver-git-swift-package-workspace",
        &[(
            "Sources/App/App.swift",
            "import OrbitPkg\nimport SwiftUI\n@main struct ExampleApp: App { var body: some Scene { WindowGroup { Text(orbitMessage()) } } }\n",
        )],
        &serde_json::json!({
            "$schema": "/tmp/.orbit/schemas/apple-app.v1.json",
            "name": "ExampleApp",
            "bundle_id": "dev.orbit.fixture.semvergitpackage",
            "version": "0.1.0",
            "build": 1,
            "platforms": {
                "ios": "18.0"
            },
            "sources": ["Sources/App"],
            "dependencies": {
                "OrbitPkg": {
                    "git": package_repo.to_string_lossy(),
                    "version": "1.0.0"
                }
            }
        }),
    );
    fs::write(
        workspace.join("orbit.lock"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "dependencies": {
                "OrbitPkg": {
                    "git": package_repo.to_string_lossy(),
                    "version": "1.0.0",
                    "revision": initial_revision
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    (
        workspace,
        SemverGitSwiftPackageFixture {
            remote_url: package_repo.to_string_lossy().into_owned(),
            initial_revision,
            matching_revision,
            non_matching_revision,
        },
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

fn run_git<const N: usize>(cwd: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
}

fn run_git_in<const N: usize>(repo: &Path, args: [&str; N]) {
    run_git(repo, args);
}

fn run_git_commit(repo: &Path, message: &str) {
    let output = Command::new("git")
        .current_dir(repo)
        .args([
            "-c",
            "user.name=Orbit Tests",
            "-c",
            "user.email=orbit-tests@example.com",
            "commit",
            "-m",
            message,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
}

fn git_output<const N: usize>(repo: &Path, args: [&str; N]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
