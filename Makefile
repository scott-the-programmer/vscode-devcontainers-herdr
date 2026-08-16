# Dev loop for the devcontainer dogfood fixture (see .devcontainer/README.md).
#
# The plugin binary runs on the HOST and reads host `docker ps` labels, so every
# target here is host-side. Nothing in this file is meant to run in-container.

SHELL := /bin/sh
.DEFAULT_GOAL := help

ROOT    := $(CURDIR)
BIN     := $(ROOT)/bin/herdr-devcontainer-status
VOLUMES := herdr-devcontainer-status-target herdr-devcontainer-status-cargo \
           herdr-devcontainer-status-claude

# Containers this repo's devcontainer produced, newest first.
LABEL := label=devcontainer.local_folder=$(ROOT)
CID    = $$(docker ps -aq --filter '$(LABEL)' | head -1)

.PHONY: help build test check status up rebuild reshell start stop down ps shell logs clean e2e claude-seed creds-seed claude relay relay-stop

help: ## Show this help
	@grep -hE '^[a-z-]+:.*?## ' $(MAKEFILE_LIST) \
	  | awk -F':.*?## ' '{printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

## --- host build / unit tests ---------------------------------------------

build: $(BIN) ## Build the host binary into ./bin (via build.sh)

# A file target, not .PHONY: `claude`, `shell` and `relay` all run through this
# binary now, and rebuilding it on every one of them would add a cargo run to
# each session start.
#
# --source: this is the dev loop, editing src/ — installing a released binary
# over that work (build.sh's default) would silently hide it.
$(BIN): Cargo.toml Cargo.lock build.sh $(wildcard src/*.rs)
	./build.sh --source

test: ## Run cargo tests
	cargo test

check: ## clippy + rustfmt check
	cargo clippy --all-targets -- -D warnings
	cargo fmt --check

## --- devcontainer lifecycle ----------------------------------------------

up: ## Create/start the devcontainer (devcontainer CLI)
	devcontainer up --workspace-folder .

rebuild: ## Rebuild the image and recreate the container (keeps the named volumes)
	devcontainer up --workspace-folder . --remove-existing-container

start: ## Restart the existing container without rebuilding
	@test -n "$(CID)" || { echo "no container yet — run 'make up'"; exit 1; }
	docker start $(CID)

stop: ## Stop the container (leaves it around as 'exited')
	@test -n "$(CID)" || { echo "no container to stop"; exit 0; }
	docker stop $(CID)

down: ## Stop and remove the container
	@test -n "$(CID)" || { echo "no container to remove"; exit 0; }
	docker rm -f $(CID)

claude-seed: ## Re-copy host Claude config into the container (overwrites)
	# export-creds.sh first: `devcontainer exec` doesn't run initializeCommand,
	# so without it the mounted credentials are whatever the last container
	# start exported, which may have expired since.
	.devcontainer/export-creds.sh
	devcontainer exec --workspace-folder . .devcontainer/seed-claude.sh --force

creds-seed: ## Re-copy host git/SSH/gh credentials into the container (overwrites)
	devcontainer exec --workspace-folder . .devcontainer/seed-creds.sh --force

claude: $(BIN) ## Run Claude in the container, reporting agent state to this herdr pane
	@$(BIN) exec claude

shell: $(BIN) ## Shell into the running container (as a claude agent pane)
	# --agent: herdr only sees the host side of a pane, so a `claude` started
	# later inside this shell can't be detected unless the claim is already in
	# place. Costs a pane that reads as agent=claude while it's only a shell.
	@$(BIN) exec --agent claude bash

relay: $(BIN) ## Start/inspect both ends of the herdr agent-state bridge
	@$(BIN) bridge start
	@$(BIN) relay start --container

relay-stop: $(BIN) ## Stop the host-side herdr socket bridge
	@$(BIN) bridge stop

reshell: ## Rebuild the container, set up the relay, then shell into it
	@$(MAKE) --no-print-directory rebuild
	# A rebuild takes the container-side relay with it, and `shell` only starts
	# one when it has a pane to report for — so bring both ends up here, where
	# the container is new and the first in-container build gets paid once.
	@$(MAKE) --no-print-directory relay
	@$(MAKE) --no-print-directory shell

logs: ## Tail container logs
	@test -n "$(CID)" || { echo "no container"; exit 1; }
	docker logs -f $(CID)

clean: down ## Remove the container and its named volumes
	-docker volume rm $(VOLUMES)

## --- what the plugin sees -------------------------------------------------

ps: ## Raw docker view of this repo's devcontainer
	@docker ps -a --filter label=devcontainer.local_folder \
	  --format '{{.State}}\t{{.Label "devcontainer.local_folder"}}\t{{.Names}}'

status: $(BIN) ## Run the plugin against this repo (build first if needed)
	@HERDR_PANE_CWD=$(ROOT) $(BIN) refresh | jq .

## --- the full transition check --------------------------------------------

e2e: $(BIN) ## running -> stopped -> none, as the plugin reports it
	@$(MAKE) --no-print-directory up
	@echo '--- expect status=running'
	@$(MAKE) --no-print-directory status
	@$(MAKE) --no-print-directory stop
	@echo '--- expect status=stopped'
	@$(MAKE) --no-print-directory status
	@$(MAKE) --no-print-directory down
	@echo '--- expect status=none (config present, no container)'
	@$(MAKE) --no-print-directory status
