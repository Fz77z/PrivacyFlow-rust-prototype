#!/bin/bash
# Assemble "PrivacyFlow Prototype.app" around the release binary and install it.
#
# The name and bundle identifier differ from the Swift PrivacyFlow app, so this
# prototype installs beside that app and keeps its own permission grants
# instead of replacing it.
#
# PrivacyFlow is installed to a stable path on purpose. macOS grants
# Accessibility, Input Monitoring and Microphone per executable identity, so
# a binary living inside target/ is re-prompted every time it is rebuilt.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STAGE="$ROOT/target/PrivacyFlow Prototype.app"
INSTALL="/Applications/PrivacyFlow Prototype.app"
VERSION="$(awk -F'"' '/^version = /{print $2; exit}' "$ROOT/Cargo.toml")"

# awk exits 0 when it matches nothing, so an empty VERSION would sail through
# and sed would write <string></string> for both version keys, which is a
# malformed bundle. The same guard rejects a version containing / or &, which
# would be swallowed by the sed substitution below rather than substituted.
if [[ ! "$VERSION" =~ ^[0-9A-Za-z.+-]+$ ]]; then
    echo "Refusing to build: no usable version read from $ROOT/Cargo.toml (got '$VERSION')" >&2
    exit 1
fi

echo "Building PrivacyFlow $VERSION"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

# Assemble from scratch every time. Merging into an existing bundle can leave
# a stale executable behind, which is exactly the confusion this avoids.
rm -rf "$STAGE"
mkdir -p "$STAGE/Contents/MacOS" "$STAGE/Contents/Resources"

sed "s/VERSION_PLACEHOLDER/$VERSION/g" "$ROOT/bundle/Info.plist" \
    > "$STAGE/Contents/Info.plist"
cp "$ROOT/target/release/privacyflow" "$STAGE/Contents/MacOS/privacyflow"

ICONSET="$ROOT/target/AppIcon.iconset"
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
# Rendering the icon takes about ten seconds and is deterministic, so it is
# regenerated only when the generator is newer than its output. Delete
# target/AppIcon.png to force it.
if [[ ! -f "$ROOT/target/AppIcon.png" || "$ROOT/bundle/make-icon.py" -nt "$ROOT/target/AppIcon.png" ]]; then
    echo "Rendering the app icon"
    python3 "$ROOT/bundle/make-icon.py" "$ROOT/target/AppIcon.png"
fi
for size in 16 32 128 256 512; do
    sips -z $size $size "$ROOT/target/AppIcon.png" \
        --out "$ICONSET/icon_${size}x${size}.png" > /dev/null
    sips -z $((size * 2)) $((size * 2)) "$ROOT/target/AppIcon.png" \
        --out "$ICONSET/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil -c icns "$ICONSET" -o "$STAGE/Contents/Resources/AppIcon.icns"

# Signed with a stable self-signed identity rather than ad hoc. An ad-hoc
# signature has no identity, so macOS falls back to identifying the app by a
# hash of its binary, and every rebuild becomes a different application whose
# Accessibility, Input Monitoring and Microphone grants have to be given
# again. This is not about trust or distribution; it is about this machine
# recognising successive builds as the same program.
SIGNING_IDENTITY="PrivacyFlow Self Signed"
if ! security find-identity -p codesigning | grep -q "$SIGNING_IDENTITY"; then
    echo "No \"$SIGNING_IDENTITY\" code-signing identity found." >&2
    echo "Run ./scripts/make-signing-cert.sh once to create it." >&2
    echo "Signing ad hoc instead would silently cost you your permission" >&2
    echo "grants on every rebuild, so this stops here rather than doing it." >&2
    exit 1
fi
codesign --force --sign "$SIGNING_IDENTITY" "$STAGE"

# Copy beside the install first, so a failed copy leaves the existing app in
# place instead of removing it and putting nothing back. The remaining window
# is the rm and the mv, which is as close to atomic as a directory swap gets
# without a replacement API.
rm -rf "$INSTALL.new"
cp -R "$STAGE" "$INSTALL.new"
rm -rf "$INSTALL"
mv "$INSTALL.new" "$INSTALL"

# Verify what was actually installed, not what was staged. Until this ran here
# it was a manual step done once, so every rebuild since shipped unverified.
codesign --verify --strict "$INSTALL"
IDENTIFIER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$INSTALL/Contents/Info.plist")"
UI_ELEMENT="$(/usr/libexec/PlistBuddy -c 'Print :LSUIElement' "$INSTALL/Contents/Info.plist")"
if [[ -z "$IDENTIFIER" || "$UI_ELEMENT" != "true" ]]; then
    # set -e does not fire on a command substitution used as an argument, so a
    # broken plist would otherwise print an empty value and exit 0.
    echo "Installed bundle has a bad Info.plist: identifier '$IDENTIFIER', LSUIElement '$UI_ELEMENT'" >&2
    exit 1
fi

echo "Installed $INSTALL"
echo "Identifier: $IDENTIFIER"
