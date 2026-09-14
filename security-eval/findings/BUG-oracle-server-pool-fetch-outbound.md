# Fetch must respect the oracle's inbound-only server-pool profile

Status: repaired before submission; regression passed in the 119/119 focused
run `723450bf-309a-45ac-996f-265ef99b1899`.

The pool oracle schedules each finite worker sequentially after rendezvous.
Its inbound-only restriction rejected `net_connect_at`, but initially omitted
the new `fetch_send_at`. A scripted Fetch reply could therefore stand in for
an outbound interaction with another pool instance whose execution had not
produced that reply. Numeric endpoint restrictions do not prove independence.

The shared pre-host-call guard now rejects both outbound operations in the
primary and workers, before and after rendezvous. Ordinary non-pool Fetch is
unchanged. The existing pool regression covers both operation names and all
four phase/role combinations. The guard precedes host dispatch, so refusal
must occur before a Fetch fixture call is consumed.

This is an oracle scheduling/evidence defect, not a native permission bypass.
