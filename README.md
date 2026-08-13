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
| `refresh` / `hook` | host | prints the pane project's devcontainer state as JSON (`running` / `stopped` / `none`) |
| `exec [--agent] <cmd>` | host | runs `<cmd>` in the container, forwards the pane's herdr identity in, and re-execs itself as `argv0=claude` so herdr matches the pane |
| `bridge` / `relay` | host / container | a loopback TCP hop that presents herdr's control socket inside the container |

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

Then open a herdr pane in any project with a `.devcontainer/`. The pane shows that project's
container state, and `herdr-devcontainer-status exec claude` runs an agent inside it that herdr
tracks like a host-side one.

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

## License

MIT
