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

Limits are 20 names per open call and 600 per batch. The per call number comes from gas, 14 Tgas
per name against the 300 Tgas a transaction carries, asserted at compile time next to the
constant and measured on chain in
`the_documented_per_call_maximum_fits_and_every_name_lands`.

`open_names` is payable and demands exactly `funding * count`. The operator pays for the accounts
they open, so `registrar` never holds a float. It also means a function call access key can never
reach the method, because the protocol forbids those keys from attaching a deposit.

A create that fails returns its slot to the batch in the callback, so a batch that half lands can
be finished without another vote.

## Replacing the operator is the revoke

`change_operator` bumps an epoch, and every batch, drafted or approved, is pinned to the epoch it
was built under. So replacing a compromised operator strands their approved batches in the same
transaction, with no separate revoke call and no fourth admin action. The stranded batches can
then be discarded by whoever holds the role, to reclaim the storage.

The worst a compromised operator can do is open names the council already approved, to the key the
council already saw.

## Changing the admin takes two steps

`change_admin` nominates, and the nominee has to call `accept_admin`. A typo in a single step
version would leave `registrar` with no governance at all.

## Upgrades

`approve_code(hash)` records a code hash and the earliest moment it can land. After the delay,
`upgrade(code)` checks the code against the hash, deploys it and chains `migrate`. The delay is
set at install and cannot be zero.

The approval is spent in a callback, not when the upgrade is launched, so a deploy that does not
land leaves the approval standing and the operator can retry without another 48 hour wait. The
callback panics when the deploy failed, so a bad attempt reports as a failed transaction rather
than a quiet no-op. Redeploying the same approved code is not a risk, it is the same bytes the
council already read.

One consequence worth knowing: the callback runs against the new code. A future version that
renames or removes `on_upgraded` would land its deploy and then report a failed callback, leaving
the approval set. That fails in the safe direction but it is a thing to keep in mind when writing
the next version.

The mainnet value is 48 hours. `opener_view` publishes both the configured delay and that mainnet
constant, so a deploy can be checked against it without reading the code.

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

The sandbox tests need both wasms, so build before running them. They stand up a real Sputnik DAO
from the code that `hos-root.sputnik-dao.near` is running, deploy this contract onto an account
named `registrar` in a local nearcore, and drive the whole flow through DAO proposals: a batch is
drafted, voted on by three of five council members, and the operator opens genuine top level
accounts. They also cover the refusals, the threshold, the epoch strand, the slot return, an
upgrade landing only after its delay with the batch state intact, a failed deploy leaving the
approval standing, and both install routes against mainnet's replayed state.

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

The sandbox builds its own DAO with its own members rather than replaying mainnet's policy. That
policy is five members with a majority threshold, which is three votes, matching what
`hos-root.sputnik-dao.near` runs today.

## tests/testnet.rs

The same install rehearsal against live testnet, ignored by default because it spends faucet
funds:

    cargo test --test testnet -- --ignored --nocapture

It needs no credentials and touches no existing account. It creates its own accounts from the
faucet, deploys the real mainnet multisig onto one of them, brings it up as a 2 of 2, and then
installs this contract through a multisig request, which is the sequence that would be run on
mainnet.

Run twice on 2026-09-12, both clean. The second landed on
`dev-20260912183400-31014144628914.testnet`. Verified independently over RPC rather than from the
test's own assertions: `code_hash` came back
`8DDxNfEw7KiN8gpVazUgYNCrRMqsL33GRbJv9YpbF9ta`, matching the build exactly, `opener_view`
answered with the right admin, operator and a `mainnet_upgrade_delay_ns` of 172800000000000, and
`get_members` returned `MethodResolveError(MethodNotFound)`.

## What is not proven

Mainnet's registrar code, stored state, threshold and request nonce are all replayed exactly, and
both install routes run against them.

Two things are reproduced rather than replayed, both for the same reason, that they need private
keys nobody here holds:

- The DAO the tests vote through is built by the test with its own members. Its policy is five
  members at a majority threshold, which is three votes, matching what `hos-root.sputnik-dao.near`
  runs today, but they are not the same accounts.
- The multisig route is driven by two seats appended to mainnet's member set rather than by
  mainnet's own four keys. The set's other four entries, the threshold and the nonce are
  untouched, so the only substitution is which keys sign.

The install sequence has run on live testnet twice. Nothing here has run on mainnet.
