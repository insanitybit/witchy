#!/bin/zsh
# Regenerate using disposable synthetic keys. Commit only the public vectors.
set -euo pipefail
umask 077
vectors=${0:A:h}
scratch="$(mktemp -d)"
trap 'rm -r -- "$scratch"' EXIT
cd "$scratch"
openssl ecparam -name secp384r1 -genkey -noout -out root.key
openssl req -new -x509 -sha384 -key root.key -days 2 -subj '/CN=Witchy test root' \
    -addext 'basicConstraints=critical, CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign' -out root.pem
openssl ecparam -name secp384r1 -genkey -noout -out leaf.key
openssl req -new -sha384 -key leaf.key -subj '/CN=api.example.test' -out leaf.csr
cat > leaf.ext <<'EXT'
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:api.example.test
EXT
openssl x509 -req -sha384 -CA root.pem -CAkey root.key -set_serial 2 -days 1 \
    -extfile leaf.ext -in leaf.csr -out leaf.pem
printf '%s' 'Synthetic Witchy PKIX test transcript' > message
openssl dgst -sha384 -sign leaf.key -out "$vectors/signature.der" message
for role in root leaf; do
    openssl x509 -in "$role.pem" -outform DER -out "$role.der"
    ruby -e 'd = File.binread(ARGV[0]); STDOUT.write([d.bytesize].pack("N").byteslice(1, 3) + d)' \
        "$role.der" > "$vectors/$role.bundle"
done
ruby -e 'STDOUT.write([Time.now.to_i + 60].pack("Q>") + File.binread(ARGV[0]))' \
    message > "$vectors/signed.bin"
