const digest = 'sha384:' + location.pathname.split('/').at(-1);
const output = document.querySelector('#status');
const inspect = document.querySelector('#inspect');
const approve = document.querySelector('#approve');
const review = document.querySelector('#review');
let approval;

async function post(route, body) {
    const response = await fetch(route, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        credentials: 'omit', redirect: 'error', body: JSON.stringify(body),
    });
    if (!response.ok) throw new Error('Request refused');
    return response.json();
}

async function assertion(options) {
    const publicKey = PublicKeyCredential.parseRequestOptionsFromJSON(options);
    const credential = await navigator.credentials.get({ publicKey });
    return { challenge: options.challenge, assertion: credential.toJSON() };
}

function add(parent, tag, value) {
    const element = document.createElement(tag);
    element.textContent = value;
    parent.append(element);
}

function render(candidate) {
    if (candidate.digest !== digest) throw new Error('Unexpected candidate');
    if (candidate.approval) {
        const intent = candidate.approval.envelope.intent;
        if (intent.operation !== 'release' || intent.registry !== candidate.registry || intent.name !== candidate.name ||
            intent.version !== candidate.version || intent.digest !== digest) throw new Error('Unexpected release intent');
    }
    add(review, 'h2', `${candidate.name} ${candidate.version}`);
    const facts = document.createElement('dl');
    const rows = [
        ['Registry', candidate.registry], ['Candidate digest', candidate.digest],
        ['Entry file', candidate.comparison.entry],
        ['Dependencies', Object.entries(candidate.comparison.requires).map(([name, version]) => `${name} ${version}`).join(', ') || 'None'],
        ['Previous published version', candidate.comparison.previous || 'First active release'],
    ];
    if (candidate.comparison.previous_digest) rows.push(['Previous digest', candidate.comparison.previous_digest]);
    if (candidate.provenance) {
        const ci = candidate.provenance;
        rows.push(['CI workflow', ci.workflow], ['Workflow commit', ci.workflow_commit], ['Source commit', ci.revision],
                  ['CI run', `${ci.run_id}, attempt ${ci.run_attempt}, check ${ci.check_run_id}`]);
    } else rows.push(['Staged by', 'Local operator']);
    for (const [label, value] of rows) {
        add(facts, 'dt', label);
        add(facts, 'dd', value);
    }
    review.append(facts);
    add(review, 'h3', 'Source changes');
    if (candidate.comparison.files.length === 0) add(review, 'p', 'No source file changes.');
    for (const file of candidate.comparison.files) {
        add(review, 'h4', `${file.file} (${file.state})`);
        add(review, 'pre', file.diff);
    }
    review.hidden = false;
    approval = candidate.approval;
    approve.hidden = !approval;
    if (!approval) output.textContent = candidate.approved ? 'This candidate is already approved.' : 'This version has a competing approval.';
}

inspect.addEventListener('click', async () => {
    inspect.disabled = true;
    output.textContent = 'Verify with your passkey to view the candidate.';
    try {
        const request = await post('/v1/review/challenge', { digest });
        const candidate = await post('/v1/review/inspect', await assertion(request.publicKey));
        output.textContent = '';
        render(candidate);
        inspect.hidden = true;
    } catch {
        output.textContent = 'Review was refused. Check the candidate link and your maintainer credential.';
        inspect.disabled = false;
    }
});

approve.addEventListener('click', async () => {
    approve.disabled = true;
    output.textContent = 'Verify with your passkey to approve the displayed digest.';
    try {
        const receipt = await post('/v1/review/approve', await assertion(approval.publicKey));
        if (receipt.digest !== digest) throw new Error('Unexpected release receipt');
        output.textContent = 'Approval recorded for the displayed digest.';
        approve.hidden = true;
    } catch {
        output.textContent = 'Approval was refused. Reload to review again if the challenge expired.';
        approve.disabled = false;
    }
});

if (typeof globalThis.PublicKeyCredential?.parseRequestOptionsFromJSON !== 'function') {
    output.textContent = 'This browser does not support the required passkey API.';
} else {
    inspect.disabled = false;
}
