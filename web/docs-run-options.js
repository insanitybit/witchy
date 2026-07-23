// Deterministic, explicit page grants shared by the live book and its browser
// audit. Each run gets a fresh in-memory Dir from this immutable source data.
const utf8 = new TextEncoder();

async function docsFetch(url) {
  return {
    status: 200,
    redirected: false,
    type: "basic",
    headers: new Map([["content-type", "text/plain; charset=utf-8"]]),
    arrayBuffer: async () =>
      utf8.encode(`witchy documentation fixture for ${url}`).buffer,
  };
}

export const DOCS_RUN_OPTIONS = Object.freeze({
  args: ["error", "notes.txt"],
  capabilities: {
    clock: true,
    console: { input: ["Ada"] },
    dir: {
      write: true,
      files: {
        "app.log": "started\n",
        "config.toml": "mode = \"docs\"\n",
        "config.txt": "docs configuration\n",
        "log.txt": "docs log\n",
        "notes.txt": "first line\nerror: example diagnostic\n",
        "uploads/.keep": "",
      },
    },
    env: {
      HOME: "/home/witchy",
      SCAN_IGNORE_CASE: "1",
      WRITE: "1",
    },
    fetch: {
      origins: ["https://example.com"],
    },
    vm: true,
    secrets: {
      signing: {
        value: "0000000000000000000000000000000000000000000000000000000000000000",
        useOnly: true,
      },
      "api-token": "sk-live-abc",
    },
  },
  fetchImpl: docsFetch,
});
