# herdr-devcontainer-status

A herdr plugin that makes VS Code devcontainers legible to herdr — and
makes a Claude Code session running *inside* one show up as a normal herdr agent.

## The problem

1. **You can't tell from a pane whether its project's devcontainer is up.** herdr shows panes;
   Docker knows about containers; nothing connects the two.
2. **`devcontainer exec claude` is invisible to herdr.** herdr identifies a pane's agent by
   scanning the foreground process group for a process named `claude` — across the exec
   boundary all it sees is the devcontainer CLI. And herdr's agent-state hook inside the
   container has no pane identity and no socket to report on, because Docker Desktop can't
   bind-mount a unix socket.

## What it does

One host binary with three jobs:

| subcommand | runs on | what it does |
| --- | --- | --- |
| `refresh` / `hook` | host | prints the pane project's devcontainer state as JSON (`running` / `stopped` / `none`) |
| `exec [--agent] <cmd>` | host | runs `<cmd>` in the container, forwards the pane's herdr identity in, and re-execs itself as `argv0=claude` so herdr matches the pane |
| `bridge` / `relay` | host / container | a loopback TCP hop that presents herdr's control socket inside the container |

The repo is also its own test fixture: it carries a `.devcontainer/`, so opening a herdr pane
here exercises the detection path.

## Getting started

Requires a Rust toolchain, Docker, and the [`devcontainer` CLI](https://github.com/devcontainers/cli)
on the host. The binary always runs on the **host** — never build or run it inside the container.

```sh
./build.sh                        # compile to ./bin/herdr-devcontainer-status
herdr plugin link "$(pwd)"        # register with herdr
```

Then, from a herdr pane in this repo:

```sh
make up        # create/start the devcontainer
make status    # what the plugin reports for this pane
make claude    # run Claude in the container, visible in `herdr agent list`
make shell     # shell in, claiming the pane as an agent
```

`make help` lists the rest (`rebuild`, `reshell`, `relay`, `clean`, `e2e`, …).

## More

`.devcontainer/README.md` covers the container itself in depth: credential seeding, why Claude
Code is baked into the image, the bridge/relay design, and the security tradeoffs of mounting
host credentials into a container.

## License

MIT
