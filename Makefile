SHELL := /bin/sh

CARGO ?= cargo
IMAGE ?= steve:dev
M1_VENV ?= target/m1-sdk-venv
M1_PYTHON := $(M1_VENV)/bin/python
M1_EXPECTED_SHA ?= $(shell git rev-parse HEAD)
M1_CANDIDATE_DIR := target/m1-candidate-$(shell git rev-parse --short=12 HEAD)
M1_CANDIDATE_BINARY := $(M1_CANDIDATE_DIR)/debug/steve
M1_TARGET_PARENT ?=
M1_DEPLOYMENT_PATH ?=

.PHONY: help run doctor test-upstream build test check workflow-check quality-gates install update clean hooks docker-build m1-sdk-setup m1-target-qualification m1-demo m1-release-gate

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
		'make m1-target-qualification M1_TARGET_PARENT=/absolute/deploy/parent M1_DEPLOYMENT_PATH=/absolute/deploy/root' \
		'make m1-demo        Open the guided M1 candidate gate' \
		'make m1-release-gate  Run the headless M1 candidate gate' \
		'make clean          Remove Rust build artifacts'

run:
	$(CARGO) run --locked -- serve

doctor:
	$(CARGO) run --locked -- doctor

test-upstream:
	$(CARGO) run --locked -- test-upstream

build:
	$(CARGO) build --locked --all-features

test:
	$(CARGO) test --locked --all-features

check:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --all-targets --all-features -- -D warnings
	$(CARGO) check --locked --all-targets --all-features

workflow-check:
	@set -eu; output=$$(python3 -m agent_workflow.demo); test -n "$$output"; printf '%s\n' "$$output" | python3 -c 'import json,sys; result=json.load(sys.stdin); assert result.get("ok") is True and result.get("scenarios"), "empty or failed workflow demo"'; printf '%s\n' "$$output"
	@set -eu; output=$$(python3 scripts/dispatch_work_packages.py); test -n "$$output"; python3 -c 'import json,sys; summary=json.loads(sys.argv[1].splitlines()[0]); assert summary.get("packages", 0) > 0, "empty work-package inventory"' "$$output"; printf '%s\n' "$$output"

quality-gates: check test
	$(CARGO) build --locked --release --all-features

install:
	$(CARGO) install --locked --path . --force

update:
	git pull --ff-only
	$(MAKE) install

hooks:
	git config core.hooksPath .githooks
	@echo "Git hooks enabled: pre-commit=make check, pre-push=make check"

docker-build:
	docker build -t $(IMAGE) .

m1-sdk-setup:
	python3 -m venv $(M1_VENV)
	$(M1_PYTHON) -m pip install --disable-pip-version-check -r tests/requirements-sdk.txt
	PYTHONOPTIMIZE=1 $(M1_PYTHON) -c 'from importlib.metadata import version; import sys; expected={"anthropic":"1.2.0","openai":"3.6.0"}; found={name:version(name) for name in expected}; print(found); sys.exit(0 if found == expected else "wrong SDK versions")'

m1-target-qualification:
	test -n "$(M1_TARGET_PARENT)" || { echo 'M1_TARGET_PARENT must be the absolute deployment parent or mount' >&2; exit 2; }
	test -n "$(M1_DEPLOYMENT_PATH)" || { echo 'M1_DEPLOYMENT_PATH must be the intended absolute accounting root' >&2; exit 2; }
	$(CARGO) build --locked --all-features --target-dir $(M1_CANDIDATE_DIR)
	M1_DEPLOYMENT_PATH="$(M1_DEPLOYMENT_PATH)" PYTHONOPTIMIZE=1 python3 -B scripts/m1_release_gate.py --qualify-target "$(M1_TARGET_PARENT)" --candidate-binary $(M1_CANDIDATE_BINARY) --expected-sha $(M1_EXPECTED_SHA) --output target/m1-target-qualification.json

m1-demo: m1-sdk-setup
	test -n "$(M1_DEPLOYMENT_PATH)" || { echo 'M1_DEPLOYMENT_PATH must be the intended absolute accounting root' >&2; exit 2; }
	M1_DEPLOYMENT_PATH="$(M1_DEPLOYMENT_PATH)" PYTHONOPTIMIZE=1 $(M1_PYTHON) -B scripts/m1_release_gate.py --guided --expected-sha $(M1_EXPECTED_SHA)

m1-release-gate: m1-sdk-setup
	test -n "$(M1_DEPLOYMENT_PATH)" || { echo 'M1_DEPLOYMENT_PATH must be the intended absolute accounting root' >&2; exit 2; }
	M1_DEPLOYMENT_PATH="$(M1_DEPLOYMENT_PATH)" PYTHONOPTIMIZE=1 $(M1_PYTHON) -B scripts/m1_release_gate.py --headless --expected-sha $(M1_EXPECTED_SHA) --output target/m1-release-gate.json

clean:
	$(CARGO) clean --locked
