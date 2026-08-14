# Using herdr-devcontainer-status

A step-by-step guide, starting from the one assumption that you already have
[herdr](https://herdr.dev) installed and running.

## 1. Prerequisites

- [Docker](https://docs.docker.com/get-started/get-docker/), running.
- The [`devcontainer` CLI](https://github.com/devcontainers/cli):
  ```sh
  npm install -g @devcontainers/cli
  ```
- [Rust](https://rustup.rs) — only if step 2 can't find a prebuilt binary for your platform
  (macOS or Linux, x86_64 or arm64). Most people don't need this.

## 2. Install the plugin

```sh
herdr plugin install scott-the-programmer/vscode-devcontainers-herdr
```

herdr clones the repo, shows you a trust preview (pass `--yes` to skip it in a script), then
runs the manifest's build step. That downloads the release binary matching the checkout's
version for your platform, verifies it against the release's `SHA256SUMS`, and installs it —
along with two small statically-linked Linux copies of the same binary, which is what lets
`exec` (step 6) work inside *any* project's container later, with nothing more to install there.
If no prebuilt binary covers your platform, or the download fails, it falls back to
`cargo build` — this is the one path that needs Rust.

## 3. Verify it installed

```sh
herdr plugin list
herdr plugin action list --plugin devcontainer-status
```

The first should list `devcontainer-status` as installed and enabled; the second should show
one action, `refresh`.

## 4. See devcontainer status — automatically

Open a herdr pane in any project that has a `.devcontainer/`. Nothing to run: herdr fires the
plugin for you on `pane.created`, `pane.focused`, and `workspace.focused`, and it reports what
it finds (`running` / `stopped` / `none`) back to herdr as a `devcontainer` metadata token on
the pane (or, for `workspace.focused`, on the workspace).

That token is invisible until you add it to a sidebar row — it's off by default like any other
plugin token:

```toml
# ~/.config/herdr/config.toml
ui.sidebar.agents.rows = [["state_icon", "workspace", "tab"], ["agent", "$devcontainer"]]
```

Reload herdr's config (or restart) and the token shows up next to each pane once its project's
devcontainer state has been detected.

## 5. Check it by hand

You don't need to wait for an event to fire. Ask for a refresh directly:

```sh
herdr plugin action invoke devcontainer-status.refresh
```

and see what actually happened — every hook/action run, its exit status, and its output — with:

```sh
herdr plugin log list --plugin devcontainer-status
```

## 6. Run an agent inside the container, tracked by herdr

This is the other half of the plugin: running `claude` *inside* a project's devcontainer while
herdr still shows it as a normal pane in `herdr agent list`, with live idle/working/done status.

First, make sure that project's devcontainer is actually up:

```sh
devcontainer up --workspace-folder .
```

Then find where herdr installed the plugin — `herdr plugin list --json` includes each plugin's
path, or look directly under `~/.config/herdr/plugins/github/devcontainer-status-<hash>/`. From
a herdr pane opened at that project's root:

```sh
~/.config/herdr/plugins/github/devcontainer-status-<hash>/bin/herdr-devcontainer-status exec claude
```

That single command handles everything underneath: it starts the host-side bridge if it isn't
already running, `docker cp`s a matching relay binary into the container's `/tmp` the first time
it's needed, and forwards your pane's herdr identity in — all with nothing to configure. Ctrl-C
behaves normally; the pane just stops being a tracked agent when the command exits.

Use `exec --agent bash` instead of `exec claude` to open a claiming shell — useful if you want
to `claude` from inside it yourself later, since herdr only ever sees the host side of the pane
and can't detect an agent started *after* the shell already began.

### Put it on your `PATH`

There's no manifest action for `exec` itself, since it has to stay bound to the pane it's run
from — so typing the full install path every time gets old fast. Add this to your shell rc file
(`~/.zshrc`, `~/.bashrc`, …); it globs for the plugin's `bin/` directory so it keeps working
across reinstalls, even if the `-<hash>` suffix on the checkout changes:

```sh
# herdr-devcontainer-status
for _herdr_devstatus_bin in "$HOME/.config/herdr/plugins/github"/devcontainer-status-*/bin; do
  [ -d "$_herdr_devstatus_bin" ] && export PATH="$_herdr_devstatus_bin:$PATH"
done
unset _herdr_devstatus_bin
```

(On Windows, or if you'd rather resolve the path explicitly than glob for it, ask herdr instead:
`herdr plugin list --json` includes each plugin's install path.)

With that on `PATH`, add a couple of aliases for the commands you'll actually type:

```sh
alias hdc='herdr-devcontainer-status exec claude'         # run claude, tracked by herdr
alias hds='herdr-devcontainer-status exec --agent bash'   # a claiming shell
alias hdr='herdr-devcontainer-status refresh'              # status as JSON, for this pane
```

`hdc` from any herdr pane in a project with a `.devcontainer/` is the everyday version of step 6.

## 7. The everyday loop

Once it's installed and a sidebar row is configured, day to day this is entirely passive:

- Open a pane in a devcontainer project → its `$devcontainer` status appears in the sidebar with
  no action from you.
- Want to work inside that container with an agent herdr can see? `hdc` (or the full
  `exec claude`) from a pane there.
- Something looks stale? `hdr`, or `herdr plugin action invoke devcontainer-status.refresh`.

## 8. Troubleshooting

- **`devcontainer CLI not found on PATH`** — `npm install -g @devcontainers/cli`, then make sure
  npm's global bin directory is actually on `PATH` in a non-interactive shell (herdr doesn't
  start one).
- **`no container labelled for <path>`** — that project's devcontainer hasn't been started yet:
  `devcontainer up --workspace-folder .` in it first.
- **`no relay binary published for container arch <arch>`** — the container's CPU architecture
  isn't one of the two the plugin ships (`x86_64`/`aarch64` Linux, static musl). `exec` still
  runs the command; only agent-state reporting is unavailable. The one exception is this
  plugin's own checkout, which falls back to building the relay in-container instead.
- **`error: checksum mismatch for ...`** — a corrupted or tampered download. Re-run
  `herdr plugin install` (or `./build.sh` from a linked checkout); it downloads fresh.

## 9. Updating or removing it

herdr v1 has no separate "update a plugin" command — reinstalling gets you the latest release:

```sh
herdr plugin uninstall devcontainer-status
herdr plugin install scott-the-programmer/vscode-devcontainers-herdr
```

To remove it for good, just the uninstall.
