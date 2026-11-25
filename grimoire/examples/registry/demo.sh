#!/bin/sh
# Publish config-reader to a local Coven registry and run the local-run app against it.
# Needs WITCHYC, WITCHY_WASMTIME_HOST and COVEN (the coven launcher) set. Run in an empty
# directory; it writes registry/, publisher/ and consumer/ there.
set -u
here=$(cd "$(dirname "$0")" && pwd)
grimoire="$here/../../grimoire"
example="$here/../local-run"
work=$(pwd)

mkdir registry publisher consumer
cp -R "$example/config-reader" publisher/
cp -R "$example/app" consumer/
rm consumer/app/main.packages
# Two witchy-2 keys (ML-DSA-65, each a 32-byte seed only its owner reads, its public key beside
# it from `coven key`): the registry's operator, which signs the registry's snapshot (no
# publisher holds it), and config-reader's owner, which signs its releases and index. The app
# declares both key IDs.
(umask 077 && head -c 32 /dev/urandom > operator && head -c 32 /dev/urandom > owner)
operator=$("$COVEN" key --key operator | cut -d' ' -f2)
owner=$("$COVEN" key --key owner | cut -d' ' -f2)
cat >> consumer/app/witchy.toml <<EOF

[registry]
path = "../../registry"
operator = "$operator"
id = "demo"

[dependencies]
config-reader = "0.1.0"

[owners]
config-reader = "$owner"
EOF

echo "== 1. publish config-reader 0.1.0, signed by its owner; publishing other source as 0.1.0 fails"
"$COVEN" publish publisher/config-reader --registry registry --key owner
echo '// another reader' >> publisher/config-reader/reader.wy
"$COVEN" publish publisher/config-reader --registry registry --key owner; echo "exit $?"
echo "-- the operator signs the snapshot that publishes it to clients"
"$COVEN" snapshot --registry registry --key operator --id demo

echo "== 2. lock, remove the publisher checkout, run"
"$grimoire" lock consumer/app
rm -r publisher
"$grimoire" run consumer/app; echo "exit $?"

echo "== 3. change the registry copy after locking: refused before compilation"
slot=registry/config-reader/0.1.0
cp "$slot/source.wy" source.saved
sed 's/port \* 10/port * 10 + 1/' source.saved > "$slot/source.wy"
printf '#!/bin/sh\necho "  witchyc $1 ${2##*/}" >> "%s/compiler.log"\nexec "%s" "$@"\n' "$work" "$WITCHYC" > logged-witchyc
chmod +x logged-witchyc
WITCHYC="$work/logged-witchyc" "$grimoire" run consumer/app; echo "exit $?"
# The same change with the registry's digest record rewritten to match it (a version's digest
# is of its `package` record and then its source).
printf 'sha384:%s' "$(cat "$slot/package" "$slot/source.wy" | shasum -a 384 | cut -c1-96)" > "$slot/digest"
WITCHYC="$work/logged-witchyc" "$grimoire" run consumer/app; echo "exit $?"
echo "compiler invocations during both refused runs:"
cat compiler.log
cp source.saved "$slot/source.wy"
printf 'sha384:%s' "$(cat "$slot/package" "$slot/source.wy" | shasum -a 384 | cut -c1-96)" > "$slot/digest"

echo "== 4. without the app's file grant, the locked dependency cannot read settings.conf"
printf '[files]\n' > consumer/app/main.grants.toml
"$grimoire" run consumer/app; echo "exit $?"
cp "$example/app/main.grants.toml" consumer/app/

echo "== 5. the app's policy holds the registry copy: config-reader given nothing is refused"
cp consumer/app/main.policy policy.saved
sed 's/^@config-reader: File<Read>/@config-reader: nothing/' policy.saved > consumer/app/main.policy
"$grimoire" run consumer/app; echo "exit $?"
cp policy.saved consumer/app/main.policy

echo "== 6. an update locks, and runs only after the app's author reviews it"
mkdir publisher && cp -R "$example/config-reader" publisher/
sed 's/"0.1.0"/"0.1.1"/' "$example/config-reader/witchy.toml" > publisher/config-reader/witchy.toml
printf '\npub fn default_port() -> U64:\n    return 3000\n' >> publisher/config-reader/reader.wy
"$COVEN" publish publisher/config-reader --registry registry --key owner
"$COVEN" snapshot --registry registry --key operator
sed 's/config-reader = "0.1.0"/config-reader = "0.1.1"/' consumer/app/witchy.toml > manifest.saved
cp manifest.saved consumer/app/witchy.toml
"$grimoire" lock consumer/app
"$grimoire" run consumer/app; echo "exit $?"
"$grimoire" review --accept consumer/app; echo "exit $?"
"$grimoire" run consumer/app; echo "exit $?"
