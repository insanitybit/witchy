# Glamour server adapter

`glamour-server` is the optional native companion for progressive Glamour
forms. It turns the same checked `FormSchema` used by static HTML and the build
manifest into a `server` request handler.

```text
let signup = signup_schema()
let app = server.router().post(
    "/signup",
    glamour_server.form_action(
        signup,
        "https://example.com",
        65536,
        create_account,
    ),
)
server.serve(net, "127.0.0.1:8080", app)
```

The adapter checks the schema method and local action path, requires a configured
same-origin `Origin` or `Referer` for POST, requires URL-encoded form content,
bounds the body before decoding, rejects malformed escapes, and uses
`glamour.decode_form_entries` for duplicate, unknown, required, and field-kind
validation.

`FormSubmission` separates ordinary values from `ServerSecretValue`. A native
handler may read a named secret with `glamour.form_submission_secret`; it should
consume that value immediately and never place it in a model, response, log, or
artifact. The browser Glamour API does not expose this server-only accessor.
