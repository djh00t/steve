SHELL := /bin/sh

CARGO ?= cargo
IMAGE ?= steve:dev

.PHONY: help run doctor test-upstream build test check quality-gates install update clean hooks docker-build

help:
	@printf '%s\n' \
		'make run            Run Steve using config.toml by default' \
		'make doctor         Validate database, object storage and config' \
		'make test-upstream  Run deterministic development upstream on :18080' \
		'make build          Build the debug binary' \
		'make test           Run all tests' \
		'make check          Pre-commit formatting/clippy/check gates' \
		'make quality-gates  Hosted CI check, test and release-build gates' \
		'make install        Install/update the steve binary with Cargo' \
		'make update         Fast-forward source and reinstall' \
		'make hooks          Enable repository pre-commit/pre-push hooks' \
		'make docker-build   Build the local OCI image' \
		'make clean          Remove Rust build artifacts'

run:
	$(CARGO) run -- serve

doctor:
	$(CARGO) run -- doctor

test-upstream:
	$(CARGO) run -- test-upstream

build:
	$(CARGO) build --all-features

test:
	$(CARGO) test --all-features

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(CARGO) check --all-targets --all-features

quality-gates: check test
	$(CARGO) build --release --all-features

install:
	$(CARGO) install --path . --force

update:
	git pull --ff-only
	$(MAKE) install

hooks:
	git config core.hooksPath .githooks
	@echo "Git hooks enabled: pre-commit=make check, pre-push=make check"

docker-build:
	docker build -t $(IMAGE) .

clean:
	$(CARGO) clean
