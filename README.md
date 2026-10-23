# smart-cli

The contract-only CLI for the [smart-protocol](https://github.com/smart-foundation/smart-protocol-official) contract.

## What this is

smart-cli is a **pure contract consumer**. It does not know any implementation. It speaks capnp RPC. It generates its command surface from the contract's registry. It works with ANY compliant implementation — smart-lib, a mock, or a third-party implementation written in any language.

## The architecture law

```
smart-cli's dependencies: capnp, capnp-rpc, capnpc
smart-cli's dependencies on smart-lib: NONE
smart-cli's dependency on any implementation: NONE
```

This is proven mechanically:
- `cargo tree` in this workspace names no implementation crate
- `Cargo.lock` contains no smart-lib entry
- The CI grep (`grep -i smart-lib Cargo.lock`) returns empty

## Building

```bash
# Build the CLI
cargo build -j6

# Build with specific command families
cargo build -j6 --features cmd-files,cmd-process,cmd-settings

# Build with all families
cargo build -j6 --all-features
```

## Usage

```bash
# Connect to a service
smart --endpoint unix:/tmp/smart-service.sock status

# Or set the endpoint via environment
SMART_ENDPOINT=unix:/tmp/smart-service.sock smart status

# Or use a config file
smart status  # reads from ~/.config/smart/endpoint
```

## Testing

```bash
# All tests (one binary at a time — the full suite OOMs this box)
cargo test -j6 --test cli_golden
cargo test -j6 --test remote_parity
cargo test -j6 --test generic_path
cargo test -j6 --test bootstrap

# Headless gate (AI-first surface)
cargo test -j6 --test headless_gate

# Zero-cost proof
cargo test -j6 --test zero_cost
```

## Key principles

1. **No implementation dependency.** The CLI speaks only the contract. `cargo tree` proves it.
2. **Generated surface.** Command paths come from the service's registry (`listCommands`). No hand-written command table.
3. **Generic path.** The CLI can call a schema it has never compiled, through capnp introspection.
4. **Open transports.** Unix, tcp, and any future transport via the open registry. No closed enum.
5. **No hardcoded paradigms.** No socket path literal. No implementation name. No hardcoded ordinal.
6. **The contract is the authority.** This code consumes it; it does not define it.

## The presenter framework

`src/present.rs` carries the shell's presentation layer across the seam:
- `Cell{content, style}` split — width measured on content, not styled strings
- Width-aware middle truncation (head + basename kept)
- Unicode-width measurement (W/F=2, ambiguous=1, ANSI stripped)
- Box tables with `◉/○/●/◌/◐` glyph vocabulary

## License

MIT OR Apache-2.0
