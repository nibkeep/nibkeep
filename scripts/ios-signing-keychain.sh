#!/bin/sh
# Puts the certificate that signs the iOS test apps into a keychain of its own, which an SSH
# session can unlock without a person: over SSH, codesign cannot use the login keychain,
# and fails with errSecInternalComponent.
#
# Run it once on the Mac, in Terminal, in the account that signs, with the certificate and
# its key exported from Keychain Access as a .p12 file:
#
#     sh scripts/ios-signing-keychain.sh ~/Desktop/signing.p12
#
# It asks for the .p12 file's password. The keychain's own password is random, and stays
# in a file that only this account can read; scripts/test-on-device.py reads it on the Mac
# to unlock the keychain. The keychain holds nothing but the development certificate. It may
# be run again: a certificate already there is kept.
set -eu

p12="${1:?usage: sh scripts/ios-signing-keychain.sh EXPORTED.p12}"
keychain="$HOME/Library/Keychains/nibkeep-signing.keychain-db"
password_file="$HOME/.config/nibkeep/signing-keychain-password"

umask 077
mkdir -p "$(dirname "$password_file")"
[ -f "$password_file" ] || openssl rand -hex 32 > "$password_file"
password=$(cat "$password_file")

[ -f "$keychain" ] || security create-keychain -p "$password" "$keychain"
security unlock-keychain "$keychain" < "$password_file" > /dev/null
# Locks when the Mac sleeps, not after a while: each test run unlocks it.
security set-keychain-settings -l "$keychain"
# Run again, for example to put the search list right, the certificate is already there.
if ! failure=$(security import "$p12" -k "$keychain" -T /usr/bin/codesign 2>&1 > /dev/null); then
	case $failure in
	*"already exists"*) echo "The certificate is already in the keychain." ;;
	*) echo "$failure" >&2; exit 1 ;;
	esac
fi
# Lets codesign use the key without asking anyone.
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" > /dev/null
# First in the search list, so that codesign finds the key here rather than in the login
# keychain, which holds the same certificate.
others=$(security list-keychains -d user | tr -d '"' | grep -v nibkeep-signing || true)
# shellcheck disable=SC2086 # One keychain per line; their paths have no spaces.
security list-keychains -d user -s "$keychain" $others

security find-identity -v -p codesigning "$keychain"
echo "Done. Delete $p12 now: the keychain has what it held."
