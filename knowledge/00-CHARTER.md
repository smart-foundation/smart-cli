# 00 — CHARTER: what `smart-cli` is and the laws of working here

You are an agent building **smart-cli** (next generation) in
`/home/lufuno/smart/smart-cli-ng/`. Read this fully before touching anything.
Your brief (`1791632375927-network-full-completion.md`, Wave 3) says what YOU
build; this charter says what everything is. It is a copy of `smart-lib-ng`'s
charter, with the parts that bind a *client* (not the library) made explicit;
laws #8 and #9 are copied verbatim.

## What this is

`smart-cli` is the **network-domain client of the smart-protocol contract**.
It is a thin parsing + rendering shell that speaks **capnp over the contract's
transports**, and it adds NO capability: an agent can do everything the CLI
does by calling the contract directly (`knowledge/05-AI-FIRST-SURFACE.md`). The
contract — 89 frozen schemas at `/home/lufuno/smart/smart-protocol-official/` —
is read-only input. The CLI generates its OWN bindings from it at build time.

## The architecture law (the founder's correction — obey absolutely)

**The CLI does not know smart-lib. Nothing knows smart-lib.** This crate's only
dependency is the CONTRACT: the `capnp` runtime plus the bindings `build.rs`
generates from `../smart-protocol-official/schema`. There is no path dep on
smart-lib, no re-export of it, no link to it — not a dependency, not a
build-dependency, not a patch. Proof: `Cargo.lock` names only
`capnp`/`capnpc`/`embedded-io`/`smart-cli`, and the CI grep for `smart-lib` in
the dependency set is empty (`knowledge/17-cli-proofs.md`).

## The CLI's copy of the seven laws

1. **One library, every altitude** — the library CORE (`frame`, `json`,
   `selector`, `watch`) is `no_std` + `alloc`; `std` is an opt-in widening for
   the host-socket transports and I/O (the CLI ships with `std`, a hosted
   program never builds `no_std` alone).
2. **Feature = command family.** Each `cmd-*` feature turns on its family's
   schema (`schema-*`, which drives `build.rs`) and its command surface.
   Disabling a family removes its schema from codegen: `--no-default-features
   --features cmd-dns` links NOTHING from the net schemas (proven by the
   generated-file diff).
3. **A command is a thin surface** over an EXISTING service contract, bound by
   schema identity — never a hand-written permission string, never a parallel
   table. The grammar is generated from the registry + the schema.
4. **Transports are data.** `--connect <path>` / `--addr host:port` / `@host`
   select the transport; only the transport changes, the grammar does not.
5. **Authority is a handle.** No permission strings. The capability held at
   acquisition IS the authority; a mutating verb on a read-only connection is
   `permissionDenied` before the backend is reached.
6. **Honest degradation.** `Unavailable` + reason is first-class; the wire never
   pretends. No fabricated values, no fake success.
7. **Generated glue, hand-written core.** JSON/render/selector-over-contract
   logic is hand-written and small; the schema surface is generated.

## The two laws that bind this client verbatim (copied from smart-lib-ng)

8. **The host is read-only.** You never change the state of the machine you run
   on. Reading is free (`/proc`, `/sys`, `/etc`, D-Bus reads, `--print-reply`
   queries, opening devices read-only). Changing is banned: no state-changing
   calls to host services or daemons (a D-Bus method that writes, `nmcli`-style
   commands that act), no `sudo`, no killing or starting processes, no writing
   `/etc` or `/var`, no enabling/disabling anything. The incident that birthed
   this law: an agent called NetworkManager's `Enable(false)` over D-Bus to
   "probe" an unprivileged call; it succeeded, wrote `NetworkingEnabled=false`
   into `/var/lib/NetworkManager/NetworkManager.state` — persistent state that
   survived reboot and killed the host's wifi. A "what happens if" question is
   answered from documentation, reasoning, or a mock — never live fire on
   someone's machine. **The live proof of any host-mutating capability is run by
   the founder, not by you.** *(For the CLI: no test runs a live service; every
   test drives a MOCK service double speaking the frame protocol.)*

9. **The transport hierarchy: kernel first, daemon for policy, contract for
   consumers.** A host read that the kernel is authoritative for (interfaces,
   addresses, routes, counters, carrier) is read from the kernel — on Linux that
   is netlink, event-fed into a local state store, so a read costs no daemon
   hop. A daemon (D-Bus and friends) is used only for what the kernel does not
   own: connection profiles, credentials, vendor policy surfaces — and for the
   control (mutating) operations only that daemon exposes. The daemon path is a
   FALLBACK for reads, never the primary. Between our own components, the
   contract's transports carry the traffic (zero-copy where the contract says
   DataHandle). Hand-rolling a host wire protocol is a last resort, never a
   first: use maintained crate ecosystems behind a mockable seam. The proven
   failure of the alternative is recorded: a per-call D-Bus read path made every
   status query a marshalled daemon round trip and needed three wire-bug fixes
   in its own marshaller.

## File ownership

| Path | Owner |
|---|---|
| everything under `smart-cli-ng/**` | Wave-3 CLI agent |
| `smart-protocol-official/schema/**` | read-only canonical contract |
| `smart-lib-ng/**`, `naledzi/**`, `smart-protocol/**` | do not touch |

## Build discipline

- One build at a time, `-j 6`, never `cargo clean`, no `--workspace` builds of
  other projects. No git commits (the orchestrator commits).
- `cargo test -j 6` in the default config AND
  `cargo test -j 6 --no-default-features --features cmd-dns` must both be green.

## What "done" means

A box is checkable only when it is mechanically verified (build output, test
output, or a generated artifact cited in `knowledge/17-cli-proofs.md`). Never
mark by opinion.

## Law #10 — machine safety (this box is small; respect it)

The build box has **15 GB RAM and NO swap**, and `/tmp` is a tmpfs (RAM).
Commands are run with that in mind, always:

1. **Never copy a workspace that contains `target/`.** `cp -r . /tmp/...` on
   a workspace with a multi-GB `target/` copies all of it into RAM and
   freezes the machine — this already happened once (2026-10-13, a 6.7 GB
   `target` copied into the 7.7 GB tmpfs while `-j6` compiled on top; the
   machine required a hard restart). To move a workspace, copy sources only
   (`rsync -a --exclude target`, or `git archive`) — never `cp -r` of a live
   workspace.
2. **`/tmp` is not scratch space for anything big.** tmpfs is RAM; a file
   written there is memory spent. Use the workspace or the disk for scratch.
3. **Parallelism: `-j 6` is the approved bound** — it is the house number
   and crashing the machine was caused by the workspace copy, NOT by the
   job count. Do not slow work down beyond it.
4. **Verify with `cargo check` before `cargo build` when iterating**; run
   the full test suite when checks are green. Debug artifacts accumulate —
   prefer `check` for fast loops, build/test for verification.
5. **No command runs without a reason, and nothing destructive runs at
   all unless the task requires it.** If a command does not answer a
   question the task asks, it does not run — diagnostic curiosity,
   workspace duplication, and clever scratch-space moves are not reasons.
   State what a command proves before running it; if it proves nothing
   the task asks, do not run it. Focus on the work.
