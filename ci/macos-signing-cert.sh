#!/usr/bin/env bash
#
# macos-signing-cert.sh
#
# Creates and manages a local, self-signed code signing identity for
# TGZTerminal development builds.
#
# Why this exists: macOS records TCC (privacy) grants -- "allow access to your
# Documents folder", Full Disk Access, Accessibility, etc. -- against the
# signing identity of the app that asked. For an *ad-hoc* signed bundle
# (`codesign --sign -`) there is no identity, so TCC falls back to pinning the
# code directory hash of the binary. Every rebuild produces a new cdhash, which
# invalidates every previously granted permission, so the app re-prompts on the
# next launch. Upstream WezTerm never showed this because its official builds
# are signed with a real Developer ID and notarized.
#
# Signing with a stable identity -- a real Developer ID if you have one, or the
# self-signed certificate this script creates if you do not -- makes the TCC
# requirement "this bundle id, signed by this certificate", which survives
# rebuilds. Grants are then given once.
#
# Usage:
#   ci/macos-signing-cert.sh create      Create the certificate (idempotent).
#   ci/macos-signing-cert.sh identity    Print the identity name to sign with,
#                                        or exit 1 if none exists. Used by
#                                        ci/build-macos-bundle.sh.
#   ci/macos-signing-cert.sh status      Show what is available.
#   ci/macos-signing-cert.sh reset-tcc   Forget all privacy grants for the app,
#                                        so the next launch re-prompts once.
#   ci/macos-signing-cert.sh delete      Remove the self-signed certificate.
#
# Environment:
#   TGZ_SIGN_CERT_NAME    Common name of the self-signed certificate.
#                         Default: "TGZTerminal Local Signing".
#   TGZ_SIGN_KEYCHAIN     Keychain to use. Default: the login keychain.
#   TGZ_KEYCHAIN_PASSWORD Login keychain password. Optional. If set, the
#                         private key's partition list is updated so codesign
#                         never shows a "wants to use key" dialog.
#   BRAND_BUNDLE_ID       Bundle id used by reset-tcc.
#                         Default: com.tgzterminal.app
#
set -euo pipefail

CERT_CN=${TGZ_SIGN_CERT_NAME:-TGZTerminal Local Signing}
KEYCHAIN=${TGZ_SIGN_KEYCHAIN:-$HOME/Library/Keychains/login.keychain-db}
BUNDLE_ID=${BRAND_BUNDLE_ID:-com.tgzterminal.app}

log() {
  echo "==> $*"
}

fail() {
  echo "error: $*" >&2
  exit 1
}

# Prints every valid codesigning identity name in the keychain, one per line.
list_identities() {
  security find-identity -v -p codesigning "$KEYCHAIN" 2>/dev/null |
    sed -n 's/^[[:space:]]*[0-9]*)[[:space:]]*[0-9A-F]*[[:space:]]*"\(.*\)"$/\1/p'
}

self_signed_exists() {
  list_identities | grep -qxF "$CERT_CN"
}

# True once the certificate is in the keychain, whether or not it is trusted
# for code signing yet. `create` is interrupted mid-way if the user dismisses
# the trust-settings password dialog, so this lets a rerun resume instead of
# importing a second copy.
self_signed_imported() {
  security find-certificate -c "$CERT_CN" "$KEYCHAIN" >/dev/null 2>&1
}

# Prefer a real Developer ID over the local self-signed certificate: it is
# stable *and* accepted by Gatekeeper without a per-machine trust exception.
developer_id() {
  list_identities | grep -m1 '^Developer ID Application' || true
}

# codesign only accepts an identity whose certificate is trusted for code
# signing. Trusting it in the *login* keychain keeps this per-user and needs no
# sudo, but macOS shows a password dialog. That dialog cannot appear when this
# script runs without a session that can display it, so say what to do instead
# of failing opaquely.
trust_cert() {
  log "Trusting the certificate for code signing (macOS will ask for your password)"
  if ! security add-trusted-cert -r trustRoot -p codeSign -k "$KEYCHAIN" "$1"; then
    echo >&2
    echo "The trust-settings dialog was dismissed or could not be shown." >&2
    echo "Run this again from a normal Terminal session and enter your" >&2
    echo "login password when asked:" >&2
    echo >&2
    echo "    ci/macos-signing-cert.sh create" >&2
    echo >&2
    fail "security add-trusted-cert failed"
  fi
}

allow_key_use() {
  if [[ -n "${TGZ_KEYCHAIN_PASSWORD:-}" ]]; then
    log "Updating key partition list so codesign runs without prompting"
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
      -k "$TGZ_KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null 2>&1 ||
      echo "warning: set-key-partition-list failed; codesign may prompt once" >&2
  else
    log "Note: the first codesign run will ask to use the new key."
    log "      Choose \"Always Allow\" so later builds are non-interactive."
  fi
}

cmd_identity() {
  local dev_id
  dev_id=$(developer_id)
  if [[ -n "$dev_id" ]]; then
    echo "$dev_id"
    return 0
  fi
  if self_signed_exists; then
    echo "$CERT_CN"
    return 0
  fi
  return 1
}

cmd_create() {
  if self_signed_exists; then
    log "Certificate already present: $CERT_CN"
    return 0
  fi

  local dev_id
  dev_id=$(developer_id)
  if [[ -n "$dev_id" ]]; then
    log "A Developer ID identity is already available: $dev_id"
    log "Builds will use it; no self-signed certificate needed."
    return 0
  fi

  command -v openssl >/dev/null 2>&1 || fail "openssl not found on PATH"

  # Deliberately not `local`: the EXIT trap runs in global scope, where a
  # function-local would be unbound under `set -u`.
  tmp=$(mktemp -d)
  trap 'rm -rf "${tmp:-}"' EXIT

  if self_signed_imported; then
    log "Certificate already imported but not trusted for code signing; resuming"
    security find-certificate -c "$CERT_CN" -p "$KEYCHAIN" >"$tmp/cert.pem" ||
      fail "could not export the existing certificate from the keychain"
    trust_cert "$tmp/cert.pem"
    allow_key_use
    self_signed_exists ||
      fail "certificate still not usable for codesigning; check Keychain Access"
    log "Done. Rebuild the bundle, then run: $0 reset-tcc"
    return 0
  fi

  # -addext is not portable across the openssl/LibreSSL versions shipped by
  # macOS, so pass the extensions through a config file instead.
  cat >"$tmp/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions    = code_signing
prompt             = no

[dn]
CN = $CERT_CN

[code_signing]
basicConstraints = critical,CA:false
keyUsage         = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

  log "Generating self-signed code signing certificate: $CERT_CN"
  openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 3650 \
    -config "$tmp/openssl.cnf" \
    -keyout "$tmp/key.pem" -out "$tmp/cert.pem" >/dev/null 2>&1 ||
    fail "openssl failed to generate the certificate"

  # An empty PKCS#12 password is rejected by some `security import` versions,
  # so use a throwaway one. The bundle never leaves $tmp.
  #
  # OpenSSL 3 defaults to AES-256-CBC + PBKDF2, which Security.framework cannot
  # read ("MAC verification failed during PKCS12 import"), so pin the older PBE
  # algorithms macOS understands. LibreSSL (/usr/bin/openssl) ignores neither
  # flag set, hence the fallback for builds where these options are unknown.
  if ! openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
    -name "$CERT_CN" -out "$tmp/cert.p12" -passout pass:tgzterminal \
    -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 >/dev/null 2>&1; then
    openssl pkcs12 -export -inkey "$tmp/key.pem" -in "$tmp/cert.pem" \
      -name "$CERT_CN" -out "$tmp/cert.p12" -passout pass:tgzterminal >/dev/null 2>&1 ||
      fail "openssl failed to build the PKCS#12 bundle"
  fi

  log "Importing into $KEYCHAIN"
  security import "$tmp/cert.p12" -k "$KEYCHAIN" -P tgzterminal \
    -T /usr/bin/codesign -T /usr/bin/security >/dev/null ||
    fail "security import failed"

  trust_cert "$tmp/cert.pem"
  allow_key_use

  self_signed_exists ||
    fail "certificate imported but not usable for codesigning; check Keychain Access"

  log "Done. Rebuild the bundle, then run: $0 reset-tcc"
}

cmd_status() {
  echo "keychain:  $KEYCHAIN"
  echo "bundle id: $BUNDLE_ID"
  echo
  echo "codesigning identities:"
  if [[ -z "$(list_identities)" ]]; then
    echo "  (none)"
  else
    list_identities | sed 's/^/  /'
  fi
  echo
  if identity=$(cmd_identity); then
    echo "builds will sign with: $identity"
  else
    echo "builds will sign ad-hoc (\"-\"); macOS will re-prompt for folder"
    echo "access after every rebuild. Run: $0 create"
  fi
}

cmd_reset_tcc() {
  command -v tccutil >/dev/null 2>&1 || fail "tccutil not found"
  log "Resetting privacy grants for $BUNDLE_ID"
  # Non-fatal: tccutil returns non-zero when there is nothing recorded yet.
  tccutil reset All "$BUNDLE_ID" || true
  log "Quit and relaunch the app; approve each prompt once."
}

cmd_delete() {
  self_signed_exists || { log "Nothing to delete: $CERT_CN not present"; return 0; }
  log "Deleting certificate and private key: $CERT_CN"
  security delete-identity -c "$CERT_CN" "$KEYCHAIN" >/dev/null ||
    fail "security delete-identity failed"
  log "Deleted."
}

case "${1:-status}" in
  create) cmd_create ;;
  identity) cmd_identity ;;
  status) cmd_status ;;
  reset-tcc) cmd_reset_tcc ;;
  delete) cmd_delete ;;
  -h | --help | help)
    sed -n '2,41p' "$0" | sed 's/^# \{0,1\}//'
    ;;
  *) fail "unknown subcommand: $1 (try --help)" ;;
esac
