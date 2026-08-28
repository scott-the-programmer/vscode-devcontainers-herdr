# herdr-devcontainer-status

A [herdr](https://herdr.dev) plugin that makes VS Code devcontainers visible to herdr, and
lets an agent (Claude Code, OpenCode, etc.) running *inside* one show up as a normal,
tracked herdr agent, the same as one running on the host.

## Quick start

```sh
herdr plugin install scott-the-programmer/vscode-devcontainers-herdr
```

That's it for installation. Nothing to configure. Now open a herdr pane in any project
that has a `.devcontainer/`. herdr detects the container's state (`running` / `stopped` /
`none`) automatically and reports it back, ready to show in the sidebar once you add the
`$devcontainer` token to a row:

```toml
# ~/.config/herdr/config.toml
ui.sidebar.agents.rows = [["state_icon", "workspace", "tab"], ["agent", "$devcontainer"]]
```

To run an agent inside that container while herdr still tracks the pane, use `exec` from
that same pane:

```sh
herdr-devcontainer-status exec claude
```

This starts `claude` inside the devcontainer, wired up so herdr sees it exactly like an
agent on the host, with the same idle/working/done status.

For the full walkthrough (finding the binary's install path, aliases worth adding,
troubleshooting), see [`USAGE.md`](USAGE.md).

## Why this exists

herdr shows panes and Docker shows containers, but nothing connects the two by default.
An agent running inside a devcontainer is invisible to herdr. This plugin fixes that: it
reports the container's status and tracks agent sessions running inside it.

## What it does

One host binary, three jobs:

| subcommand | runs on | what it does |
| --- | --- | --- |
| `refresh` / `hook` | host | detects the pane project's devcontainer state and reports it to herdr |
| `exec [--agent <name>] <cmd>` | host | runs `<cmd>` inside the container, carrying the pane's herdr identity in so herdr tracks it |
| `bridge` / `relay` | host / container | the plumbing that lets the container side talk back to herdr's control socket on the host |

`exec` works against **any** project's devcontainer, not just checkouts of this repo. It
pushes a small statically-linked (musl) copy of itself into the target container the first
time it's needed, so nothing needs to be installed there ahead of time, and the target
project doesn't need a Rust toolchain or any relation to this crate at all.

See [`.devcontainer/README.md`](.devcontainer/README.md) if you want the internals of how
the two ends of the relay fit together.

## Requirements

- [Docker](https://docs.docker.com/get-started/get-docker/), running
- The [`devcontainer` CLI](https://github.com/devcontainers/cli): `npm i -g @devcontainers/cli`
- [Rust](https://rustup.rs), only needed if there's no prebuilt binary for your platform
  (macOS/Linux, x86_64/arm64), or you're building from a checkout

## How installation works

`herdr plugin install` clones this repo and runs `./build.sh`, which downloads the release
binary matching that checkout's version for your platform, verifies it against the
release's `SHA256SUMS`, and installs it. If no prebuilt binary covers your platform, or the
download fails, it falls back to `cargo build` (needs Rust). `SHA256SUMS` is an integrity
check, not provenance; releases also carry a
[build provenance attestation](https://github.com/scott-the-programmer/vscode-devcontainers-herdr/attestations),
verifiable out of band with
`gh attestation verify --repo scott-the-programmer/vscode-devcontainers-herdr <tarball>`.

## Contributing

The binary always runs on the **host**. Never build or run it inside the container.

```sh
./build.sh --source            # compile to ./bin/herdr-devcontainer-status (what `make build` runs)
herdr plugin link "$(pwd)"     # use this checkout instead of an installed copy
```

`./build.sh` with no flags tries a prebuilt download first, same as `herdr plugin install`.
`HERDR_PLUGIN_BUILD=prebuilt ./build.sh` is useful for debugging just that path.

This repo carries a `.devcontainer/` of its own, so it doubles as the test fixture. From a
herdr pane here:

```sh
make up        # create/start the devcontainer
make status    # what the plugin reports for this pane
make claude    # run Claude in the container, visible in `herdr agent list`
make shell     # shell in, claiming the pane as an agent
make test      # cargo test
```

`make help` lists the rest (`rebuild`, `reshell`, `relay`, `clean`, `e2e`, …), and
[`.devcontainer/README.md`](.devcontainer/README.md) covers the container in depth:
credential seeding, the bridge/relay design, and the security tradeoffs of mounting host
credentials into a container.

Maintainers cutting a release should see [`docs/RELEASING.md`](docs/RELEASING.md).

## License

MIT
