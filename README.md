# herdr-devcontainer-status

A herdr plugin that makes VS Code devcontainers legible to herdr — and
makes an agent session running *inside* one show up as a normal herdr agent.

## The problem

herdr shows panes, Docker knows about containers, and nothing currently connects the two. This relay bridges the gap

Coding harnesses such as Claude Code, OpenCode and Pi do not show up in herdr by default when
they run inside one. This binary attempts to simplify the process by starting a session with a
thin relay to feed back agent information to herdr.

## What it does

One host binary with three jobs:

| subcommand | runs on | what it does |
| --- | --- | --- |
| `refresh` / `hook` | host | prints the pane project's devcontainer state as JSON (`running` / `stopped` / `none`) and reports it to herdr as pane/workspace metadata |
| `exec [--agent <name>] <cmd>` | host | runs `<cmd>` in the container, forwards the pane's herdr identity in, and re-execs itself with `argv0` set to the claimed agent's name (from `<cmd>`, or overridden by `--agent`) so herdr matches the pane |
| `bridge` / `relay` | host / container | a loopback TCP hop that presents herdr's control socket inside the container |

`exec` works against **any** project's devcontainer, not just checkouts of this crate: it
`docker cp`s a small statically-linked (musl) copy of this same binary into the target
container's `/tmp` the first time it's needed, so agent-state reporting doesn't depend on that
project having a Rust toolchain, or being this crate at all. Nothing to configure — see
[Contributing](#contributing) if you want to see it work, or `.devcontainer/README.md` for how
the two ends of the relay fit together.

## Requirements

- [Docker](https://docs.docker.com/get-started/get-docker/)
- [`devcontainer` CLI](https://github.com/devcontainers/cli) — `npm i -g @devcontainers/cli`
- [Rust](https://rustup.rs) — only if there's no prebuilt binary for your platform (see below), or
  you're building from a checkout

## Getting started

```sh
herdr plugin install scott-the-programmer/vscode-devcontainers-herdr
```

`herdr plugin install` clones the repo and runs `./build.sh`, which downloads the release binary
matching that checkout's version for your platform — macOS and Linux, x86_64 and arm64 — verifies
it against the release's `SHA256SUMS`, and installs it. If no prebuilt binary covers your
platform, or the download fails, it falls back to `cargo build` (needs Rust). `SHA256SUMS` is an
integrity check, not provenance; releases also carry a
[build provenance attestation](https://github.com/scott-the-programmer/vscode-devcontainers-herdr/attestations),
which you can verify out of band with `gh attestation verify --repo
scott-the-programmer/vscode-devcontainers-herdr <tarball>`.

Then open a herdr pane in any project with a `.devcontainer/`. herdr fires the plugin
automatically on `pane.created`/`pane.focused`/`workspace.focused`, so the pane's devcontainer
state (`running`/`stopped`/`none`) shows up as a `$devcontainer` metadata token with no command
to run — add it to a sidebar row to see it:

```toml
# ~/.config/herdr/config.toml
ui.sidebar.agents.rows = [["state_icon", "workspace", "tab"], ["agent", "$devcontainer"]]
```

`herdr-devcontainer-status exec claude` runs an agent inside that container that herdr tracks
like a host-side one.

## Native Linux Docker

The defaults assume Docker Desktop, which forwards `host.docker.internal` to the
host's loopback. Plain Linux Docker does neither, so point both ends at the
docker bridge:

```sh
# in the herdr pane, or your shell profile
export HERDR_RELAY_HOST=172.17.0.1    # exec forwards this into the container
export HERDR_BRIDGE_BIND=172.17.0.1   # the host bridge listens here too
```

Use your own docker bridge address — `ip -4 addr show docker0` — which is also
what `--add-host=host.docker.internal:host-gateway` resolves to, if you would
rather keep the name and set only `HERDR_BRIDGE_BIND`.

Set both or neither. Moving only the container end leaves the bridge on
loopback, which turns a name-resolution failure into a refused connection, and
moving only the host end leaves the container dialling a name that does not
resolve. `~/.herdr/relay.log` inside the container names which one you have:

```text
relay: host.docker.internal:47100: failed to lookup address information   # no name
relay: 172.17.0.1:47100: Connection refused                               # bridge still on loopback
```

`HERDR_BRIDGE_BIND` widens who can reach herdr's control socket, and that socket
is not authenticated — on the docker bridge address, every container on that
bridge can reach it. That is why it is opt-in and why the default stays on
loopback.

Note that none of this affects whether the agent *appears* in herdr. Pane
identity is passed inward as environment at exec time and needs no socket, so a
broken relay looks like a working session whose state is screen-detected rather
than reported. That is the failure this is easy to miss.

## Contributing

The binary always runs on the **host** — never build or run it inside the container.

```sh
./build.sh --source            # compile to ./bin/herdr-devcontainer-status (what `make build` runs)
herdr plugin link "$(pwd)"     # use this checkout instead of an installed copy
```

`./build.sh` with no flags tries a prebuilt download first, same as `herdr plugin install` —
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
`.devcontainer/README.md` covers the container in depth — credential seeding, the bridge/relay
design, and the security tradeoffs of mounting host credentials into a container.

Maintainers cutting a release should see [`docs/RELEASING.md`](docs/RELEASING.md).

## License

MIT
