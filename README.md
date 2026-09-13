# registrar-opener

A contract for `registrar` that lets a security council approve a list of top level names in one
vote, after which a named operator opens them in batches without going back for another vote.

Governance is a Sputnik DAO. There are two roles. The admin is the DAO and does three things:
approve a batch, change the operator, change the admin. The operator drafts batches and opens the
names once a batch is approved. The council never has to look at a name twice and never has to
vote per name.

## Why it has to live on registrar

Creating a top level account is checked against `predecessor_id`, and the predecessor is whatever
contract emits the create. A separate proxy loses that right in either direction, so the code that
issues the CreateAccount has to be running on `registrar` itself. That is proven here rather than
asserted: `a_proxy_cannot_open_a_name_even_when_registrar_signs_the_call` deploys a proxy, has
`registrar` sign the call into it, and the runtime still refuses with
`CreateAccountOnlyByRegistrar`.

This is also why installing it replaces what is on `registrar` rather than sitting beside it. A
NEAR account holds one contract.

## The batch

    create_batch(owner_key, funding) -> batch_id     operator
    add_names(batch_id, names)                       operator, draft only
    approve_batch(batch_id, digest)                  admin
    open_names(batch_id, names)                      operator, approved only, payable
    discard_batch(batch_id)                          operator on a draft, admin on anything
    forget_names(batch_id, names)                    operator or admin, discarded batches only

A batch carries the exact names, one full access key that every opened account receives, and the
funding each account is created with. The approval is bound to a digest over all three, so editing
a batch after a vote does not inherit that vote, it invalidates it.

The digest is folded as the batch is built. `create_batch` seeds it from the owner key and the
funding, and every `add_names` call chains each name into it:

    seed   = sha256("registrar-opener:batch:v1" || owner_key || funding_le)
    digest = sha256(previous_digest || name)

Every name is hashed exactly once, in the call that was already writing it to storage, so no call
ever walks the whole batch and `approve_batch` is a single 32 byte comparison. Anyone can
recompute the digest off chain from the published list, which is what makes the vote meaningful;
`the_stored_digest_is_the_one_an_outside_observer_computes` does exactly that in the test suite.

A batch has no size limit. Names sit in a `LookupSet`, which cannot be iterated and has no length,
so a name is one storage record and the only thing the contract ever does to one is a single
`storage_has_key`, `storage_write` or `storage_remove`. No call reads more than one name, so no
batch is large enough to make one collapse. That record measures 57 bytes on chain, and opening
the name hands it straight back, which
`a_name_costs_one_record_and_opening_it_gives_that_record_back` asserts in both directions.

Limits are 20 names per open call and 4 live batches. The per call number comes from gas, 14 Tgas
per name against the 300 Tgas a transaction carries, asserted at compile time next to the constant
and measured on chain in `the_documented_per_call_maximum_fits_and_every_name_lands`.

What a set nothing can enumerate costs is that discarding a batch cannot return the records of
names that were never opened. Those keys are already unreachable, since every read goes through
the batch record discarding removed, but they would sit on the account forever, so `forget_names`
deletes them, 100 per call, refusing any batch that is still live so it cannot reach into one the
council is using.

Keying names by batch id also puts weight on something that used to be only hygiene.
`next_batch_id` only ever climbs and panics rather than wrapping, and neither `new` nor `migrate`
will reset it. An id that came round again would inherit whatever the dead batch stranded under
the same prefix, which lets a revoked approval come back rather than staying revoked. Only the
operator can call `open_names`, so that was never a way in for anyone else, but a revoke should
stay a revoke.

Names must be top level, between 3 and 64 bytes, and not an implicit address. The last one uses
the protocol's own `get_account_type().is_implicit()`, so it covers NEAR implicit accounts,
Ethereum implicit accounts and NEP-616 deterministic addresses rather than a hand rolled hex
check. Opening one would squat an address somebody else derives from their own key.

`open_names` is payable and demands exactly `funding * count`. The operator pays for the accounts
they open, so `registrar` never holds a float. It also means a function call access key can never
reach the method, because the protocol forbids those keys from attaching a deposit.

A create that fails returns its slot to the batch in the callback, so a batch that half lands can
be finished without another vote. The account create, the transfer and the key are one batched
receipt, so either all three land or none do and there is no half opened account to reconcile.

## Revoking

`change_operator` locks the old operator out immediately, because every operator method compares
the caller against the one stored in state. A replaced key cannot draft, open or discard anything
from the moment the call lands. That is the answer to a compromised operator.

Batches survive the change on purpose. The council approved their contents by digest, so the
names and the key are what was voted on regardless of who drafted them, and the incoming operator
can either finish them or discard them.

The admin can discard any batch, which is the council withdrawing an approval it already gave. The
operator can only discard a draft or a batch whose names have all been opened. That asymmetry
matters in a case that needs no attacker at all. If the same name sits in two approved batches and
is opened from the first, the second can never drain, so without the admin path it would hold one
of the four slots forever. Either way discarding is a single write, since nothing has to be walked
to tear a batch down.

The worst a compromised operator can do is open names the council already approved, to the key the
council already saw.

## Changing the admin takes two steps

`change_admin` nominates, and the nominee has to call `accept_admin`. A typo in a single step
version would leave `registrar` with no governance at all.

## Upgrades

`upgrade(code)` is admin only. It deploys the code and chains `migrate` in the same receipt, and a
callback panics if the deploy did not land so a bad attempt reports as a failed transaction rather
than a quiet no-op.

There is no timelock, deliberately. A delay only buys anything if somebody is watching and can
cancel inside it, which needs guardians and a notification system, and that is a whole stack to
build and keep running. Without it a delay is ceremony. The real gate is that the admin is a
3 of 5 DAO whose proposals are visible on chain.

The method is admin only rather than admin or operator for the same reason: with nothing pre
approved, whoever can call `upgrade` can put arbitrary code on `registrar`.

## How it gets onto registrar

The multisig that is there now installs its own replacement. `execute_request` in multisig2
chains every action in a request onto one `Promise`, so a single request with `receiver_id` set to
`registrar` and actions

    [ DeployContract { code }, FunctionCall { "new", args, gas } ]

deploys and initialises in one receipt, with no window where the account has new code and no
state. `new` refuses any caller that is not the account itself, which closes the gap where someone
races in between a deploy and its init and names themselves admin. That also covers the other
route, a FullAccess key signing the same two actions in one transaction, since a transaction
signed by `registrar` against `registrar` has `predecessor_id == registrar`.

The DAO cannot do this. A Sputnik `FunctionCall` proposal can only call a method that already
exists on `registrar`, and multisig2 has no self deploy. The DAO becomes the authority once the
new code is live, not before.

Both routes are proven against mainnet's own stored state, replayed into a local nearcore:

- `the_opener_installs_over_mainnets_own_stored_state` takes the FullAccess key route, all ten
  mainnet rows patched in unchanged, one transaction carrying both actions. It then drafts a
  batch, has the DAO approve it and opens a real top level account on top.
- `the_multisig_installs_the_opener_from_mainnets_own_state_at_its_own_threshold` takes the
  multisig route. Mainnet's four member keys are left untouched and two seats we hold are
  appended, moving only the member count in `STATE` from four to six. One seat raises the deploy
  request and a second confirms it.

Three things in that second test are load bearing. The threshold is read out of mainnet's own
blob and asserted to be 2, so the request executes at mainnet's real confirmation count. The
request comes back as id 1, and a freshly initialised multisig starts at nonce 0, so that is
direct evidence mainnet's own `request_nonce` was in play. And before any of it runs, the test
encodes one of mainnet's real member keys with the same helper used for the appended seats and
asserts the result matches a real index row from the fixture, so the seats are stored the way the
chain stores members rather than in a shape that merely happens to work.

A member identified by a public key has to sign a transaction sent from `registrar` to
`registrar`. `current_member` only takes the key based branch when
`current_account_id == predecessor_account_id`; anything else is treated as an account based
member and refused. That is why the four member keys are access keys on `registrar` itself.

## What replacing the multisig costs

Worth being explicit, because it is not reversible through this path:

- The 2 of 4 multisig on `registrar` stops existing. As of 2026-09-12 it has four member keys, a
  threshold of 2, and `list_request_ids` is empty, so nothing is in flight.
- Those four member keys are function call keys on `registrar` itself and go inert. That is not
  an assumption about how they were provisioned, it follows from `current_member` only accepting
  a key based member when the transaction comes from `registrar` to `registrar`.
- Deploying replaces `STATE` and nothing else. The multisig's other nine rows, four member
  entries, four index entries and one request counter, are orphaned in place rather than cleared,
  so the account keeps paying storage for them. A few hundred bytes, measured in the replay test
  below, which asserts ten rows before and ten after.
- `registrar` can then only do what this contract exposes. Generic actions like Transfer or AddKey
  are gone except through a FullAccess key.
- `registrar` holds two FullAccess keys, so a bad deploy is recoverable by whoever holds them.

## Building and testing

Toolchain is pinned in `rust-toolchain.toml`.

    cargo test --lib
    cargo near build non-reproducible-wasm --locked --no-abi
    cargo build -p registrar-opener-stub --target wasm32-unknown-unknown --release
    cargo test --test sandbox

The sandbox tests need both wasms, so build before running them. Nothing about the governance in
them is invented. They stand `hos-root.sputnik-dao.near` up at that exact account id, running the
code it runs on mainnet, adopting the policy it has on mainnet, with its five real member accounts
and its real threshold of three. The contract goes onto an account named `registrar`, and the flow
runs through genuine DAO proposals: a batch is drafted, three of the five real members vote, and
the operator opens genuine top level accounts.

They also cover the refusals, the threshold stopping one vote short, an operator replacement
locking the old one out while the new one finishes the batch, the admin revoke, the slot return,
a stranded batch forgotten back to the exact byte it started on, a batch id proven never to be
reissued, a DAO driven upgrade carrying the batch state across, a failed
deploy leaving the contract working, the operator being refused an upgrade, a registrar key
failing to forge a callback, and both install routes against mainnet's replayed state.

Lint is two passes, because the sandbox dev dependencies cannot build for wasm:

    cargo clippy --lib --target wasm32-unknown-unknown -- -D warnings
    cargo clippy --tests -- -D warnings

For the bytes that would actually be deployed, build them reproducibly:

    cargo near build reproducible-wasm --no-abi

That runs inside `sourcescan/cargo-near:0.22.0-rust-1.97.1` pinned by digest in `Cargo.toml`, and
builds the git commit rather than the working tree, so it needs a clean worktree and the HEAD
commit pushed. Two people running it at the same commit get the same sha256, which is what makes
it possible for a council to vote on a hash rather than on trust. The hash for a given commit is
recorded in that commit's annotated tag, not in a file, because cargo-near stamps the commit into
the wasm and committing the hash would change it.

A plain `cargo build` produces a wasm the nearcore VM rejects. Use cargo-near for anything that
will be deployed.

## Fixtures

`fixtures/registrar-mainnet.wasm` is the code deployed at `registrar` today. Verify it:

    curl -s -X POST https://rpc.mainnet.near.org -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"view_code",
           "finality":"final","account_id":"registrar"}}' \
      | jq -r .result.code_base64 | base64 -d | sha256sum
    sha256sum fixtures/registrar-mainnet.wasm

Expected: `f3f9ee3e5e29d020dc73f581ff1490c03dff875ed01327dfebbe666724930a76`, 340613 bytes.

`fixtures/sputnik-dao-v2.3.1.wasm` is the code `hos-root.sputnik-dao.near` is running. Same
command with that account id gives
`3b4a563dcbfb10f004bd1f912a35df0c83a6eaade8cd8f70bc626d9ab92f19b9`, 539647 bytes.

`fixtures/registrar-mainnet-state.json` is every key and value stored on `registrar`, ten rows,
pulled with:

    curl -s -X POST https://rpc.mainnet.near.org -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"view_state",
           "finality":"final","account_id":"registrar","prefix_base64":""}}' \
      | jq -S -c .result.values

It was taken from two independent providers and the responses were byte identical.
`mainnets_own_stored_state_reads_back_under_mainnets_own_code` patches those exact rows into a
sandbox under the real mainnet code and asserts the four member keys, the threshold of 2 and the
empty request list all come back, so the fixture is proven to be real state and not a
plausible-looking blob. Both install routes then run on top of it, as described under how it gets
onto registrar.

The ten rows are the multisig's own layout: four member elements under the `\0e` prefix, four
index entries under `\0i`, one request counter keyed by a member, and `STATE`. `STATE` opens with
the member set's index prefix followed by the element count, which is the only field the multisig
route's test rewrites.

Both fixture wasms can also be checked against the hash the protocol itself computed, rather than
against a download, by comparing `code_hash` from `view_account` with base58 of the file's
sha256. Today that is `HRP7Qf2HDaXTD8EWs7G8siNNGxWWBCe1JaDxcM2cJmQR` for `registrar` and
`4zSoHkLjJWZm34Psd4Eq2WUXHELt6LxtKmUXpsZWAECp` for the DAO.

`fixtures/hos-root-dao-policy.json` is the live policy of `hos-root.sputnik-dao.near`, read with
`get_policy` from two independent providers with identical responses. The sandbox initialises its
DAO with that exact object and asserts `get_policy` returns it unchanged, so the roles, the five
member accounts, the `RoleWeight` threshold of three, the bond and the proposal period are
mainnet's rather than a reproduction. Refresh it with:

    curl -s -X POST https://rpc.mainnet.near.org -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"call_function",
           "finality":"final","account_id":"hos-root.sputnik-dao.near",
           "method_name":"get_policy","args_base64":"e30="}}' \
      | jq -r '[.result.result[]] | implode' | jq .

The member accounts are created in the sandbox holding test keys, which is the only way any test
can vote as them, and the DAO sees the votes arrive from those account ids exactly as it would on
mainnet.

The multisig install test is the one place a signer is added rather than reused. Mainnet's four
multisig members are bare public keys, and no private key exists for them outside their holders,
so the test keeps all four untouched and appends two seats it can sign with, moving only the
member count in `STATE` from four to six. The threshold, the request nonce and every other byte
stay mainnet's, and the multisig's execution path does not branch on which member confirms.

## tests/testnet.rs

The same install rehearsal against live testnet, ignored by default because it spends faucet
funds:

    cargo test --test testnet -- --ignored --nocapture

It needs no credentials and touches no existing account. It creates its own accounts from the
faucet, deploys the real mainnet multisig onto one of them, brings it up as a 2 of 2, and then
installs this contract through a multisig request, which is the sequence that would be run on
mainnet.

Run twice on 2026-09-12, both clean, against the shape the contract had that day. Verified
independently over RPC rather than from the test's own assertions: the installed `code_hash`
matched the build exactly, `opener_view` answered with the right admin and operator, and
`get_members` returned `MethodResolveError(MethodNotFound)`.

## Status

The install sequence has run on live testnet twice. Nothing here has run on mainnet yet.
