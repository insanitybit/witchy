#!/usr/bin/env bash
# A complete local tour of the coven registry: start a registry server, publish
# a rune to it through trusted publishing (short-lived identity token, no API
# key), promote it with a second factor, then consume it from a separate
# project — resolution, signature verification, capability-footprint gating and
# all. Everything runs on localhost in a throwaway directory.
#
#   cargo build --release && ./scripts/local-registry-demo.sh
set -euo pipefail

BIN="${WITCHY_BIN:-$(dirname "$0")/../target/release/witchy}"
if [ ! -x "$BIN" ]; then
    echo "witchy binary not found at $BIN — run: cargo build --release" >&2
    exit 1
fi
BIN="$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")"

step() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

DEMO="$(mktemp -d "${TMPDIR:-/tmp}/witchy-registry-demo.XXXXXX")"
export WITCHY_HOME="$DEMO/home"        # isolated store + keys (not ~/.witchy)
export WITCHY_COOLDOWN_SECS=0          # demo: skip the staging cooldown so a fresh release resolves immediately
SERVER_PID=""
cleanup() {
    if [ -n "$SERVER_PID" ]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$DEMO"
}
trap cleanup EXIT
cd "$DEMO"

step "1. Generate the demo identity provider (the 'GitHub Actions' of this demo)"
mkdir -p idp
PUBHEX="$("$BIN" coven-gen-issuer --out "$DEMO/idp")"
echo "issuer public key: $PUBHEX"

step "2. Start the coven registry server on localhost"
mkdir -p registry
# The registry signs its records with a root signing key (the lockfile pins it).
LC_ALL=C head -c 32 /dev/urandom | od -An -v -tx1 | tr -d ' \n\t' > "$DEMO/registry-signing.seed"
# coven-serve binds the exact address it is given, so pre-pick a free port and
# pass it concretely (rather than relying on :0 ephemeral-port discovery).
PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
export COVEN_URL="http://127.0.0.1:$PORT"
"$BIN" coven-serve --addr "127.0.0.1:$PORT" --root "$DEMO/registry" \
    --signing-key "$DEMO/registry-signing.seed" \
    --trust-issuer "local-idp=$PUBHEX" > "$DEMO/server.log" &
SERVER_PID=$!
for _ in $(seq 1 100); do
    grep -q "coven serving" "$DEMO/server.log" 2>/dev/null && break
    sleep 0.1
done
echo "registry serving at $COVEN_URL"

step "3. Author a library rune"
mkdir -p lib/src
cat > lib/witchy.toml <<'EOF'
[rune]
name = "acme/shout"
version = "1.0.0"
EOF
cat > lib/src/shout.witchy <<'EOF'
// Uppercase greeting helpers. Pure: no capability parameters, so this rune's
// computed footprint is empty — the consumer can verify that.
pub fn shout(s: String) -> String:
    "HEY " <> string.to_upper(s)
EOF

step "4. Publish via TRUSTED PUBLISHING (a short-lived CI identity token)"
CI_TOKEN="$("$BIN" coven-mint-token --issuer-key "$DEMO/idp" --issuer local-idp \
    --sub "repo:acme/shout-repo:ref:refs/heads/main" \
    --claim repository=acme/shout-repo --claim workflow_ref=release.yml \
    --claim ref=refs/heads/main)"
(cd lib && WITCHY_USER=ci-bot COVEN_ID_TOKEN="$CI_TOKEN" "$BIN" publish .)
echo "(the version lands STAGED — it is not yet resolvable)"

step "5. Promote to RELEASED (a human, with a second factor)"
HUMAN_TOKEN="$("$BIN" coven-mint-token --issuer-key "$DEMO/idp" --issuer local-idp \
    --sub alice --claim amr=webauthn)"
(cd lib && WITCHY_USER=alice COVEN_ID_TOKEN="$HUMAN_TOKEN" \
    "$BIN" promote acme/shout 1.0.0)

step "6. Create a CONSUMER project and add the dependency"
mkdir -p app/src
cat > app/witchy.toml <<'EOF'
[rune]
name = "demo/app"
version = "0.1.0"
EOF
cat > app/src/app.witchy <<'EOF'
import shout

fn main(console: Console):
    print(console, shout.shout("from the local registry"))
EOF
(cd app && WITCHY_USER=dev "$BIN" add acme/shout)
echo "--- the lockfile pins content hashes AND the registry's signing key:"
grep -E "name|sha256|ed25519" app/witchy.lock | sed 's/^/    /'

step "7. Inspect the dependency's capability footprint before trusting it"
(cd app && WITCHY_USER=dev "$BIN" tree)
(cd app && WITCHY_USER=dev "$BIN" audit src/app.witchy)

step "8. Build and run the consumer"
(cd app && WITCHY_USER=dev "$BIN" run .)

step "9. The gate: a publish from the WRONG repository is refused"
cat > lib/witchy.toml <<'EOF'
[rune]
name = "acme/shout"
version = "1.1.0"
EOF
EVIL_TOKEN="$("$BIN" coven-mint-token --issuer-key "$DEMO/idp" --issuer local-idp \
    --sub "repo:evil/fork:ref:refs/heads/main" \
    --claim repository=evil/fork --claim workflow_ref=release.yml \
    --claim ref=refs/heads/main)"
if (cd lib && WITCHY_USER=ci-bot COVEN_ID_TOKEN="$EVIL_TOKEN" "$BIN" publish . 2>/dev/null); then
    echo "UNEXPECTED: the rogue publish succeeded" >&2
    exit 1
else
    echo "refused, as it must be: the 'acme' namespace is bound to acme/shout-repo + release.yml"
fi

step "Done — the full lifecycle ran against a local registry"
echo "publish (trusted, staged) -> promote (2FA, released) -> add (verified) -> audit -> run"
