use std::path::Path;

use super::write_executable;

pub fn create_security_mock(mock_bin: &Path, db_path: &Path) {
    write_executable(
        &mock_bin.join("security"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "security $@" >> "$MOCK_LOG"
db="{db}"
cmd="$1"
shift
case "$cmd" in
  add-generic-password)
    account=""
    service=""
    password=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        -w) password="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    mkdir -p "$(dirname "$db")"
    tmp="$db.tmp"
    touch "$db"
    grep -v "^$service|$account|" "$db" > "$tmp" || true
    printf '%s|%s|%s\n' "$service" "$account" "$password" >> "$tmp"
    mv "$tmp" "$db"
    ;;
  find-generic-password)
    account=""
    service=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    value="$(awk -F'|' -v svc="$service" -v acct="$account" '$1 == svc && $2 == acct {{ print $3; exit }}' "$db" 2>/dev/null)"
    if [ -z "$value" ]; then
      exit 44
    fi
    printf '%s\n' "$value"
    ;;
  delete-generic-password)
    account=""
    service=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a) account="$2"; shift 2 ;;
        -s) service="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    tmp="$db.tmp"
    touch "$db"
    grep -v "^$service|$account|" "$db" > "$tmp" || true
    mv "$tmp" "$db"
    ;;
  list-keychains)
    if [ "$#" -ge 2 ] && [ "$1" = "-d" ] && [ "$2" = "user" ]; then
      exit 0
    fi
    ;;
  create-keychain|unlock-keychain|set-keychain-settings|import|set-key-partition-list)
    ;;
  find-identity)
    printf '  1) 04B011F1ABF0F7B8DDF99CD8BC88D5366AC8CC4D "Imported Identity"\n'
    ;;
  *)
    echo "unexpected security command: $cmd" >&2
    exit 1
    ;;
esac
"#,
            db = db_path.display()
        ),
    );
}

pub fn create_watch_xcrun_mock(mock_bin: &Path, sdk_root: &Path) {
    create_xcrun_mock(mock_bin, sdk_root, XcrunMockKind::Watch);
}

pub fn create_build_xcrun_mock(mock_bin: &Path, sdk_root: &Path) {
    create_xcrun_mock(mock_bin, sdk_root, XcrunMockKind::Build);
}

pub fn create_quality_swift_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("swift"),
        r#"#!/bin/sh
set -eu
echo "swift $@" >> "$MOCK_LOG"
if [ "$#" -ge 4 ] && [ "$1" = "package" ] && [ "$2" = "--package-path" ] && [ "$4" = "dump-package" ]; then
  package_path="$3"
  if [ -f "$package_path/Sources/OrbitPkg/OrbitPkg.swift" ]; then
    cat <<'JSON'
{"name":"OrbitPkg","products":[{"name":"OrbitPkg","targets":["OrbitPkg"]}],"targets":[{"name":"OrbitPkg","path":"Sources/OrbitPkg","dependencies":[],"type":"regular"}]}
JSON
    exit 0
  fi
  echo "unexpected package path: $package_path" >&2
  exit 1
fi
scratch=""
product=""
show_bin_path=0
prev=""
for arg in "$@"; do
  if [ "$prev" = "--scratch-path" ]; then
    scratch="$arg"
  fi
  if [ "$prev" = "--product" ]; then
    product="$arg"
  fi
  if [ "$arg" = "--show-bin-path" ]; then
    show_bin_path=1
  fi
  prev="$arg"
done
if [ -z "$scratch" ]; then
  echo "missing --scratch-path" >&2
  exit 1
fi
bin_dir="$scratch/release"
mkdir -p "$bin_dir"
if [ "$show_bin_path" -eq 1 ]; then
  printf '%s\n' "$bin_dir"
  exit 0
fi
case "$product" in
  orbit-swift-format|orbit-swiftlint)
    cat > "$bin_dir/$product" <<'SCRIPT'
#!/bin/sh
set -eu
echo "__PRODUCT__ $@" >> "$MOCK_LOG"
printf '%s\n' "__PRODUCT__ request:" >> "$MOCK_LOG"
cat "$1" >> "$MOCK_LOG"
printf '\n' >> "$MOCK_LOG"
SCRIPT
    sed -i '' "s#__PRODUCT__#$product#g" "$bin_dir/$product"
    chmod +x "$bin_dir/$product"
    exit 0
    ;;
  *)
    echo "unexpected swift product: $product" >&2
    exit 1
    ;;
esac
"#,
    );
}

pub fn create_testing_swift_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("swift"),
        r#"#!/bin/sh
set -eu
echo "swift $@" >> "$MOCK_LOG"
if [ "$#" -lt 1 ] || [ "$1" != "test" ]; then
  echo "unexpected swift command: $@" >&2
  exit 1
fi
package_path=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "--package-path" ]; then
    package_path="$arg"
  fi
  prev="$arg"
done
if [ -z "$package_path" ] || [ ! -f "$package_path/Package.swift" ]; then
  echo "missing generated Package.swift" >&2
  exit 1
fi
"#,
    );
}

pub fn create_idb_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("idb"),
        r#"#!/bin/sh
set -eu
echo "idb $@" >> "$MOCK_LOG"
cmd=""
if [ "$#" -ge 2 ] && [ "$1" = "ui" ]; then
  cmd="$2"
elif [ "$#" -ge 1 ]; then
  cmd="$1"
fi
case "$cmd" in
  describe-all)
    cat <<'JSON'
[
  {
    "AXLabel": "ExampleApp",
    "frame": { "x": 0, "y": 0, "width": 393, "height": 852 }
  },
  {
    "AXLabel": "Continue",
    "frame": { "x": 40, "y": 120, "width": 200, "height": 44 }
  },
  {
    "AXIdentifier": "email-value",
    "AXLabel": "qa@example.com",
    "frame": { "x": 40, "y": 180, "width": 200, "height": 44 }
  },
  {
    "AXLabel": "Welcome",
    "frame": { "x": 40, "y": 200, "width": 200, "height": 44 }
  }
]
JSON
    ;;
  describe-point)
    cat <<'JSON'
{
  "AXLabel": "Continue",
  "frame": { "x": 40, "y": 120, "width": 200, "height": 44 }
}
JSON
    ;;
  video|record-video)
    out="$2"
    mkdir -p "$(dirname "$out")"
    printf 'mp4' > "$out"
    ;;
  log)
    printf 'mock log line\n'
    ;;
  crash)
    sub="$2"
    case "$sub" in
      list)
        printf 'mock-crash-1.ips\n'
        ;;
      show)
        printf 'mock crash payload\n'
        ;;
      delete)
        ;;
      *)
        echo "unexpected idb crash command: $@" >&2
        exit 1
        ;;
    esac
    ;;
  contacts)
    if [ "$#" -ge 2 ] && [ "$2" = "update" ]; then
      :
    else
      echo "unexpected idb contacts command: $@" >&2
      exit 1
    fi
    ;;
  dylib)
    if [ "$#" -ge 2 ] && [ "$2" = "install" ]; then
      :
    else
      echo "unexpected idb dylib command: $@" >&2
      exit 1
    fi
    ;;
  instruments)
    printf 'mock instruments trace\n'
    ;;
  tap|text|swipe|clear-keychain|set-location|uninstall|approve|launch|focus|add-media|kill|open)
    ;;
  button|key|key-sequence)
    ;;
  *)
    echo "unexpected idb command: $@" >&2
    exit 1
    ;;
esac
"#,
    );
    write_executable(
        &mock_bin.join("idb_companion"),
        r#"#!/bin/sh
set -eu
echo "idb_companion $@" >> "$MOCK_LOG"
"#,
    );
}

pub fn create_ditto_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("ditto"),
        r#"#!/bin/sh
set -eu
echo "ditto $@" >> "$MOCK_LOG"
if [ "$#" -lt 2 ]; then
  echo "ditto mock expects at least source and destination" >&2
  exit 1
fi
src=""
out=""
prev=""
for arg in "$@"; do
  src="$prev"
  out="$arg"
  prev="$arg"
done
mkdir -p "$(dirname "$out")"
rm -f "$out"
src_parent="$(dirname "$src")"
src_name="$(basename "$src")"
(
  cd "$src_parent"
  /usr/bin/zip -qry "$out" "$src_name"
)
"#,
    );
}

pub fn create_submit_swinfo_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("swinfo"),
        r#"#!/bin/sh
set -eu
echo "swinfo $@" >> "$MOCK_LOG"
out=""
temp=""
spi=0
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then
    out="$arg"
  fi
  if [ "$prev" = "-temporary" ]; then
    temp="$arg"
  fi
  if [ "$prev" = "--output-spi" ]; then
    spi=1
  fi
  prev="$arg"
done
if [ -n "$out" ]; then
  mkdir -p "$(dirname "$out")"
  printf 'plist' > "$out"
fi
if [ -n "$temp" ]; then
  mkdir -p "$temp"
fi
if [ "$spi" -eq 1 ] && [ -n "$temp" ]; then
  printf 'zip' > "$temp/DTAppAnalyzerExtractorOutput-MOCK.zip"
fi
"#,
    );
}

pub fn create_passthrough_mock(mock_bin: &Path, name: &str) {
    write_executable(
        &mock_bin.join(name),
        &format!(
            r#"#!/bin/sh
set -eu
echo "{name} $@" >> "$MOCK_LOG"
"#,
        ),
    );
}

enum XcrunMockKind {
    Build,
    Watch,
}

fn create_xcrun_mock(mock_bin: &Path, sdk_root: &Path, kind: XcrunMockKind) {
    let sdk_version_block = match kind {
        XcrunMockKind::Build => "  printf '%s\\n' \"18.0\"\n  exit 0",
        XcrunMockKind::Watch => {
            "  case \"$2\" in\n    watchos|watchsimulator) printf '%s\\n' \"11.0\" ;;\n    *) printf '%s\\n' \"18.0\" ;;\n  esac\n  exit 0"
        }
    };
    let extra_commands = match kind {
        XcrunMockKind::Build => {
            r#"if [ "$1" = "altool" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "list" ] && [ "$3" = "devices" ]; then
  cat <<'JSON'
{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-18-0":[{"udid":"IOS-UDID","name":"iPhone 16","state":"Booted"}]}}
JSON
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "boot" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "bootstatus" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "install" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "launch" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "terminate" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "openurl" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "privacy" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "location" ] && [ "$4" = "start" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "io" ] && [ "$4" = "screenshot" ]; then
  mkdir -p "$(dirname "$5")"
  printf 'png' > "$5"
  exit 0
fi"#
        }
        XcrunMockKind::Watch => {
            r#"if [ "$1" = "simctl" ] && [ "$2" = "list" ] && [ "$3" = "devices" ]; then
  cat <<'JSON'
{"devices":{"com.apple.CoreSimulator.SimRuntime.watchOS-11-0":[{"udid":"WATCH-UDID","name":"Apple Watch Series 9","state":"Shutdown"}]}}
JSON
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "boot" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "bootstatus" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "install" ]; then
  exit 0
fi
if [ "$1" = "simctl" ] && [ "$2" = "launch" ]; then
  exit 0
fi"#
        }
    };
    write_executable(
        &mock_bin.join("xcrun"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "xcrun $@" >> "$MOCK_LOG"
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-path" ]; then
  mkdir -p "{sdk}"
  printf '%s\n' "{sdk}"
  exit 0
fi
if [ "$#" -ge 2 ] && [ "$1" = "--find" ] && [ "$2" = "swiftc" ]; then
  printf '%s\n' "{sdk}/Toolchains/OrbitDefault.xctoolchain/usr/bin/swiftc"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-version" ]; then
{sdk_version_block}
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-build-version" ]; then
  printf '%s\n' "TESTSDK1"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "swiftc" ]; then
  out=""
  module=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
      out="$arg"
    fi
    if [ "$prev" = "-emit-module-path" ]; then
      module="$arg"
    fi
    prev="$arg"
  done
  mkdir -p "$(dirname "$out")"
  : > "$out"
  if [ -n "$module" ]; then
    mkdir -p "$(dirname "$module")"
    : > "$module"
  fi
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && {{ [ "$3" = "clang" ] || [ "$3" = "clang++" ]; }}; then
  out=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
      out="$arg"
    fi
    prev="$arg"
  done
  if [ -n "$out" ]; then
    mkdir -p "$(dirname "$out")"
    : > "$out"
  fi
  exit 0
fi
if [ "$#" -ge 1 ] && [ "$1" = "actool" ]; then
  compile_dir=""
  partial=""
  app_icon=0
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--compile" ]; then
      compile_dir="$arg"
    fi
    if [ "$prev" = "--output-partial-info-plist" ]; then
      partial="$arg"
    fi
    if [ "$prev" = "--app-icon" ]; then
      app_icon=1
    fi
    prev="$arg"
  done
  mkdir -p "$compile_dir"
  : > "$compile_dir/Assets.car"
  if [ "$app_icon" -eq 1 ]; then
    : > "$compile_dir/AppIcon60x60@2x.png"
    : > "$compile_dir/AppIcon76x76@2x~ipad.png"
    cat > "$partial" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIcons</key>
  <dict>
    <key>CFBundlePrimaryIcon</key>
    <dict>
      <key>CFBundleIconFiles</key>
      <array>
        <string>AppIcon60x60</string>
      </array>
      <key>CFBundleIconName</key>
      <string>AppIcon</string>
    </dict>
  </dict>
  <key>CFBundleIcons~ipad</key>
  <dict>
    <key>CFBundlePrimaryIcon</key>
    <dict>
      <key>CFBundleIconFiles</key>
      <array>
        <string>AppIcon60x60</string>
        <string>AppIcon76x76</string>
      </array>
      <key>CFBundleIconName</key>
      <string>AppIcon</string>
    </dict>
  </dict>
</dict>
</plist>
PLIST
  else
    cat > "$partial" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict/>
</plist>
PLIST
  fi
  exit 0
fi
{extra_commands}
echo "unexpected xcrun command: $@" >&2
exit 1
"#,
            sdk = sdk_root.display(),
            sdk_version_block = sdk_version_block,
            extra_commands = extra_commands,
        ),
    );
}
