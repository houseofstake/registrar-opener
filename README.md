# registrar-opener

NEAR's multisig2 with one addition: the council can approve a list of top level names in a single
request, and a named caller can then open them in batches without going back for another vote.

This is meant to be deployed onto `registrar` on mainnet, replacing the code that is there while
keeping the multisig and its members intact.

## Why it has to live on registrar

Creating a top level account is checked against `predecessor_id`. Only `registrar` passes that
check. A cross contract call rewrites `predecessor_id` to the calling contract, so `registrar`
calling out to a separate proxy loses the right to create the account, and the create fails.
There is no arrangement of a standalone contract that gets around this. The code that issues the
CreateAccount has to be running on `registrar` itself.

That is the whole reason this is a fork of the multisig rather than a new contract beside it.

## What is upstream and what is new

`src/lib.rs` is NEAR's multisig2, unchanged apart from `mod opener;` and `#[ignore]` on six
upstream tests. `vendor/multisig2-lib.rs.orig` is the file it came from, so:

    diff vendor/multisig2-lib.rs.orig src/lib.rs

is eight lines. Everything else is in `src/opener.rs`.

The six ignored tests use `#[should_panic]`, which cannot work here. near-sdk 4 routes
`env::panic_str` through a plain `extern "C"` shim, and since rustc 1.81 a panic crossing that
boundary aborts the process instead of unwinding, so the whole test binary dies. The opener's own
refusal tests get around it by re-executing the test binary in a subprocess and checking it
failed, which is what the `refuses!` macro in `opener.rs` does.

## The grant

The council passes one request calling `grant_names`, which records against a grantee:

    grantee        who may open these names
    names          the exact list, not a count
    owner_key      the full access key each opened account gets
    funding        yoctoNEAR sent to each account
    expires_at_ns  after which nothing more opens

The grantee then calls `open_names` with up to 20 names per call. Each one is checked against the
approved list before anything is created, so a grant cannot be spent on a name the council did not
vote for. Every grant carries an epoch, and revoking bumps it, which orphans any approval left
behind rather than leaving it usable.

A create that fails returns its slot to the grant in the callback, so a batch that half lands can
be retried for the remainder without another vote.

Limits are 20 names per call and 600 per grant. The per call number comes from gas: 14 Tgas per
name against the 300 Tgas a transaction can carry, asserted at compile time next to the constant.

## Building and testing

Toolchain is pinned to 1.86 in `rust-toolchain.toml`. near-sdk will not build above it and
cargo-near enforces it.

    cargo test
    cargo near build non-reproducible-wasm --locked --no-abi
    cargo test --test sandbox

The sandbox tests need the wasm, so build before running them. They install the real mainnet
`registrar` code into a local nearcore, upgrade it in place to this build, and check the members,
the confirmation threshold and the pending requests all survive, that the multisig still executes
requests afterwards, and that a granted caller opens genuine top level accounts.

`fixtures/registrar-mainnet.wasm` is the code currently deployed at `registrar`. Verify it:

    curl -s -X POST https://rpc.mainnet.near.org -H 'Content-Type: application/json' \
      -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"view_code",
           "finality":"final","account_id":"registrar"}}' \
      | jq -r .result.code_base64 | base64 -d | sha256sum
    sha256sum fixtures/registrar-mainnet.wasm

`tests/testnet.rs` runs the same upgrade against live testnet and is ignored by default. It needs
credentials in `~/.near-credentials/testnet` and funded accounts. Point `COHORT_FILE` at a json
array of names to use your own list, otherwise it falls back to the twelve placeholders in
`fixtures/cohort.example.json`.

## What is not proven yet

The sandbox tests run against mainnet's real code but against a multisig the test builds itself,
with its own members. They do not replay mainnet's actual stored state. Its whole state is ten
key value rows, so seeding those exact bytes and running the upgrade against a byte identical copy
is the obvious next step and has not been done.

Nothing here has run on mainnet.
