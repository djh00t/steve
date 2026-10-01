#!/bin/bash
set -euo pipefail
if [[ $# != 3 ]]; then
    echo 'Usage: build.sh /absolute/daemon /absolute/private/native.toml /absolute/output/Steve.app' >&2
    exit 2
fi
source_dir="$(cd "$(dirname "$0")" && pwd)"
daemon="$1"
config="$2"
bundle="$3"
[[ "$daemon" = /* && "$config" = /* && "$bundle" = /* ]]
[[ -x "$daemon" && -f "$config" ]]
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
module_cache="$(mktemp -d "${TMPDIR:-/tmp}/steve-swift-cache.XXXXXX")"
trap 'rm -rf "$module_cache"' EXIT
xcrun swiftc -target "$(uname -m)-apple-macosx13.0" -module-cache-path "$module_cache" "$source_dir/main.swift" -o "$bundle/Contents/MacOS/Steve" -framework AppKit
cp "$daemon" "$bundle/Contents/Resources/steve"
python3 - "$config" "$bundle/Contents/Info.plist" <<'PY'
import plistlib, sys
with open(sys.argv[2], 'wb') as output:
    plistlib.dump({'CFBundleIdentifier':'com.djh00t.steve.local',
                  'CFBundleName':'Steve','CFBundleDisplayName':'Steve',
                  'CFBundleExecutable':'Steve','CFBundlePackageType':'APPL',
                  'CFBundleVersion':'1','CFBundleShortVersionString':'0.1.0',
                  'NSHighResolutionCapable':True,'SteveConfigPath':sys.argv[1],
                  'NSAppTransportSecurity':{'NSAllowsLocalNetworking':True}},output)
PY
"$bundle/Contents/MacOS/Steve" --self-test
echo "$bundle"
