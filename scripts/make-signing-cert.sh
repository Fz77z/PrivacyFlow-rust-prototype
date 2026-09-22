#!/bin/bash
# Create the stable code-signing identity PrivacyFlow is signed with.
#
# Why this exists: an ad-hoc signature has no identity, so macOS falls back
# to identifying the app by its cdhash, which is a hash of the binary. Every
# rebuild changes the binary, so macOS sees a different application and the
# Accessibility, Input Monitoring and Microphone grants do not carry over.
# In practice that means re-granting permissions after every single build.
#
# A self-signed certificate gives the signature a constant identity, so the
# grants survive rebuilds. It is not a Developer ID and does nothing for
# distributing the app to anyone else. It exists purely so this machine
# recognises successive builds as the same program.
#
# Run once. It is safe to re-run: it does nothing if the identity exists.
set -euo pipefail

COMMON_NAME="PrivacyFlow Self Signed"
KEYCHAIN="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$COMMON_NAME" "$KEYCHAIN" >/dev/null 2>&1; then
    echo "Signing identity \"$COMMON_NAME\" already exists. Nothing to do."
    exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "Generating a self-signed code-signing certificate"
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
    -subj "/CN=$COMMON_NAME" \
    -addext "basicConstraints=critical,CA:false" \
    -addext "keyUsage=critical,digitalSignature" \
    -addext "extendedKeyUsage=critical,codeSigning" \
    2>/dev/null

# macOS's importer rejects OpenSSL 3's default PKCS12 MAC, so the bundle is
# written with the legacy algorithms it understands. The passphrase is
# throwaway: the file exists for a few seconds inside a temporary directory
# that is deleted on exit.
openssl pkcs12 -export -legacy \
    -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 \
    -inkey "$WORK/key.pem" -in "$WORK/cert.pem" \
    -out "$WORK/identity.p12" -passout pass:privacyflow \
    2>/dev/null

echo "Importing it into your login keychain"
echo "macOS may ask for your login password. That is expected, and it is"
echo "asking so that codesign is allowed to use the new key without"
echo "prompting on every build."
security import "$WORK/identity.p12" -k "$KEYCHAIN" -P privacyflow \
    -T /usr/bin/codesign -T /usr/bin/security >/dev/null

# The identity is checked without -v on purpose. A self-signed certificate
# is in no trust chain, so it is never "valid" in that sense, and codesign
# does not care: it needs the private key, not a chain.
if ! security find-identity -p codesigning | grep -q "$COMMON_NAME"; then
    echo "The certificate imported but is not usable for code signing." >&2
    echo "Check Keychain Access for \"$COMMON_NAME\" under My Certificates." >&2
    exit 1
fi

echo
echo "Certificate created."
echo
echo "The first build will show a keychain dialog asking whether codesign"
echo "may use the new key. Choose Always Allow, not Allow: that records the"
echo "permission once, and later builds go through silently."
echo
echo "It is tempting to pre-authorise this with set-key-partition-list, but"
echo "matching the key by label does not work after a PKCS12 import, and"
echo "matching it by capability would rewrite the access list of every"
echo "signing key in your keychain. One dialog is the smaller price."
echo
echo "Then rebuild with ./scripts/build-app.sh and grant PrivacyFlow its"
echo "permissions one final time. They will survive rebuilds after that."
