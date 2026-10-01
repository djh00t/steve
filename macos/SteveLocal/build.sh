#!/bin/bash
set -euo pipefail
if [[ $# != 3 ]]; then
    echo 'Usage: build.sh /absolute/daemon /absolute/private/native.toml /absolute/output/Steve.app' >&2
    exit 2
fi
source_dir="$(cd "$(dirname "$0")" && pwd)"
daemon="$1"
config="$2"
destination="$3"
[[ "$daemon" = /* && "$config" = /* && "$destination" = /* ]]
[[ -x "$daemon" && -f "$config" ]]
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/steve-app-build.XXXXXX")"
install_dir=""
backup=""
cleanup() {
    if [[ -n "$backup" && -e "$backup" && ! -e "$destination" ]]; then
        mv "$backup" "$destination"
    fi
    rm -rf "$build_dir"
    [[ -z "$install_dir" ]] || rm -rf "$install_dir"
}
trap cleanup EXIT
bundle="$build_dir/Steve.app"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
module_cache="$build_dir/cache"
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
# Seal on native temporary storage; managed folders can add signing-invalid metadata.
codesign --force --sign - "$bundle"
codesign --verify --deep --strict "$bundle"
"$bundle/Contents/MacOS/Steve" --self-test
if [[ -e "$destination" ]]; then
    python3 - "$destination/Contents/Info.plist" <<'PY'
import plistlib, sys
with open(sys.argv[1], 'rb') as existing:
    if plistlib.load(existing).get('CFBundleIdentifier') != 'com.djh00t.steve.local':
        sys.exit('Refusing to overwrite a different app')
PY
fi
mkdir -p "$(dirname "$destination")"
install_dir="$(mktemp -d "$(dirname "$destination")/.steve-install.XXXXXX")"
ditto "$bundle" "$install_dir/Steve.app"
codesign --verify --deep --strict "$install_dir/Steve.app"
if [[ -e "$destination" ]]; then
    backup="$install_dir/previous.app"
    mv "$destination" "$backup"
fi
mv "$install_dir/Steve.app" "$destination"
echo "$destination"
