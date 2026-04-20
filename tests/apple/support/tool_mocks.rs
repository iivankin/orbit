use std::path::Path;

use super::write_executable;

const IDB_MOCK_SCRIPT: &str = r#"#!/bin/sh
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
"#;

const IDB_COMPANION_MOCK_SCRIPT: &str = r#"#!/bin/sh
set -eu
echo "idb_companion $@" >> "$MOCK_LOG"
"#;

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
      printf '"%s/Library/Keychains/login.keychain-db"\n' "$HOME"
      exit 0
    fi
    ;;
  create-keychain|unlock-keychain|set-keychain-settings|set-key-partition-list)
    ;;
  import)
    p12=""
    keychain=""
    password=""
    cert_path=""
    cert_format=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -k) keychain="$2"; shift 2 ;;
        -P) password="$2"; shift 2 ;;
        -T) shift 2 ;;
        *)
          if [ -z "$p12" ]; then
            p12="$1"
          fi
          shift
          ;;
      esac
    done
    mkdir -p "$(dirname "$db")"
    touch "$db"
    der_path="${{p12%.*}}.cer"
    pem_path="${{p12%.*}}.pem"
    if [ -f "$der_path" ]; then
      cert_path="$der_path"
      cert_format="DER"
    elif [ -f "$pem_path" ]; then
      cert_path="$pem_path"
      cert_format="PEM"
    fi
    if [ -n "$cert_path" ]; then
      if [ "$cert_format" = "DER" ]; then
        hash="$(openssl x509 -inform DER -in "$cert_path" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
        name="$(openssl x509 -inform DER -in "$cert_path" -noout -subject | sed -E 's/^subject= *//; s/.*CN *= *//')"
      else
        hash="$(openssl x509 -in "$cert_path" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
        name="$(openssl x509 -in "$cert_path" -noout -subject | sed -E 's/^subject= *//; s/.*CN *= *//')"
      fi
    else
      cert_tmp="$(mktemp)"
      if openssl pkcs12 -in "$p12" -clcerts -nokeys -passin "pass:$password" -out "$cert_tmp" >/dev/null 2>&1; then
        hash="$(openssl x509 -in "$cert_tmp" -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
        name="$(openssl x509 -in "$cert_tmp" -noout -subject | sed -E 's/^subject= *//; s/.*CN *= *//')"
        cert_path="$(dirname "$db")/$hash.pem"
        cp "$cert_tmp" "$cert_path"
        cert_format="PEM"
        rm -f "$cert_tmp"
      else
        rm -f "$cert_tmp"
        hash="$(printf '%s' "$p12" | shasum | awk '{{print toupper(substr($1, 1, 40))}}')"
        name="Imported Identity"
      fi
    fi
    tmp="$db.tmp"
    grep -v "^import|$keychain|$hash|" "$db" > "$tmp" || true
    printf '%s|%s|%s|%s|%s|%s|%s|%s\n' "import" "$keychain" "$hash" "$name" "$cert_path" "$cert_format" "$p12" "$password" >> "$tmp"
    mv "$tmp" "$db"
    ;;
  find-identity)
    keychain=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -p|-s) shift 2 ;;
        -v) shift ;;
        *)
          keychain="$1"
          shift
          ;;
      esac
    done
    touch "$db"
    awk -F'|' -v kc="$keychain" '
      $1 == "import" && (kc == "" || $2 == kc) {{
        count += 1
        printf "  %d) %s \"%s\"\n", count, $3, $4
      }}
    ' "$db"
    ;;
  find-certificate)
    keychain=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a|-Z|-p) shift ;;
        *)
          keychain="$1"
          shift
          ;;
      esac
    done
    touch "$db"
    awk -F'|' -v kc="$keychain" '
      $1 == "import" && (kc == "" || $2 == kc) {{
        printf "%s|%s|%s|%s|%s\n", $3, $5, $6, $7, $8
      }}
    ' "$db" | while IFS='|' read -r hash cert_path cert_format p12 password; do
      [ -n "$hash" ] || continue
      printf 'SHA-1 hash: %s\n' "$hash"
      if [ -n "$cert_path" ] && [ -f "$cert_path" ]; then
        if [ "$cert_format" = "DER" ]; then
          openssl x509 -inform DER -in "$cert_path" -outform PEM 2>/dev/null
        else
          openssl x509 -in "$cert_path" -outform PEM 2>/dev/null
        fi
      elif [ -n "$p12" ] && [ -f "$p12" ]; then
        openssl pkcs12 -in "$p12" -clcerts -nokeys -passin "pass:$password" 2>/dev/null
      fi
    done
    exit 0
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

pub fn create_lldb_attach_mock(developer_dir: &Path) {
    let bin_dir = developer_dir.join("usr").join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("lldb"),
        r#"#!/bin/sh
set -eu
echo "lldb $@" >> "$MOCK_LOG"
printf '(lldb) '
while IFS= read -r line; do
  echo "$line" >> "$MOCK_LOG"
  case "$line" in
    "process attach -i -w -n "*)
      printf 'Process 123 stopped\n'
      printf '(lldb) '
      ;;
    "process continue")
      printf 'Process 123 resuming\n'
      printf '(lldb) '
      exit 0
      ;;
    *)
      printf '(lldb) '
      ;;
  esac
done
"#,
    );
}

pub fn create_xcodebuild_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("xcodebuild"),
        r#"#!/bin/sh
set -eu
echo "xcodebuild $@" >> "$MOCK_LOG"
if [ "$#" -eq 1 ] && [ "$1" = "-version" ]; then
  printf '%s\n' "Xcode 16.0"
  printf '%s\n' "Build version 16A242d"
  exit 0
fi
echo "unexpected xcodebuild command: $@" >&2
exit 1
"#,
    );
}

pub fn create_sw_vers_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("sw_vers"),
        r#"#!/bin/sh
set -eu
echo "sw_vers $@" >> "$MOCK_LOG"
if [ "$#" -ne 1 ]; then
  echo "unexpected sw_vers command: $@" >&2
  exit 1
fi
case "$1" in
  -productVersion)
    printf '%s\n' "15.0"
    ;;
  -buildVersion)
    printf '%s\n' "24A335"
    ;;
  *)
    echo "unexpected sw_vers command: $@" >&2
    exit 1
    ;;
esac
"#,
    );
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
  if [ -f "$package_path/Sources/OrbiPkg/OrbiPkg.swift" ]; then
    cat <<'JSON'
{"name":"OrbiPkg","products":[{"name":"OrbiPkg","targets":["OrbiPkg"]}],"targets":[{"name":"OrbiPkg","path":"Sources/OrbiPkg","dependencies":[],"type":"regular"}]}
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
  orbi-swift-format|orbi-swiftlint)
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
    write_executable(&mock_bin.join("idb"), IDB_MOCK_SCRIPT);
    write_executable(&mock_bin.join("idb_companion"), IDB_COMPANION_MOCK_SCRIPT);
}

pub fn create_python3_fb_idb_install_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("python3"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "python3 $@" >> "$MOCK_LOG"
if [ "$#" -ge 6 ] && [ "$1" = "-m" ] && [ "$2" = "pip" ] && [ "$3" = "install" ]; then
  case " $* " in
    *" fb-idb==1.1.7 "*)
      bin_dir="$HOME/Library/Python/3.12/bin"
      mkdir -p "$bin_dir"
      cat > "$bin_dir/idb" <<'EOF'
{idb_script}
EOF
      chmod +x "$bin_dir/idb"
      exit 0
      ;;
  esac
fi
echo "unexpected python3 command: $@" >&2
exit 1
"#,
            idb_script = IDB_MOCK_SCRIPT
        ),
    );
}

pub fn create_brew_idb_companion_install_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("brew"),
        &format!(
            r#"#!/bin/sh
set -eu
echo "brew $@" >> "$MOCK_LOG"
prefix="$HOME/.orbi-test-brew/idb-companion"
cmd="${{1:-}}"
case "$cmd" in
  tap)
    exit 0
    ;;
  install)
    if [ "$#" -eq 2 ] && [ "$2" = "idb-companion" ]; then
      mkdir -p "$prefix/bin"
      cat > "$prefix/bin/idb_companion" <<'EOF'
{companion_script}
EOF
      chmod +x "$prefix/bin/idb_companion"
      exit 0
    fi
    ;;
  --prefix)
    if [ "$#" -eq 2 ] && [ "$2" = "idb-companion" ] && [ -x "$prefix/bin/idb_companion" ]; then
      printf '%s\n' "$prefix"
      exit 0
    fi
    exit 1
    ;;
esac
echo "unexpected brew command: $@" >&2
exit 1
"#,
            companion_script = IDB_COMPANION_MOCK_SCRIPT
        ),
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

pub fn create_codesign_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("codesign"),
        r#"#!/bin/sh
set -eu
echo "codesign $@" >> "$MOCK_LOG"
if [ "$#" -lt 1 ]; then
  echo "codesign mock expects a bundle path" >&2
  exit 1
fi
bundle=""
verify=0
for arg in "$@"; do
  case "$arg" in
    -dv|--display|--verbose=*)
      verify=1
      ;;
  esac
  bundle="$arg"
done
if [ "$verify" -eq 1 ]; then
  if [ -d "$bundle" ]; then
    printf 'Executable=%s/Contents/MacOS/ExampleApp\n' "$bundle" >&2
    printf 'flags=0x10000(runtime)\n' >&2
  fi
  printf 'Authority=Developer ID Application: Example Team\n' >&2
  exit 0
fi
if [ -d "$bundle/Contents" ]; then
  signature_root="$bundle/Contents/_CodeSignature"
elif [ -d "$bundle" ]; then
  signature_root="$bundle/_CodeSignature"
else
  mkdir -p "$(dirname "$bundle")"
  printf 'signed\n' > "$bundle.signature"
  exit 0
fi
mkdir -p "$signature_root"
printf 'signed\n' > "$signature_root/CodeResources"
"#,
    );
}

pub fn create_hdiutil_mock(mock_bin: &Path) {
    write_executable(
        &mock_bin.join("hdiutil"),
        r#"#!/bin/sh
set -eu
echo "hdiutil $@" >> "$MOCK_LOG"
if [ "$#" -lt 1 ]; then
  echo "hdiutil mock expects an output path" >&2
  exit 1
fi
out=""
for arg in "$@"; do
  out="$arg"
done
mkdir -p "$(dirname "$out")"
printf 'dmg' > "$out"
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
if [ "$1" = "simctl" ] && [ "$2" = "spawn" ] && [ "$4" = "log" ] && [ "$5" = "stream" ]; then
  process_name=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--process" ]; then
      process_name="$arg"
    fi
    prev="$arg"
  done
  printf '%s\n' "Filtering the log data using \"process == $process_name\""
  printf '2026-04-02 12:00:00.000000+0000 %s[123:456] mock log line\n' "$process_name"
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
  printf '%s\n' "{sdk}/Toolchains/OrbiDefault.xctoolchain/usr/bin/swiftc"
  exit 0
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-version" ]; then
{sdk_version_block}
fi
if [ "$#" -ge 3 ] && [ "$1" = "--sdk" ] && [ "$3" = "--show-sdk-build-version" ]; then
  printf '%s\n' "TESTSDK1"
  exit 0
fi
if [ "$#" -ge 2 ] && [ "$1" = "xctrace" ] && [ "$2" = "record" ]; then
  shift 2
  output=""
  template=""
  attach=""
  launch=0
  time_limit=""
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --output)
        output="$2"
        shift 2
        ;;
      --template)
        template="$2"
        shift 2
        ;;
      --attach)
        attach="$2"
        shift 2
        ;;
      --time-limit)
        time_limit="$2"
        shift 2
        ;;
      --device|--env)
        shift 2
        ;;
      --no-prompt)
        shift
        ;;
      --launch)
        launch=1
        shift
        if [ "$#" -gt 0 ] && [ "$1" = "--" ]; then
          shift
        fi
        break
        ;;
      *)
        shift
        ;;
    esac
  done
  if [ -z "$output" ] || [ -z "$template" ]; then
    echo "xctrace record mock requires --output and --template" >&2
    exit 1
  fi
  if [ -z "$attach" ] && [ "$launch" -eq 0 ]; then
    echo "xctrace record mock requires a target mode" >&2
    exit 1
  fi
  mkdir -p "$output"
  printf '%s\n' "$template" > "$output/template.txt"
  if [ -n "$time_limit" ]; then
    exit 0
  fi
  if [ "$launch" -eq 1 ] && [ "$#" -gt 0 ]; then
    if command -v "$1" >/dev/null 2>&1; then
      "$@"
      exit $?
    fi
    if [ -x "$1" ]; then
      "$@"
      exit $?
    fi
  fi
  trap 'exit 0' INT TERM
  while :; do
    sleep 1
  done
  exit 0
fi
if [ "$#" -ge 2 ] && [ "$1" = "xctrace" ] && [ "$2" = "export" ]; then
  if [ -n "${{MOCK_XCTRACE_EXPORT_FAIL_COUNT_FILE:-}}" ] && [ -f "$MOCK_XCTRACE_EXPORT_FAIL_COUNT_FILE" ]; then
    remaining="$(cat "$MOCK_XCTRACE_EXPORT_FAIL_COUNT_FILE" 2>/dev/null || printf '0')"
    case "$remaining" in
      ''|*[!0-9]*)
        remaining=0
        ;;
    esac
    if [ "$remaining" -gt 0 ]; then
      printf '%s\n' "$((remaining - 1))" > "$MOCK_XCTRACE_EXPORT_FAIL_COUNT_FILE"
      echo "Export failed: Document Missing Template Error" >&2
      exit 10
    fi
  fi
  if [ "${{MOCK_XCTRACE_EXPORT_FAIL:-0}}" = "1" ]; then
    echo "Export failed: Document Missing Template Error" >&2
    exit 10
  fi
  mode=""
  output=""
  input=""
  xpath=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "--output" ]; then
      output="$arg"
    fi
    if [ "$prev" = "--input" ]; then
      input="$arg"
    fi
    if [ "$arg" = "--toc" ]; then
      mode="toc"
    fi
    if [ "$prev" = "--xpath" ]; then
      mode="xpath"
      xpath="$arg"
    fi
    if [ "$arg" = "--har" ]; then
      mode="har"
    fi
    prev="$arg"
  done
  if [ "$mode" = "toc" ]; then
    case "$input" in
      *allocations.trace*)
      if [ -n "$output" ]; then
        mkdir -p "$(dirname "$output")"
        cat > "$output" <<'XML'
<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" model="MacBook Pro" name="Example Mac" os-version="26.4 (25E246)" uuid="DEVICE-UUID"/>
        <process type="attached" return-exit-status="0" name="Orbi" pid="123" termination-reason="exit(0)"/>
      </target>
      <summary>
        <duration>5.0</duration>
        <template-name>Allocations</template-name>
      </summary>
    </info>
    <processes>
      <process name="Orbi" pid="123" path="/Applications/Orbi.app/Contents/MacOS/Orbi"/>
    </processes>
    <tracks>
      <track name="Allocations">
        <details>
          <detail name="Statistics" kind="table"/>
          <detail name="Allocations List" kind="table"/>
        </details>
      </track>
      <track name="VM Tracker">
        <details>
          <detail name="Regions Map" kind="table"/>
        </details>
      </track>
    </tracks>
  </run>
</trace-toc>
XML
      else
        cat <<'XML'
<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" model="MacBook Pro" name="Example Mac" os-version="26.4 (25E246)" uuid="DEVICE-UUID"/>
        <process type="attached" return-exit-status="0" name="Orbi" pid="123" termination-reason="exit(0)"/>
      </target>
      <summary>
        <duration>5.0</duration>
        <template-name>Allocations</template-name>
      </summary>
    </info>
    <processes>
      <process name="Orbi" pid="123" path="/Applications/Orbi.app/Contents/MacOS/Orbi"/>
    </processes>
    <tracks>
      <track name="Allocations">
        <details>
          <detail name="Statistics" kind="table"/>
          <detail name="Allocations List" kind="table"/>
        </details>
      </track>
      <track name="VM Tracker">
        <details>
          <detail name="Regions Map" kind="table"/>
        </details>
      </track>
    </tracks>
  </run>
</trace-toc>
XML
      fi
      exit 0
      ;;
    esac
    if [ -n "$output" ]; then
      mkdir -p "$(dirname "$output")"
      cat > "$output" <<'XML'
<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" model="MacBook Pro" name="Example Mac" os-version="26.4 (25E246)" uuid="DEVICE-UUID"/>
        <process type="launched" return-exit-status="0" name="Orbi" pid="123" termination-reason="exit(0)"/>
      </target>
      <summary>
        <start-date>2026-04-03T04:18:08.145+03:00</start-date>
        <end-date>2026-04-03T04:18:10.083+03:00</end-date>
        <duration>1.938214</duration>
        <end-reason>Time limit reached</end-reason>
        <instruments-version>16.0 (17E192)</instruments-version>
        <template-name>Time Profiler</template-name>
        <recording-mode>Deferred</recording-mode>
        <time-limit>1 second</time-limit>
      </summary>
    </info>
    <processes>
      <process name="Orbi" pid="123" path="/Applications/Orbi.app/Contents/MacOS/Orbi"/>
      <process name="xctrace" pid="456" path="/Applications/Xcode.app/Contents/Developer/usr/bin/xctrace"/>
    </processes>
    <data>
      <table schema="tick" frequency="10"/>
      <table schema="time-profile" target-pid="ALL"/>
      <table schema="time-profile" target-pid="123"/>
    </data>
    <tracks/>
  </run>
</trace-toc>
XML
    else
      cat <<'XML'
<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" model="MacBook Pro" name="Example Mac" os-version="26.4 (25E246)" uuid="DEVICE-UUID"/>
        <process type="launched" return-exit-status="0" name="Orbi" pid="123" termination-reason="exit(0)"/>
      </target>
      <summary>
        <start-date>2026-04-03T04:18:08.145+03:00</start-date>
        <end-date>2026-04-03T04:18:10.083+03:00</end-date>
        <duration>1.938214</duration>
        <end-reason>Time limit reached</end-reason>
        <instruments-version>16.0 (17E192)</instruments-version>
        <template-name>Time Profiler</template-name>
        <recording-mode>Deferred</recording-mode>
        <time-limit>1 second</time-limit>
      </summary>
    </info>
    <processes>
      <process name="Orbi" pid="123" path="/Applications/Orbi.app/Contents/MacOS/Orbi"/>
      <process name="xctrace" pid="456" path="/Applications/Xcode.app/Contents/Developer/usr/bin/xctrace"/>
    </processes>
    <data>
      <table schema="tick" frequency="10"/>
      <table schema="time-profile" target-pid="ALL"/>
      <table schema="time-profile" target-pid="123"/>
    </data>
    <tracks/>
  </run>
</trace-toc>
XML
    fi
    exit 0
  fi
  if [ "$mode" = "xpath" ]; then
    case "$input" in
      *allocations.trace*)
      if [ "$xpath" = '/trace-toc/run/tracks/track[@name="Allocations"]/details/detail[@name="Statistics"]' ]; then
        cat <<'XML'
<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/tracks[1]/track[1]/details[1]/detail[1]'>
    <row category="All Heap &amp; Anonymous VM" persistent-bytes="33782272" count-persistent="1161" total-bytes="34183680" transient-bytes="401408" count-events="1183" count-transient="6" count-total="1167"/>
    <row category="All Heap Allocations" persistent-bytes="33782272" count-persistent="1161" total-bytes="33790464" transient-bytes="8192" count-events="1175" count-transient="2" count-total="1163"/>
    <row category="All Anonymous VM" persistent-bytes="0" count-persistent="0" total-bytes="393216" transient-bytes="393216" count-events="8" count-transient="4" count-total="4"/>
    <row category="Malloc 256.0 KiB" persistent-bytes="33554432" count-persistent="128" total-bytes="33554432" transient-bytes="0" count-events="128" count-transient="0" count-total="128"/>
    <row category="Malloc 48 Bytes" persistent-bytes="8208" count-persistent="171" total-bytes="8208" transient-bytes="0" count-events="171" count-transient="0" count-total="171"/>
    <row category="VM: Anonymous VM" persistent-bytes="0" count-persistent="0" total-bytes="393216" transient-bytes="393216" count-events="8" count-transient="4" count-total="4"/>
  </node>
</trace-query-result>
XML
        exit 0
      fi
      if [ "$xpath" = '/trace-toc/run/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]' ]; then
        cat <<'XML'
<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/tracks[1]/track[1]/details[1]/detail[2]'>
    <row address="0x10133c000" category="Malloc 256.0 KiB" live="true" responsible-caller="allocateChunk()" responsible-library="Orbi" size="262144"/>
    <row address="0x10137c000" category="Malloc 256.0 KiB" live="true" responsible-caller="allocateChunk()" responsible-library="Orbi" size="262144"/>
    <row address="0x10139c000" category="Malloc 48 Bytes" live="true" responsible-caller="bootstrap()" responsible-library="Orbi" size="48"/>
    <row address="0x10139c100" category="VM: Anonymous VM" live="false" responsible-caller="&lt;Call stack limit reached&gt;" responsible-library="" size="393216"/>
  </node>
</trace-query-result>
XML
        exit 0
      fi
      ;;
    esac
    if [ -n "$output" ]; then
      mkdir -p "$(dirname "$output")"
      cat > "$output" <<'XML'
<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/data[1]/table[2]'>
    <schema name="time-profile">
      <col><mnemonic>time</mnemonic></col>
      <col><mnemonic>thread</mnemonic></col>
      <col><mnemonic>process</mnemonic></col>
      <col><mnemonic>core</mnemonic></col>
      <col><mnemonic>thread-state</mnemonic></col>
      <col><mnemonic>weight</mnemonic></col>
      <col><mnemonic>stack</mnemonic></col>
    </schema>
    <row>
      <sample-time id="1" fmt="00:00.001.000">1000000</sample-time>
      <weight id="2" fmt="3.00 ms">3000000</weight>
      <tagged-backtrace id="3" fmt="heavyWork() ← main">
        <backtrace id="4">
          <frame id="5" name="heavyWork()" addr="0x102000100">
            <binary id="6" name="Orbi" path="/Applications/Orbi.app/Contents/MacOS/Orbi"/>
          </frame>
          <frame id="7" name="main" addr="0x102000050">
            <binary ref="6"/>
          </frame>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <sample-time id="8" fmt="00:00.002.000">2000000</sample-time>
      <weight ref="2"/>
      <tagged-backtrace id="9" fmt="sin ← heavyWork() ← main">
        <backtrace id="10">
          <frame id="11" name="sin" addr="0x180000100">
            <binary id="12" name="libsystem_m.dylib" path="/usr/lib/system/libsystem_m.dylib"/>
          </frame>
          <frame ref="5"/>
          <frame ref="7"/>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <sample-time id="13" fmt="00:00.003.000">3000000</sample-time>
      <weight id="14" fmt="1.00 ms">1000000</weight>
      <tagged-backtrace id="15" fmt="0x102000200 ← main">
        <backtrace id="16">
          <frame id="17" name="0x102000200" addr="0x102000200"/>
          <frame ref="7"/>
        </backtrace>
      </tagged-backtrace>
    </row>
  </node>
</trace-query-result>
XML
    else
      cat <<'XML'
<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/data[1]/table[2]'>
    <schema name="time-profile">
      <col><mnemonic>time</mnemonic></col>
      <col><mnemonic>thread</mnemonic></col>
      <col><mnemonic>process</mnemonic></col>
      <col><mnemonic>core</mnemonic></col>
      <col><mnemonic>thread-state</mnemonic></col>
      <col><mnemonic>weight</mnemonic></col>
      <col><mnemonic>stack</mnemonic></col>
    </schema>
    <row>
      <sample-time id="1" fmt="00:00.001.000">1000000</sample-time>
      <weight id="2" fmt="3.00 ms">3000000</weight>
      <tagged-backtrace id="3" fmt="heavyWork() ← main">
        <backtrace id="4">
          <frame id="5" name="heavyWork()" addr="0x102000100">
            <binary id="6" name="Orbi" path="/Applications/Orbi.app/Contents/MacOS/Orbi"/>
          </frame>
          <frame id="7" name="main" addr="0x102000050">
            <binary ref="6"/>
          </frame>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <sample-time id="8" fmt="00:00.002.000">2000000</sample-time>
      <weight ref="2"/>
      <tagged-backtrace id="9" fmt="sin ← heavyWork() ← main">
        <backtrace id="10">
          <frame id="11" name="sin" addr="0x180000100">
            <binary id="12" name="libsystem_m.dylib" path="/usr/lib/system/libsystem_m.dylib"/>
          </frame>
          <frame ref="5"/>
          <frame ref="7"/>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <sample-time id="13" fmt="00:00.003.000">3000000</sample-time>
      <weight id="14" fmt="1.00 ms">1000000</weight>
      <tagged-backtrace id="15" fmt="0x102000200 ← main">
        <backtrace id="16">
          <frame id="17" name="0x102000200" addr="0x102000200"/>
          <frame ref="7"/>
        </backtrace>
      </tagged-backtrace>
    </row>
  </node>
</trace-query-result>
XML
    fi
    exit 0
  fi
  if [ "$mode" = "har" ]; then
    if [ -n "$output" ]; then
      mkdir -p "$(dirname "$output")"
      cat > "$output" <<'JSON'
{{"log":{{"version":"1.2"}}}}
JSON
    else
      cat <<'JSON'
{{"log":{{"version":"1.2"}}}}
JSON
    fi
    exit 0
  fi
  echo "unexpected xctrace export command: $@" >&2
  exit 1
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
  depfile=""
  source=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-o" ]; then
      out="$arg"
    fi
    if [ "$prev" = "-MF" ]; then
      depfile="$arg"
    fi
    if [ "$prev" = "-c" ]; then
      source="$arg"
    fi
    prev="$arg"
  done
  if [ -n "$out" ]; then
    mkdir -p "$(dirname "$out")"
    : > "$out"
  fi
  if [ -n "$depfile" ] && [ -n "$out" ]; then
    mkdir -p "$(dirname "$depfile")"
    deps="$source"
    if [ -n "$source" ] && [ -f "$source" ]; then
      source_dir="$(dirname "$source")"
      for header in "$source_dir"/*.h "$source_dir"/*.hh "$source_dir"/*.hpp "$source_dir"/*.hxx; do
        if [ -f "$header" ]; then
          deps="$deps $header"
        fi
      done
    fi
    printf '%s: %s\n' "$out" "$deps" > "$depfile"
  fi
  exit 0
fi
if [ "$#" -ge 1 ] && [ "$1" = "lipo" ]; then
  out=""
  prev=""
  for arg in "$@"; do
    if [ "$prev" = "-output" ]; then
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
