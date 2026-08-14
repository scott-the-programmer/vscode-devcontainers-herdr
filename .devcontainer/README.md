# Devcontainer notes

This repo carries a devcontainer for two reasons.

## 1. It's the dev environment

Rust toolchain + clippy/rustfmt, rust-analyzer wired to `cargo clippy`, and
`target/` + the cargo registry on named volumes so builds aren't paying macOS
bind-mount costs.

## 2. It's the dogfood fixture

`herdr-devcontainer-status` detects the devcontainer state of a pane's project.
Because this repo now has `.devcontainer/devcontainer.json`, opening a herdr pane
here is itself a test case: `discover::find` resolves this directory as the
project root, and once the container is up, `docker.rs` should see a container
labelled `devcontainer.local_folder=<host path to this repo>`.

Quick manual check from the host:

```sh
docker ps -a --filter label=devcontainer.local_folder \
  --format '{{.State}}\t{{.Label "devcontainer.local_folder"}}\t{{.Names}}'
```

Start the container, run that, stop it, run it again — you get the
`running` / `exited` / absent transitions the status plugin has to render.

### The path-mapping caveat

The plugin binary is built for and runs on the **host** (`build.sh` refuses to
build in Docker for exactly this reason). Developing inside the container is
fine, but *running* the plugin from inside it will not match paths:

- `discover::find` sees `/workspaces/vscode-devcontainers-herdr`
- `devcontainer.local_folder` labels say `/Users/<you>/…/vscode-devcontainers-herdr`

`HERDR_HOST_WORKSPACE` is set in the container env to the host path so a manual
in-container run can translate. For real end-to-end verification, build via
`./build.sh` on the host and let herdr invoke the binary there.

`build.sh` may get that binary from a release download now rather than a
`cargo build` (see the root README) — either way it installs by writing
`bin/.herdr-devcontainer-status.new` and renaming it into place, whether that
file came off disk from `cargo` or out of an extracted tarball. That's not
tidiness: overwriting a macOS binary that has already been run leaves the
kernel's cached code signature mismatched, and every later exec then dies with
`SIGKILL` before `main` — no output, exit 137. Easy to miss before `make
claude`/`shell`/`relay` started running this binary every session.

## Claude Code in the container

Claude Code is **baked into the image** by `.devcontainer/Dockerfile`, not
installed at post-create time. It's a ~260MB download; as a post-create step
every rebuild paid for it again, and because that step was non-fatal
(`|| echo`) a failed download quietly produced a container with no `claude`.
In the image it's a build failure instead, and the layer is cached.

It's the native installer, not the
`ghcr.io/anthropics/devcontainer-features/claude-code` feature. The feature
installs via npm, and this is a Node-less Rust image — it would pull a
nodesource repo plus Node 18 (EOL) and a set of iptables/ipset firewall
packages nothing here uses. The native build is one self-updating binary under
`~/.local`, so `claude update` also works without sudo.

Details worth knowing:

- **`install-claude.sh` drives the download itself** rather than piping
  `https://claude.ai/install.sh` to bash. It runs the same steps (resolve
  version → verify the manifest SHA256 → let the binary install itself), but
  upstream's curl has no timeout and no retry, and this transfer stalls often
  enough from inside Docker that a plain `curl | bash` build step hangs
  forever instead of failing. Here `--speed-limit`/`--speed-time` turn a stall
  into an error and `-C -` resumes.
- **It installs as `vscode`, not root.** The installer is `$HOME`-relative, so
  as root the launcher would land in `/root/.local/bin`, invisible to the
  remoteUser. The Dockerfile switches back to `USER root` at the end, which is
  the base image's default and what features expect.
- **Only `~/.local` survives into the running container.** `~/.claude` is a
  named volume that masks whatever the installer left there — fine, because
  the launcher is `~/.local/bin/claude` symlinked into
  `~/.local/share/claude/versions/`. `install-claude.sh` asserts this at build
  time so the mount can't silently break the install.
- **`~/.local/bin` is on PATH via `ENV` in the Dockerfile**, not
  `devcontainer.json`'s `remoteEnv` — one source of truth, and it holds for
  plain `docker run` too. Caveat: a *login* shell as root (`sh -lc`) still
  loses it, because Debian's `/etc/profile` overwrites PATH. As `vscode` — the
  `remoteUser`, and the only one that matters here — it survives both login
  and non-login shells, because the installer also appended the export to
  vscode's shell rc.
- **Pin a version** by setting the `CLAUDE_VERSION` build arg in
  `devcontainer.json` (e.g. `"2.1.220"`) instead of the default `latest`.
- **Self-updates still work.** `claude update` writes to `~/.local` in the
  container's writable layer; a rebuild resets it to whatever the image has.

## Claude config in the container

The host's `~/.claude` and `~/.claude.json` are bind-mounted **read-only** at
`/host-claude` and `/host-claude.json`. `/home/vscode/.claude` is a named
volume, and `.devcontainer/seed-claude.sh` (run by post-create) copies across
credentials, `settings.json`, `CLAUDE.md`, the statusline script, and
`commands/ agents/ skills/ hooks/ plugins/`.

Session state — `history.jsonl`, `projects/`, `todos/`, `shell-snapshots/`,
`cache/` — is deliberately left behind. It's per-machine, it's most of the
host dir's ~300MB, and sharing it live means host and container sessions
racing on the same append-only files.

Consequences worth knowing:

- **Writes don't flow back.** Change a setting in here and the host is
  untouched. A token refreshed in here doesn't refresh the host's, and vice
  versa.
- **Credentials come from the macOS Keychain, not from
  `~/.claude/.credentials.json`.** On macOS that JSON file is a side-copy Claude
  stops updating once the Keychain entry exists — ours was eleven hours stale
  and held a different token from the live one. Seeding it gave the container an
  access token that had already expired alongside a refresh token the host had
  long since rotated away; the refresh failed, Claude rewrote the record with
  empty tokens, and every session in here asked for a login.
  `.devcontainer/export-creds.sh` runs on the **host** as `initializeCommand`,
  dumps the live Keychain record to `~/.claude/.container-credentials.json`, and
  that file is what gets mounted and seeded. On Linux there's no Keychain and it
  just copies the JSON, which there is the real store.

  Deliberately *not* `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`),
  which would sidestep rotation entirely but bills as **API usage** instead of
  drawing on the team subscription.
- **A blanked or expired credentials file heals itself.** The config lives on a
  named volume that outlives any single container, so before this a logged-out
  record was pinned there for good. `seed-claude.sh` now re-seeds whenever the
  container's token is missing, empty or past `expiresAt`, and
  `postStartCommand` re-runs it on every start. An *unexpired* token is left
  alone — overwriting a live session's credentials is itself a way to cause a
  rotation mismatch.
- **Refresh tokens rotate, so don't run two containers against one workspace.**
  Both get the same named volume and therefore the same credentials file, and
  whichever refreshes first invalidates the other's copy. Same reason a refresh
  in here can eventually strand the host's token; `make claude-seed` re-exports
  and re-seeds when that happens.
- **Re-seed with `make claude-seed`** (adds `--force`, overwriting the
  container copies). Plain seeding keeps whatever is already there, except for
  the two files it can tell are empty: a credentials file with no access token,
  and a `~/.claude.json` with no `projects`/`mcpServers` of its own. The latter
  is the stub the VS Code extension writes when it starts before post-create
  finishes — without the check, the seed politely keeps a 400-byte config and
  the host's MCP servers and trusted folders never arrive.
- **`~/.claude.json` is copied, not mounted.** Claude rewrites that file with
  a temp-file rename, which fails with `EBUSY` against a single-file bind
  mount. Read-only + copy avoids the whole problem.
- **The seeded credential is a real OAuth token.** Anything running in this
  container can read it. That's already true of the host Docker socket this
  container mounts, but it's worth being deliberate about: don't run untrusted
  code in here, and prefer a container-local login if you'd rather not have
  the host token present at all — drop the two `/host-claude*` mounts from
  `devcontainer.json` and `claude` will just prompt.

## Seeing the container's Claude session in herdr

Run it as **`make claude`** (not `devcontainer exec claude`), from a herdr pane.
Then the pane shows up in `herdr agent list` as a claude agent with live
idle/working/done status, same as a host-side session.

A bare `devcontainer exec … claude` shows up as nothing at all, for two
independent reasons. The plugin binary handles both, as subcommands beside the
status one it exists for:

| subcommand | runs on | what it does |
| --- | --- | --- |
| `exec <cmd>` | host | forwards the pane's herdr identity into the container, spoofs `argv0`, runs `<cmd>` (`make claude`, `make shell`) |
| `bridge start\|stop\|status\|serve` | host | serves `~/.config/herdr/herdr.sock` on `127.0.0.1:47100` (`initializeCommand`, `make relay`) |
| `relay start\|stop\|status\|serve` | container | presents that port as `~/.herdr/herdr.sock` |
| `relay … --container` | host | drives the container's relay through the CLI |

**1. The pane's identity doesn't cross the boundary.** herdr's Claude
integration (`~/.claude/hooks/herdr-agent-state.sh`, seeded into the container
with the rest of `hooks/`) reports the session over a socket, but it exits `0`
unless `HERDR_ENV`, `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` are all set. herdr
sets those in the *pane's* shell on the host; nothing carries them across
`devcontainer exec`. `exec` passes them per-exec with `--remote-env`, which is
also why they can't go in `devcontainer.json`: `containerEnv` is fixed at create
time and the pane id differs per pane.

And the socket they name has to exist *in the container*, which is what the
bridge and the relay are for — two mirror-image hops (`src/forward.rs`):

```text
container:  ~/.herdr/herdr.sock  ->  host.docker.internal:47100   (relay)
host:       127.0.0.1:47100      ->  ~/.config/herdr/herdr.sock   (bridge)
```

They exist because **Docker Desktop can't bind-mount a unix socket**: file
sharing doesn't carry sockets across the VM boundary, so mounting `herdr.sock`
gets you a path that exists and never connects. (`/var/run/docker.sock` works
only because Docker Desktop special-cases it.) The bridge binds loopback only —
Docker Desktop forwards `host.docker.internal` to the host's `127.0.0.1`, so the
herdr control socket never reaches the LAN.

Both ends are the same binary, which means the container needs a **linux build of
it**. Not cross-compiled from the host at exec time: that needs a linux linker
the host doesn't have. Instead `build.sh` cross-fetches both linux targets
(`x86_64`/`aarch64`-unknown-linux-musl, static) from the release alongside the
host binary, into `bin/linux/<triple>/` next to it. `exec` then `docker cp`s
whichever one matches the container's `uname -m` into `/tmp` the first time a
session needs one — one `devcontainer exec` round trip to check, so it's a
no-op in the steady state — and marks it executable. `/tmp` isn't persisted, so
a container restart just means the next `exec` re-pushes it, and the pushed
binary carries its own version, so a stale copy from before a plugin upgrade
is never picked up by accident.

This works in **any** project's container, not just this crate's own — the old
version of this binary ran `cargo build --release` in the container instead,
which only worked here, where the container happens to have both a Rust
toolchain and this crate's `Cargo.toml` at the workspace root. That path still
exists as a last-resort fallback for a source install with no bundled relay
binaries (`HERDR_PLUGIN_BUILD=source` before any release existed, or
`HERDR_PLUGIN_RELAY=0`), but only when the container really is this crate's own
checkout — it refuses to build an unrelated project.

**2. herdr identifies a pane's agent by `argv0`, not by the reported session.**
It scans the pane's foreground process group for a known name; all the host can
see here is `Code Helper (Plugin)` running the devcontainer CLI. Without a match
the pane stays `agent=none status=unknown` and the output rules that produce
idle/working never run — the reported session id alone doesn't change that.
So `exec` re-execs itself with `argv0` rewritten to `claude`
(`CommandExt::arg0`) and stays alive as the parent of the container command,
putting one host-side process named `claude` in the group:

```text
zsh -lc make claude
└─ make claude
   └─ claude exec claude          <- us, re-exec'd; this is what herdr matches
      └─ sh …/devcontainer exec --remote-env … claude
         └─ Code Helper (Plugin) … cli.js exec …
```

It has to be a re-exec rather than a spoofed child: the name must sit on a
process that outlives the call, and the `devcontainer` CLI is a `/bin/sh` script
that execs node, so a spoof applied to *it* would be lost. Status then comes from
herdr scanning the pane's output, which is the container TUI, so it tracks
normally.

Consequences worth knowing:

- **`make shell` claims the pane too** (`exec --agent bash`), so a `claude`
  typed inside that shell is detected, not just reported. It has to claim up
  front: herdr only ever sees the *host* side of the pane, and a `claude` started
  later inside the container changes nothing it can observe. The cost is a pane
  that reads as `agent=claude` while it's only a shell — `exec bash` without the
  flag is the honest version if that bothers you.
- **The reported transcript path is a container path.** The hook sends
  `agent_session_path` as `/home/vscode/.claude/projects/…`, which the host
  can't read, so anything herdr derives from the transcript rather than from
  pane output is unavailable. Status doesn't depend on it. Fixing it properly
  means pointing `CLAUDE_CONFIG_DIR` at a directory bind-mounted at the *same
  absolute path* on both sides, which trades away the "container writes never
  reach the host" property above.
- **`make relay` / `make relay-stop`** start/inspect both ends, and stop the host
  bridge. One bridge serves every pane; no pane owns its lifetime, and `start` is
  a no-op whenever something already answers on the endpoint.
- **`postStartCommand` doesn't start the relay.** It used to. A relay is only
  useful to a session that also has `HERDR_PANE_ID`, and only `exec` can supply
  that — so `exec` starts it, idempotently, before each session. Starting one at
  container start would mean pushing (or building) a forwarder nothing would
  talk to yet.
- **Not herdr's problem, but visible in the same place:** plugin hooks seeded
  in from the host's `~/.claude` config sometimes shell out to `node`. The
  Rust base image doesn't have one, so `devcontainer.json` adds the
  `ghcr.io/devcontainers/features/node:1` feature for exactly this — it's
  unrelated to Claude Code itself, which still uses the native, Node-free
  installer above.

## Git, SSH and gh credentials

Same read-only-mount-then-copy shape as the Claude config.
`.devcontainer/seed-creds.sh` (run by post-create) copies:

| host (read-only mount) | container |
| --- | --- |
| `~/.ssh` → `/host-ssh` | `~/.ssh/id_*`, `known_hosts`, `config` (keys re-`chmod 600`) |
| `~/.gitconfig` → `/host-gitconfig` | `~/.gitconfig`, host paths rewritten |
| `~/.config/git` → `/host-git` | `~/.config/git/allowed_signers` |
| `~/.config/gh` → `/host-gh` | `~/.config/gh` (`hosts.yml` `chmod 600`) |

Why copy instead of using the mounts directly:

- **ssh refuses a key it doesn't own.** The bind mount carries the host uid
  (501 on macOS), not `vscode`'s 1000.
- **`gh auth` and `git config --global` rewrite in place**, so a writable
  mount would edit the host's files.
- **Absolute host paths in `.gitconfig` don't exist in here.**
  `user.signingkey` and `gpg.ssh.allowedSignersFile` point at
  `/Users/<you>/…`; the seed rewrites the host `$HOME` prefix (passed in as
  `HERDR_HOST_HOME`) to `/home/vscode`. Without that, every commit fails to
  sign.
- **`gh` itself** comes from the `github-cli` feature — the token alone would
  be a file nothing reads.

Verified in-container after a rebuild: `ssh -T git@github.com` authenticates,
`gh auth status` shows the host account, and a signed empty commit verifies
`G` against the copied `allowed_signers`.

Re-seed with `make creds-seed` (`--force`, overwrites the container copies);
plain post-create seeding keeps whatever is already there.

**Security:** this puts your real SSH private key and a `gho_…` GitHub token
inside the container, readable by anything running in it — including agents.
The same caveat as the Claude token, and the same escape hatch: drop the four
mounts from `devcontainer.json` if you'd rather authenticate separately in
here. Forwarding the host SSH agent instead of copying the key is the smaller
blast radius, but `${localEnv:SSH_AUTH_SOCK}` only exists for VS Code sessions,
not for `devcontainer up` from a plain shell — which is how the Makefile
drives it.

### Exercising multi-config detection

`configs_in` also handles `.devcontainer/<subfolder>/devcontainer.json`. To test
that against this repo, add e.g. `.devcontainer/alt/devcontainer.json` — the
discovery result should then list two configs for one project root.
