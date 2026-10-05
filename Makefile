SHELL := bash
.DEFAULT_GOAL := all
export PATH := $(if $(CARGO_HOME),$(CARGO_HOME),$(HOME)/.cargo)/bin:$(PATH):/opt/homebrew/bin:/usr/local/bin

CARGO ?= cargo
PYTHON ?= $(shell bash scripts/python.sh)
CARGO_TARGET_DIR ?= target
TARGET ?=
FEATURES ?=
NO_DEFAULT_FEATURES ?= 0
SMOKE_FLAGS ?=
DIST_DIR ?= dist
BUMP ?= auto
REMOTE ?= origin
export CARGO_TARGET_DIR TARGET

HOST_TARGET = $(shell rustc -vV | sed -n 's/^host: //p')
PACKAGE_TARGET = $(if $(strip $(TARGET)),$(TARGET),$(HOST_TARGET))
TARGET_FLAGS = $(if $(strip $(TARGET)),--target "$(TARGET)",)
FEATURE_FLAGS = $(if $(filter 1,$(NO_DEFAULT_FEATURES)),--no-default-features,) $(if $(strip $(FEATURES)),--features "$(FEATURES)",)
RELEASE_DIR = $(CARGO_TARGET_DIR)/$(if $(strip $(TARGET)),$(TARGET)/,)release
EXE_SUFFIX = $(if $(findstring windows,$(PACKAGE_TARGET)),.exe,)
BINARY ?= $(RELEASE_DIR)/anytopdf$(EXE_SUFFIX)
# Runtime plugins built from this workspace and shipped beside the CLI.
PLUGINS ?= anytopdf-plugin-whisper
PLUGIN_BINARIES = $(foreach plugin,$(PLUGINS),$(RELEASE_DIR)/$(plugin)$(EXE_SUFFIX))

.PHONY: all deps providers release-deps release-plan release-resume commit-check help build build-release release fmt fmt-check check lint test test-no-default test-python verify smoke ci package doctor clean printer printer-smoke printer-remote-smoke

all: build-release

deps:
	bash scripts/bootstrap.sh

providers:
	bash scripts/bootstrap.sh providers

release-deps:
	bash scripts/bootstrap.sh release

help:
	@printf '%s\n' \
	  'make                 Install build dependencies and build the optimized CLI' \
	  'make deps            Install missing build tools and pinned Rust components' \
	  'make providers       Install FFmpeg, ExifTool, Tesseract and Poppler' \
	  'make build           Build the workspace for development' \
	  'make build-release   Build the optimized CLI and bundled plugins locally' \
	  'make release-plan    Preview SemVer and changelog changes' \
	  'make release         Version, verify, tag, push and wait for GitHub publication' \
	  'make release-resume  Retry publication of the current release tag' \
	  'make commit-check    Check Conventional Commits since the latest release' \
	  'make fmt             Format Rust sources' \
	  'make fmt-check       Check Rust formatting' \
	  'make check           Check the workspace without linking' \
	  'make lint            Run Clippy with warnings denied' \
	  'make test            Run workspace Rust tests' \
	  'make test-no-default Run Rust tests without default features' \
	  'make test-python     Run packaging and plugin tests' \
	  'make verify          Run all formatting, lint and test checks' \
	  'make smoke           Build the release CLI and smoke-test PDFs' \
	  'make ci              Verify, then build and smoke-test the release' \
	  'make package         Build, smoke-test, and archive with SHA-256' \
	  'make doctor          Inspect optional runtime providers' \
	  'make printer         Build the optional PAPPL print-server helper' \
	  'make printer-smoke   Print a test page through the helper' \
	  'make printer-remote-smoke Print over TLS through anytopdf print remote' \
	  'make clean           Remove Cargo build outputs (keeps dist/)' \
	  '' \
	  'Build/package options: TARGET=<triple> NO_DEFAULT_FEATURES=1 FEATURES=<list>' \
	  'Other options: PYTHON=python CARGO_TARGET_DIR=target DIST_DIR=dist PLUGINS=<crates>' \
	  'PDF checks: SMOKE_FLAGS="--require-poppler --strict"'

build: | deps
	$(CARGO) build --workspace --locked $(TARGET_FLAGS) $(FEATURE_FLAGS)

build-release: | deps
	$(CARGO) build --release --locked -p anytopdf $(TARGET_FLAGS) $(FEATURE_FLAGS)
	$(if $(strip $(PLUGINS)),$(CARGO) build --release --locked $(foreach plugin,$(PLUGINS),-p $(plugin)) $(TARGET_FLAGS),)

fmt: | deps
	$(CARGO) fmt --all

fmt-check: | deps
	$(CARGO) fmt --all -- --check

check: | deps
	$(CARGO) check --workspace --locked

lint: | deps
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

test: | deps
	$(CARGO) test --workspace --locked

test-no-default: | deps
	$(CARGO) test --workspace --no-default-features --locked

test-python: | deps
	"$(PYTHON)" -m unittest discover -s tests -p 'test_*.py'

verify: | deps
	CARGO="$(CARGO)" PYTHON="$(PYTHON)" sh scripts/verify.sh

smoke: build-release
	"$(PYTHON)" scripts/smoke.py --binary "$(BINARY)" $(SMOKE_FLAGS)

# Keep verification ahead of release work even with make -j.
ci: verify
	$(MAKE) smoke

package: smoke
	$(foreach plugin,$(PLUGIN_BINARIES),"$(plugin)" --anytopdf-manifest >/dev/null &&) true
	"$(PYTHON)" scripts/package.py --target "$(PACKAGE_TARGET)" --binary "$(BINARY)" $(foreach plugin,$(PLUGIN_BINARIES),--plugin "$(plugin)") --output "$(DIST_DIR)"

doctor: | deps
	$(CARGO) run --locked -p anytopdf -- doctor

# The print-server helper is C on PAPPL and stays out of the Cargo workspace.
printer:
	$(MAKE) -C helpers/anytopdf-printer

printer-smoke: build-release printer
	PYTHON="$(PYTHON)" bash helpers/anytopdf-printer/smoke.sh helpers/anytopdf-printer/anytopdf-printer "$(BINARY)"

printer-remote-smoke: build-release printer
	PYTHON="$(PYTHON)" bash helpers/anytopdf-printer/remote-smoke.sh helpers/anytopdf-printer/anytopdf-printer "$(BINARY)"

clean:
	$(CARGO) clean

release-plan: | deps
	"$(PYTHON)" scripts/release.py plan --bump "$(BUMP)"

release: | release-deps
	"$(PYTHON)" scripts/release.py publish --bump "$(BUMP)" --remote "$(REMOTE)"

release-resume: | release-deps
	"$(PYTHON)" scripts/release.py resume --remote "$(REMOTE)"

commit-check: | deps
	"$(PYTHON)" scripts/release.py check
