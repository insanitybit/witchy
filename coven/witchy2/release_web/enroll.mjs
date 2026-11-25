const file = document.querySelector('#request');
const details = document.querySelector('#details');
const create = document.querySelector('#create');
const prove = document.querySelector('#prove');
const output = document.querySelector('#response');
const status = document.querySelector('#status');
let request;
let credential;

function b64url(bytes) {
    return btoa(String.fromCharCode(...bytes)).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '');
}

function publicKey(created) {
    const data = new Uint8Array(created.response.getAuthenticatorData());
    const id = new Uint8Array(created.rawId);
    if (created.response.getPublicKeyAlgorithm() !== -7 || data.length < 55 || id.length === 0 || id.length > 1023 ||
        (data[32] & 0x40) === 0 || (data[32] & 0x80) !== 0 ||
        new DataView(data.buffer).getUint16(53) !== id.length || data.length !== 55 + id.length + 77 ||
        !id.every((byte, index) => byte === data[55 + index])) throw new Error('Unsupported credential');
    return b64url(data.subarray(55 + id.length));
}

file.addEventListener('change', async () => {
    request = undefined;
    credential = undefined;
    create.disabled = true;
    prove.hidden = true;
    output.hidden = true;
    output.value = '';
    details.textContent = '';
    const source = file.files[0];
    try {
        if (!source || source.size > 65536) throw new Error('Invalid enrollment request');
        const parsed = JSON.parse(await source.text());
        if (file.files[0] !== source) return;
        if (parsed.format !== 'coven-enrollment-1' || parsed.origin !== location.origin ||
            typeof parsed.challenge !== 'string' || !/^[A-Za-z0-9_-]{64}$/.test(parsed.challenge) ||
            !Number.isSafeInteger(parsed.expires) || parsed.expires * 1000 <= Date.now() ||
            typeof parsed.registry !== 'string' || !Array.isArray(parsed.packages) ||
            !parsed.packages.every(name => typeof name === 'string')) throw new Error('Invalid enrollment request');
        request = parsed;
        details.textContent = `${parsed.replace ? 'Replace a passkey for' : 'Enroll a passkey for'} ${parsed.packages.join(', ')} on ${parsed.registry}.`;
        status.textContent = '';
        create.disabled = false;
    } catch {
        if (file.files[0] === source) status.textContent = 'The request is invalid, expired, or belongs to another origin.';
    }
});

create.addEventListener('click', async () => {
    create.disabled = true;
    file.disabled = true;
    try {
        const created = await navigator.credentials.create({publicKey: {
            challenge: crypto.getRandomValues(new Uint8Array(32)),
            rp: {id: location.hostname, name: 'Coven'},
            user: {id: crypto.getRandomValues(new Uint8Array(32)), name: request.registry, displayName: 'Coven maintainer'},
            pubKeyCredParams: [{type: 'public-key', alg: -7}],
            authenticatorSelection: {residentKey: 'required', userVerification: 'required'},
            attestation: 'none',
        }});
        credential = {id: created.id, public_key: publicKey(created)};
        prove.hidden = false;
        status.textContent = 'Confirm the new passkey to create your enrollment response.';
    } catch {
        status.textContent = 'The passkey could not be created.';
        create.disabled = false;
        file.disabled = false;
    }
});

prove.addEventListener('click', async () => {
    prove.disabled = true;
    try {
        const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON({
            challenge: request.challenge,
            rpId: location.hostname,
            allowCredentials: [{type: 'public-key', id: credential.id}],
            userVerification: 'required',
        });
        const assertion = await navigator.credentials.get({publicKey});
        if (assertion.id !== credential.id) throw new Error('Wrong credential');
        output.value = JSON.stringify({format: request.format, challenge: request.challenge,
                                       public_key: credential.public_key, assertion: assertion.toJSON()});
        output.hidden = false;
        prove.hidden = true;
        status.textContent = 'Save this JSON and return it to the registry operator.';
    } catch {
        status.textContent = 'The new passkey could not be confirmed.';
        prove.disabled = false;
    }
});
