# herdr-devcontainer-status

A herdr plugin that makes VS Code devcontainers legible to herdr — and
makes an agent session running *inside* one show up as a normal herdr agent.

## The problem

herdr shows panes, Docker knows about containers, and nothing connects the two — so a pane
can't tell you whether its project's devcontainer is up.

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

- [Rust](https://rustup.rs) — the plugin is built from source on install
- [Docker](https://docs.docker.com/get-started/get-docker/)
- [`devcontainer` CLI](https://github.com/devcontainers/cli) — `npm i -g @devcontainers/cli`

## Getting started

```sh
herdr plugin install scott-the-programmer/vscode-devcontainers-herdr
```

Then open a herdr pane in any project with a `.devcontainer/`. The pane shows that project's
container state, and `herdr-devcontainer-status exec claude` runs an agent inside it that herdr
tracks like a host-side one.

## Contributing

The binary always runs on the **host** — never build or run it inside the container.

```sh
./build.sh                    # compile to ./bin/herdr-devcontainer-status
herdr plugin link "$(pwd)"    # use this checkout instead of an installed copy
```

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
