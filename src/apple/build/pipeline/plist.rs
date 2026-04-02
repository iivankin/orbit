use std::collections::BTreeMap;

use super::*;

pub(super) fn needs_info_plist(target_kind: TargetKind) -> bool {
    target_kind.is_bundle()
}

pub(super) fn write_info_plist(
    project: &ProjectContext,
    toolchain: &Toolchain,
    target: &TargetManifest,
    bundle_root: &Path,
) -> Result<()> {
    let mut plist = Dictionary::new();
    let build_metadata = toolchain.bundle_build_metadata()?;
    plist.insert(
        "CFBundleIdentifier".to_owned(),
        Value::String(target.bundle_id.clone()),
    );
    plist.insert(
        "CFBundleExecutable".to_owned(),
        Value::String(target.name.clone()),
    );
    plist.insert(
        "CFBundleName".to_owned(),
        Value::String(target.name.clone()),
    );
    plist.insert(
        "CFBundleDisplayName".to_owned(),
        Value::String(
            target
                .display_name
                .clone()
                .unwrap_or_else(|| target.name.clone()),
        ),
    );
    plist.insert(
        "CFBundleShortVersionString".to_owned(),
        Value::String(project.resolved_manifest.version.clone()),
    );
    plist.insert(
        "CFBundleVersion".to_owned(),
        Value::String(
            target
                .build_number
                .clone()
                .unwrap_or_else(|| project.resolved_manifest.version.clone()),
        ),
    );
    plist.insert(
        "CFBundleInfoDictionaryVersion".to_owned(),
        Value::String("6.0".to_owned()),
    );
    plist.insert(
        "CFBundleDevelopmentRegion".to_owned(),
        Value::String("en".to_owned()),
    );
    plist.insert(
        "CFBundleSupportedPlatforms".to_owned(),
        Value::Array(vec![Value::String(
            toolchain.info_plist_supported_platform().to_owned(),
        )]),
    );
    plist.insert(
        "BuildMachineOSBuild".to_owned(),
        Value::String(build_metadata.build_machine_os_build),
    );
    plist.insert(
        "DTCompiler".to_owned(),
        Value::String(build_metadata.compiler),
    );
    plist.insert(
        "DTPlatformBuild".to_owned(),
        Value::String(build_metadata.platform_build),
    );
    plist.insert(
        "DTPlatformName".to_owned(),
        Value::String(build_metadata.platform_name),
    );
    plist.insert(
        "DTPlatformVersion".to_owned(),
        Value::String(build_metadata.platform_version),
    );
    plist.insert(
        "DTSDKBuild".to_owned(),
        Value::String(build_metadata.sdk_build),
    );
    plist.insert(
        "DTSDKName".to_owned(),
        Value::String(build_metadata.sdk_name),
    );
    plist.insert("DTXcode".to_owned(), Value::String(build_metadata.xcode));
    plist.insert(
        "DTXcodeBuild".to_owned(),
        Value::String(build_metadata.xcode_build),
    );

    match target.kind {
        TargetKind::App => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("APPL".to_owned()),
            );
            if matches!(toolchain.platform, ApplePlatform::Ios) {
                plist.insert("LSRequiresIPhoneOS".to_owned(), Value::Boolean(true));
                add_ios_app_plist_defaults(
                    &mut plist,
                    target,
                    toolchain.info_plist_supported_platform() == "iPhoneOS",
                )?;
                plist.insert(
                    "MinimumOSVersion".to_owned(),
                    Value::String(toolchain.deployment_target.clone()),
                );
            } else if matches!(toolchain.platform, ApplePlatform::Macos) {
                plist.insert(
                    "LSMinimumSystemVersion".to_owned(),
                    Value::String(toolchain.deployment_target.clone()),
                );
            } else {
                plist.insert(
                    "MinimumOSVersion".to_owned(),
                    Value::String(toolchain.deployment_target.clone()),
                );
            }
        }
        TargetKind::WatchApp => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("APPL".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
            plist.insert("WKWatchKitApp".to_owned(), Value::Boolean(true));
            if let Some(companion_bundle_id) =
                parent_bundle_id(project, &target.name, TargetKind::App)
            {
                plist.insert(
                    "WKCompanionAppBundleIdentifier".to_owned(),
                    Value::String(companion_bundle_id),
                );
            }
        }
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("XPC!".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
            let mut extension = extension_plist(
                target
                    .extension
                    .as_ref()
                    .context("extension configuration missing")?,
            )?;
            if matches!(target.kind, TargetKind::WatchExtension) {
                let watch_bundle_id = parent_bundle_id(project, &target.name, TargetKind::WatchApp)
                    .context("watch extension must be hosted by a watch app target")?;
                merge_extension_attributes(
                    &mut extension,
                    Dictionary::from_iter([(
                        "WKAppBundleIdentifier".to_owned(),
                        Value::String(watch_bundle_id),
                    )]),
                );
            }
            plist.insert("NSExtension".to_owned(), Value::Dictionary(extension));
        }
        TargetKind::Framework => {
            plist.insert(
                "CFBundlePackageType".to_owned(),
                Value::String("FMWK".to_owned()),
            );
            plist.insert(
                "MinimumOSVersion".to_owned(),
                Value::String(toolchain.deployment_target.clone()),
            );
        }
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Executable => {
            bail!("non-bundle targets do not write Info.plist files")
        }
    }

    apply_info_plist_overrides(&mut plist, &target.info_plist)?;

    let metadata_root = bundle_metadata_root(toolchain, target.kind, bundle_root);
    let path = metadata_root.join("Info.plist");
    ensure_parent_dir(&path)?;
    Value::Dictionary(plist)
        .to_file_xml(&path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    write_bundle_pkg_info(toolchain, target.kind, bundle_root)
}

fn add_ios_app_plist_defaults(
    plist: &mut Dictionary,
    target: &TargetManifest,
    is_device_build: bool,
) -> Result<()> {
    let families = resolved_ios_device_families(target.ios.as_ref());
    plist.insert(
        "UIDeviceFamily".to_owned(),
        Value::Array(
            families
                .iter()
                .map(|family| Value::Integer(ios_device_family_code(*family).into()))
                .collect(),
        ),
    );
    let required_capabilities = target
        .ios
        .as_ref()
        .and_then(|ios| ios.required_device_capabilities.as_ref());
    if is_device_build || required_capabilities.is_some() {
        plist.insert(
            "UIRequiredDeviceCapabilities".to_owned(),
            Value::Array(
                required_capabilities
                    .map(|capabilities| {
                        capabilities
                            .iter()
                            .cloned()
                            .map(Value::String)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_else(|| vec![Value::String("arm64".to_owned())]),
            ),
        );
    }
    plist.insert(
        "UIApplicationSupportsIndirectInputEvents".to_owned(),
        Value::Boolean(true),
    );
    plist.insert(
        "UILaunchScreen".to_owned(),
        Value::Dictionary(Dictionary::from_iter([(
            "UILaunchScreen".to_owned(),
            Value::Dictionary(launch_screen_dictionary(target.ios.as_ref())?),
        )])),
    );
    plist.insert(
        "UIStatusBarStyle".to_owned(),
        Value::String("UIStatusBarStyleDefault".to_owned()),
    );
    if families.contains(&IosDeviceFamily::Iphone) {
        plist.insert(
            "UISupportedInterfaceOrientations~iphone".to_owned(),
            Value::Array(resolved_ios_orientations(
                target.ios.as_ref().and_then(|ios| {
                    ios.supported_orientations
                        .as_ref()
                        .and_then(|orientations| orientations.iphone.as_ref())
                }),
                &[
                    IosInterfaceOrientation::Portrait,
                    IosInterfaceOrientation::LandscapeLeft,
                    IosInterfaceOrientation::LandscapeRight,
                ],
            )),
        );
    }
    if families.contains(&IosDeviceFamily::Ipad) {
        plist.insert(
            "UISupportedInterfaceOrientations~ipad".to_owned(),
            Value::Array(resolved_ios_orientations(
                target.ios.as_ref().and_then(|ios| {
                    ios.supported_orientations
                        .as_ref()
                        .and_then(|orientations| orientations.ipad.as_ref())
                }),
                &[
                    IosInterfaceOrientation::Portrait,
                    IosInterfaceOrientation::PortraitUpsideDown,
                    IosInterfaceOrientation::LandscapeLeft,
                    IosInterfaceOrientation::LandscapeRight,
                ],
            )),
        );
    }
    Ok(())
}

fn apply_info_plist_overrides(
    plist: &mut Dictionary,
    overrides: &BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    for (key, value) in overrides {
        plist.insert(key.clone(), json_to_plist(value)?);
    }
    Ok(())
}

fn resolved_ios_device_families(config: Option<&IosTargetManifest>) -> Vec<IosDeviceFamily> {
    config
        .and_then(|ios| ios.device_families.clone())
        .unwrap_or_else(|| vec![IosDeviceFamily::Iphone, IosDeviceFamily::Ipad])
}

fn ios_device_family_code(family: IosDeviceFamily) -> i64 {
    match family {
        IosDeviceFamily::Iphone => 1,
        IosDeviceFamily::Ipad => 2,
    }
}

fn launch_screen_dictionary(config: Option<&IosTargetManifest>) -> Result<Dictionary> {
    let mut dictionary = Dictionary::new();
    let Some(launch_screen) = config.and_then(|ios| ios.launch_screen.as_ref()) else {
        return Ok(dictionary);
    };
    for (key, value) in launch_screen {
        dictionary.insert(key.clone(), json_to_plist(value)?);
    }
    Ok(dictionary)
}

fn resolved_ios_orientations(
    configured: Option<&Vec<IosInterfaceOrientation>>,
    defaults: &[IosInterfaceOrientation],
) -> Vec<Value> {
    configured
        .map(|orientations| orientations.as_slice())
        .unwrap_or(defaults)
        .iter()
        .map(|orientation| Value::String(ios_orientation_name(*orientation).to_owned()))
        .collect()
}

fn ios_orientation_name(orientation: IosInterfaceOrientation) -> &'static str {
    match orientation {
        IosInterfaceOrientation::Portrait => "UIInterfaceOrientationPortrait",
        IosInterfaceOrientation::PortraitUpsideDown => "UIInterfaceOrientationPortraitUpsideDown",
        IosInterfaceOrientation::LandscapeLeft => "UIInterfaceOrientationLandscapeLeft",
        IosInterfaceOrientation::LandscapeRight => "UIInterfaceOrientationLandscapeRight",
    }
}

fn write_bundle_pkg_info(
    toolchain: &Toolchain,
    target_kind: TargetKind,
    bundle_root: &Path,
) -> Result<()> {
    let contents = match target_kind {
        TargetKind::App | TargetKind::WatchApp => Some("APPL????"),
        TargetKind::AppExtension | TargetKind::WatchExtension | TargetKind::WidgetExtension => {
            Some("XPC!????")
        }
        TargetKind::Framework => Some("FMWK????"),
        TargetKind::StaticLibrary | TargetKind::DynamicLibrary | TargetKind::Executable => None,
    };

    let Some(contents) = contents else {
        return Ok(());
    };

    let path = bundle_metadata_root(toolchain, target_kind, bundle_root).join("PkgInfo");
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
}

pub(super) fn extension_plist(config: &ExtensionManifest) -> Result<Dictionary> {
    let mut extension = Dictionary::new();
    for (key, value) in &config.extra {
        extension.insert(key.clone(), json_to_plist(value)?);
    }
    extension.insert(
        "NSExtensionPointIdentifier".to_owned(),
        Value::String(config.point_identifier.clone()),
    );
    extension.insert(
        "NSExtensionPrincipalClass".to_owned(),
        Value::String(config.principal_class.clone()),
    );
    Ok(extension)
}

pub(super) fn json_to_plist(value: &serde_json::Value) -> Result<Value> {
    Ok(match value {
        serde_json::Value::Null => bail!("null values are not supported in extension plist extras"),
        serde_json::Value::Bool(value) => Value::Boolean(*value),
        serde_json::Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Value::Integer(integer.into())
            } else if let Some(float) = value.as_f64() {
                Value::Real(float)
            } else {
                bail!("JSON number `{value}` is not representable in a plist");
            }
        }
        serde_json::Value::String(value) => Value::String(value.clone()),
        serde_json::Value::Array(values) => Value::Array(
            values
                .iter()
                .map(json_to_plist)
                .collect::<Result<Vec<_>>>()?,
        ),
        serde_json::Value::Object(values) => Value::Dictionary(Dictionary::from_iter(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_plist(value)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
    })
}

pub(super) fn merge_extension_attributes(extension: &mut Dictionary, attributes: Dictionary) {
    if !extension.contains_key("NSExtensionAttributes") {
        extension.insert(
            "NSExtensionAttributes".to_owned(),
            Value::Dictionary(Dictionary::new()),
        );
    }
    let existing_attributes = extension
        .get_mut("NSExtensionAttributes")
        .and_then(Value::as_dictionary_mut)
        .expect("NSExtensionAttributes must remain a dictionary");
    for (key, value) in attributes {
        existing_attributes.insert(key, value);
    }
}

fn parent_bundle_id(
    project: &ProjectContext,
    target_name: &str,
    parent_kind: TargetKind,
) -> Option<String> {
    project
        .resolved_manifest
        .targets
        .iter()
        .find(|candidate| {
            candidate.kind == parent_kind
                && candidate
                    .dependencies
                    .iter()
                    .any(|name| name == target_name)
        })
        .map(|target| target.bundle_id.clone())
}
