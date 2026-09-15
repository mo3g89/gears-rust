#!/usr/bin/env bash
# One-shot TLS material for the stack: a self-signed dev CA and two leaf
# server certificates signed by it -- one for Keycloak, one for the UI's
# nginx. certs-job.yaml runs this as a pre-install hook and stores the result
# in the qa-platform-tls Secret.
#
# WHY TLS AT ALL. oidc-authn-plugin refuses to fetch OIDC metadata over
# plaintext HTTP, so the IdP the gears talk to has to speak TLS even in a
# throwaway dev stack. Three separate checks enforce it, all against
# `UrlSecurityPolicy::STRICT`:
#   - the configured trusted-issuer `discovery_url`, at config load
#     (gears/system/authn-resolver/plugins/oidc-authn-plugin/src/config.rs:469);
#   - `s2s_oauth.discovery_url`, likewise (same file, :114);
#   - the `jwks_uri` the discovery DOCUMENT reports, at fetch time
#     (.../src/infra/oidc.rs:249-260 -- there is even a unit test named
#     `strict_policy_rejects_http_jwks_uri_from_discovery_metadata` at :379).
# The only code path that relaxes this is `allow_insecure_http_for_tests()`
# (.../src/infra/url_policy.rs:21), which is `#[doc(hidden)]`, named for tests,
# and reachable from no config field. Giving Keycloak a certificate is the
# supported way through; weakening that check is not.
#
# WHY THE BROWSER NOW NEEDS TLS TOO, i.e. why `ui.crt` exists. `oidc-client-ts`
# sends `code_challenge_method=S256` and the realm requires S256, so the login
# page has to compute a SHA-256 with `crypto.subtle` -- which browsers expose
# only in a SECURE CONTEXT: https, or http on `localhost`/`127.0.0.1`. A stack
# reached at a bare IP over http is neither, and login fails with "Crypto.subtle
# is available only in secure contexts (HTTPS)" no matter how well the rest of
# the chain agrees. That is a browser rule, not a misconfiguration, so a
# non-localhost deploy has to serve the UI over https -- which is what
# ui-tls/tls.conf and this script's `ui` leaf are for.
#
# WHY A CA AND NOT ONE SELF-SIGNED CERTIFICATE. The obvious shortcut -- one
# self-signed certificate used both as Keycloak's server certificate and as the
# gears' trust anchor -- DOES NOT WORK, and the way it fails is worth recording
# because the error names neither cause nor cure. Measured against this stack:
# a single self-signed certificate carrying `basicConstraints=critical,CA:TRUE`
# is loaded happily by reqwest and then fails every handshake with
#
#   rustls_platform_verifier: failed to verify TLS certificate:
#   invalid peer certificate: Other(OtherError(CaUsedAsEndEntity))
#
# which surfaces to the caller as a 503 "identity provider unreachable" --
# indistinguishable at the API boundary from Keycloak being down. rustls will
# not accept a CA certificate as an end-entity certificate, and it will not
# accept a non-CA certificate as a trust anchor either, so the two roles need
# two certificates. Hence: `ca.crt` (CA:TRUE, the gears' trust anchor, never
# served) signs the leaves (CA:FALSE, with the SANs, what the servers serve).
#
# WHY THE UI GETS ITS OWN LEAF RATHER THAN SHARING KEYCLOAK'S. Both are reached
# at the same public host, so one certificate would satisfy both handshakes.
# What it would not survive is the key: `keycloak.key` is Keycloak's identity,
# and serving it from nginx means the `ui` container holds it. Two leaves keep
# each private key projected into exactly one container (see the `items` lists
# in ui-deployment.yaml and gears-deployment.yaml) while one CA means a human
# trusts ONE certificate to reach both.
#
# WHY GENERATED AND NOT COMMITTED. A committed certificate means a committed
# private key. This runs as a one-shot job instead, writing into the
# qa-platform-tls Secret that keycloak, gears and ui each project the keys they
# need out of -- so nothing secret lives in git and each fresh stack gets its
# own key.
#
# WHY postgres:16 RUNS IT. It is already an image this stack pulls and it ships
# openssl 3.5.6 (checked: `command -v openssl` in postgres:16 -> /usr/bin/openssl).
# The Keycloak image itself has NO openssl (checked the same way: "not found"),
# and `alpine` would need an `apk add` at container start. This is the same
# trick `git-fixture` plays -- a stock image plus a mounted script -- rather
# than a Dockerfile for a dozen lines of openssl.
#
# IDEMPOTENT, AND PER-CERTIFICATE. Each of the three certificates is checked on
# its own and left alone if it is still good, so a stack that gains a new
# requirement re-issues only what that requirement invalidated. That matters
# most for the running-stack case: adding the `ui` leaf to an existing volume
# must not rotate the key under a live Keycloak, because `up -d` would leave
# Keycloak serving the old leaf while `gears` trusted a new CA -- the 503 above.
set -euo pipefail

# Before ANY key is written. `openssl req -keyout` creates the file at the
# process umask, which is 022 in this image -- so without this line both
# private keys exist world-readable for the moment between creation and the
# `chmod 600` at the end of this script. Narrow window, shared volume, no
# reason to leave it open.
umask 077

TLS_DIR="${TLS_DIR:-/tls}"
CA_CRT="$TLS_DIR/ca.crt"
CA_KEY="$TLS_DIR/ca.key"
KC_CRT="$TLS_DIR/keycloak.crt"
KC_KEY="$TLS_DIR/keycloak.key"
UI_CRT="$TLS_DIR/ui.crt"
UI_KEY="$TLS_DIR/ui.key"

# The containers that read these files run as uid 1000 (checked: `id` in
# quay.io/keycloak/keycloak:26.0 -> uid=1000(keycloak), and in the gears image
# -> uid=1000(appuser)). This script runs as root in postgres:16, so without the
# chown below the files land root-owned and neither reader can open them. The
# `ui` container's nginx is the exception -- its master process is root (the
# nginx:alpine image sets no USER), which is why `ui.key` can stay root-owned
# below.
OWNER_UID=1000
OWNER_GID=1000

# The host a BROWSER reaches this stack at, which is the name its TLS handshake
# checks the certificate against. Optional and defaulted, like everywhere else
# this variable appears: unset means a localhost stack, whose SANs have always
# been covered. `certs-job.yaml` passes this release's publicHost here.
PUBLIC_HOST="${PUBLIC_HOST:-localhost}"

# Same charset as deploy/docker/entrypoint.sh enforces. Here
# it keeps a stray character out of an openssl `-addext` argument, where it
# would either be rejected with an opaque message or silently produce a
# certificate with the wrong SAN.
if [[ ! "$PUBLIC_HOST" =~ ^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$ ]]; then
    echo "gen-cert: PUBLIC_HOST='$PUBLIC_HOST' is not a plain DNS hostname or IPv4 literal -- refusing to write it into a certificate's SANs" >&2
    exit 1
fi

# `IP:` and `DNS:` are not interchangeable in a SAN: a certificate whose only
# entry for an IP literal (say 203.0.113.10) is `DNS:203.0.113.10` does not match a browser
# connecting to that ADDRESS, and the failure is a bare NET::ERR_CERT_COMMON_
# NAME_INVALID with nothing pointing here.
if [[ "$PUBLIC_HOST" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]; then
    PUBLIC_SAN="IP:$PUBLIC_HOST"
else
    PUBLIC_SAN="DNS:$PUBLIC_HOST"
fi

# CN and SAN are `keycloak`, the Service name, because that is the host the
# gears dial (`https://keycloak:8443/...`) and therefore the name rustls checks
# the certificate against -- rustls matches on SAN, not CN, so the SAN is the
# load-bearing one. `localhost` and 127.0.0.1 are included so a human can
# verify either endpoint with `curl --cacert ca.crt` from inside a pod.
KC_SANS=(DNS:keycloak DNS:localhost IP:127.0.0.1)
UI_SANS=(DNS:localhost IP:127.0.0.1)
# Appended, not substituted: the local names must keep working on a stack that
# also serves a public one. Skipped when it would duplicate an entry, which is
# exactly the PUBLIC_HOST=localhost default.
if [[ " ${KC_SANS[*]} " != *" $PUBLIC_SAN "* ]]; then KC_SANS+=("$PUBLIC_SAN"); fi
if [[ " ${UI_SANS[*]} " != *" $PUBLIC_SAN "* ]]; then UI_SANS+=("$PUBLIC_SAN"); fi

# "Still valid" for a leaf means all four of: present, unexpired, CHAINS to the
# CA in this volume, and its SANs COVER every name this deploy needs.
#
# The chain check is the one presence and expiry cannot substitute for: a volume
# whose two certificates no longer belong together -- a half-finished run, a
# hand-edited volume, a CA re-issued without its leaf -- produces not an obvious
# error but the 503 "identity provider unreachable" this header warns is
# indistinguishable from Keycloak being down. Measured: replacing `ca.crt` while
# keeping the old `keycloak.crt` makes `openssl verify` report `error 20 ...
# unable to get local issuer certificate`.
#
# The SAN check is the one THIS round added, and it is a deliberate widening of
# what counts as stale. A certificate minted for a previous PUBLIC_HOST is
# present, unexpired and correctly chained, so every earlier condition kept it
# -- and it fails the handshake for the host actually being deployed. A stale
# certificate for a different host is worse than a regenerated one, so a SAN set
# that no longer covers the current host means re-issue. (It only ever grows the
# SAN list, so re-pointing a stack at a new host and back does not oscillate:
# the second deploy's names are checked against the leaf it finds, and a leaf
# minted for the wider set still covers the narrower one.)
sans_cover() {
    local crt="$1"; shift
    local printed want
    # openssl PRINTS an IP SAN as `IP Address:1.2.3.4` though it ACCEPTS
    # `IP:1.2.3.4`, so the two spellings have to be normalised before compare.
    # Whitespace is stripped rather than trusted: the value can wrap.
    printed="$(openssl x509 -in "$crt" -noout -ext subjectAltName 2>/dev/null \
        | grep -v 'Subject Alternative Name' | tr -d '[:space:]' | tr ',' '\n')"
    for want in "$@"; do
        want="${want/#IP:/IPAddress:}"
        printf '%s\n' "$printed" | grep -qxF "$want" || return 1
    done
}

leaf_ok() {
    local crt="$1" key="$2"; shift 2
    [[ -s "$crt" && -s "$key" ]] \
        && openssl x509 -in "$crt" -noout -checkend 0 >/dev/null 2>&1 \
        && openssl verify -CAfile "$CA_CRT" "$crt" >/dev/null 2>&1 \
        && sans_cover "$crt" "$@"
}

# `$1` names the files, `$2` the CN, the rest are SAN entries.
#
# -nodes: no passphrase on the key (nothing here can type one in), and it emits
# a PKCS#8 "BEGIN PRIVATE KEY" PEM, which is the form Keycloak/Quarkus reads and
# nginx accepts.
issue_leaf() {
    local crt="$1" key="$2" cn="$3"; shift 3
    local sans; sans="$(IFS=,; printf '%s' "$*")"
    echo "gen-cert: issuing $crt (CN=$cn, subjectAltName=$sans)"
    openssl req -newkey rsa:2048 -nodes -keyout "$key" -out "$crt.csr" \
        -subj "/CN=$cn" \
        >/dev/null 2>&1
    openssl x509 -req -in "$crt.csr" -sha256 -days 3650 \
        -CA "$CA_CRT" -CAkey "$CA_KEY" -CAcreateserial \
        -out "$crt" \
        -extfile <(printf '%s\n' \
            "subjectAltName=$sans" \
            "basicConstraints=critical,CA:FALSE" \
            "keyUsage=critical,digitalSignature,keyEncipherment" \
            "extendedKeyUsage=serverAuth") \
        >/dev/null 2>&1
    # The CSR and the `-CAcreateserial` serial file are byproducts, not outputs:
    # a re-issue recreates both. Removed so the volume holds exactly the files
    # the three containers actually open.
    rm -f "$crt.csr" "$TLS_DIR/ca.srl"
}

# `ca.key` is in the guard alongside `ca.crt`: without it, a volume missing the
# CA key looks complete but cannot issue a leaf at all. A regenerated CA needs
# no "force" flag for the leaves below -- the old leaves stop verifying against
# the new CA, so `leaf_ok` rejects them on its own.
if [[ -s "$CA_CRT" && -s "$CA_KEY" ]] \
    && openssl x509 -in "$CA_CRT" -noout -checkend 0 >/dev/null 2>&1; then
    echo "gen-cert: $CA_CRT is present and unexpired -- keeping it and the leaves that still chain to it"
else
    echo "gen-cert: generating a dev CA"
    # Never served to anyone; its certificate is what the gears add to their
    # root store via `http_client.custom_ca_certificate_paths`, and what a human
    # imports into a browser.
    openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
        -keyout "$CA_KEY" -out "$CA_CRT" \
        -subj "/CN=qa-platform dev CA" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" \
        >/dev/null 2>&1
fi

if leaf_ok "$KC_CRT" "$KC_KEY" "${KC_SANS[@]}"; then
    echo "gen-cert: $KC_CRT is unexpired, chains to $CA_CRT and covers ${KC_SANS[*]} -- leaving it alone"
else
    issue_leaf "$KC_CRT" "$KC_KEY" keycloak "${KC_SANS[@]}"
fi

if leaf_ok "$UI_CRT" "$UI_KEY" "${UI_SANS[@]}"; then
    echo "gen-cert: $UI_CRT is unexpired, chains to $CA_CRT and covers ${UI_SANS[*]} -- leaving it alone"
else
    issue_leaf "$UI_CRT" "$UI_KEY" "$PUBLIC_HOST" "${UI_SANS[@]}"
fi

# OWNERSHIP IS SPLIT ON PURPOSE, AND EVERY KEY IS READABLE BY EXACTLY ONE
# CONTAINER.
#
# `keycloak.key` goes to uid 1000 because the Keycloak process must read it.
# The certificates go to uid 1000 because their readers must read them.
#
# `ca.key` goes to ROOT and stays there. Nothing in the running stack ever
# needs it -- only a re-run of THIS script, which runs as root, to issue a leaf
# without invalidating the CA the gears already trust. Leaving it at uid 1000
# would have made it readable by both application processes, since both images
# happen to run as uid 1000 and so file ownership cannot tell them apart.
#
# `ui.key` goes to ROOT for the same reason, from the other direction: the UI
# pod's nginx master runs as root, so root ownership costs that container
# nothing -- while a container reading this material as uid 1000 cannot read
# the UI's private key. (The gears project `ca.crt` alone, so no key is even
# visible there -- see gears-deployment.yaml's `items`. These chowns are the
# second layer, for anything that does read the whole set.)
chown "$OWNER_UID:$OWNER_GID" "$CA_CRT" "$KC_CRT" "$KC_KEY" "$UI_CRT"
chown 0:0 "$CA_KEY" "$UI_KEY"
chmod 644 "$CA_CRT" "$KC_CRT" "$UI_CRT"
chmod 600 "$CA_KEY" "$KC_KEY" "$UI_KEY"

echo "gen-cert: $CA_CRT is the trust anchor; $KC_CRT and $UI_CRT are the server certificates"
openssl x509 -in "$KC_CRT" -noout -subject -issuer -ext subjectAltName,basicConstraints
openssl x509 -in "$UI_CRT" -noout -subject -issuer -ext subjectAltName,basicConstraints
# Proves both leaves actually chain to the CA before anything tries a handshake.
openssl verify -CAfile "$CA_CRT" "$KC_CRT" "$UI_CRT"
