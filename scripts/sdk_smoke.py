#!/usr/bin/env python3
"""Exercise official Python SDKs against Steve and its local fixture."""

import argparse
import importlib.metadata
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ANTHROPIC_SDK_VERSION = "1.2.0"
OPENAI_SDK_VERSION = "3.6.0"
READY_TIMEOUT_SECONDS = 15


def require(condition, message):
    """Keep gate evidence active when Python optimization is enabled."""
    if not condition:
        raise RuntimeError(message)


def log_tail(log_path):
    try:
        return "\n".join(log_path.read_text(encoding="utf-8").splitlines()[-20:])[-4000:]
    except OSError:
        return "(log unavailable)"


def clean_environment():
    env = os.environ.copy()
    for key in list(env):
        upper = key.upper()
        if (
            key.startswith(("STEVE_", "RUST_LOG"))
            or upper.startswith(("ANTHROPIC_", "OPENAI_", "AWS_"))
            or upper in {"HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"}
        ):
            env.pop(key)
    return env


def ready_address(child, log_path, event, address_field):
    deadline = time.monotonic() + READY_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError(
                f"{event} process exited with status {child.returncode}:\n{log_tail(log_path)}"
            )
        with log_path.open(encoding="utf-8") as log:
            for line in log:
                try:
                    record = json.loads(line)
                except json.JSONDecodeError:
                    continue
                fields = record.get("fields", {})
                if fields.get("event") == event and fields.get(address_field):
                    return fields[address_field]
        time.sleep(0.05)
    raise TimeoutError(f"timed out waiting for {event}:\n{log_tail(log_path)}")


def stop(child):
    if child is None:
        return
    if child.poll() is None:
        child.terminate()
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired as error:
                raise RuntimeError("failed to reap SDK smoke process after kill") from error


def check_sdk(provider):
    package, version = {
        "anthropic": ("anthropic", ANTHROPIC_SDK_VERSION),
        "openai": ("openai", OPENAI_SDK_VERSION),
    }[provider]
    try:
        __import__(package)
    except ImportError as error:
        raise RuntimeError(
            f"{provider} SDK dependencies are missing; install tests/requirements-sdk.txt ({error})"
        ) from error
    installed_version = importlib.metadata.version(package)
    if installed_version != version:
        raise RuntimeError(f"{package}=={version} is required, found {installed_version}")


def anthropic_smoke(inference_addr, server_log, fixture_log):
    import anthropic

    try:
        with anthropic.DefaultHttpxClient(trust_env=False, timeout=10) as http_client:
            client = anthropic.Anthropic(
                api_key="dummy-local-test-key",
                base_url=f"http://{inference_addr}",
                max_retries=0,
                timeout=10,
                http_client=http_client,
            )
            with client.messages.stream(
                model="steve-test-model",
                max_tokens=16,
                messages=[{"role": "user", "content": "hi"}],
            ) as stream:
                streamed_text = "".join(stream.text_stream)
                final_message = stream.get_final_message()
            require(streamed_text == "steve-test-response", streamed_text)
            require(final_message.stop_reason == "end_turn", final_message.stop_reason)
            final_text = [block.text for block in final_message.content if block.type == "text"]
            require(final_text == ["steve-test-response"], final_message.content)
            client.close()
    except Exception as error:
        raise RuntimeError(
            f"Anthropic SDK smoke failed: {error}\n"
            f"Steve logs:\n{log_tail(server_log)}\n"
            f"Fixture logs:\n{log_tail(fixture_log)}"
        ) from error


def openai_smoke(inference_addr, server_log, fixture_log):
    from openai import DefaultHttpxClient, OpenAI

    try:
        with DefaultHttpxClient(trust_env=False, timeout=10) as http_client:
            client = OpenAI(
                api_key="dummy-local-test-key",
                base_url=f"http://{inference_addr}/v1",
                max_retries=0,
                timeout=10,
                http_client=http_client,
            )
            with client.chat.completions.stream(
                model="steve-test-model",
                max_tokens=16,
                messages=[{"role": "user", "content": "hi"}],
            ) as stream:
                streamed_text = "".join(
                    event.delta for event in stream if event.type == "content.delta"
                )
                completion = stream.get_final_completion()
            require(streamed_text == "steve-test-response", streamed_text)
            require(completion.choices[0].finish_reason == "stop", completion.choices)
            require(
                completion.choices[0].message.content == "steve-test-response",
                completion.choices[0].message.content,
            )

            with client.responses.stream(
                model="steve-test-model",
                max_output_tokens=16,
                input="hi",
            ) as stream:
                streamed_text = ""
                lifecycle = []
                in_progress = None
                for event in stream:
                    if event.type.startswith("response.") and event.type != "response.output_text.delta":
                        lifecycle.append(event.type)
                    if event.type == "response.in_progress":
                        in_progress = event.response
                    elif event.type == "response.output_text.delta":
                        streamed_text += event.delta
                response = stream.get_final_response()
            require(
                lifecycle
                == [
                    "response.created",
                    "response.in_progress",
                    "response.output_item.added",
                    "response.content_part.added",
                    "response.output_text.done",
                    "response.content_part.done",
                    "response.output_item.done",
                    "response.completed",
                ],
                lifecycle,
            )
            require(in_progress is not None, "response.in_progress event missing")
            require(in_progress.status == "in_progress", in_progress)
            require(in_progress.id == response.id, (in_progress.id, response.id))
            require(streamed_text == "steve-test-response", streamed_text)
            require(response.status == "completed", response.status)
            require(response.output_text == "steve-test-response", response.output_text)
            client.close()
    except Exception as error:
        raise RuntimeError(
            f"OpenAI SDK smoke failed: {error}\n"
            f"Steve logs:\n{log_tail(server_log)}\n"
            f"Fixture logs:\n{log_tail(fixture_log)}"
        ) from error


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/steve"))
    parser.add_argument(
        "--provider", choices=("anthropic", "openai", "all"), default="anthropic"
    )
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"Steve binary not found: {binary} (build it with cargo build first)")

    providers = ("anthropic", "openai") if args.provider == "all" else (args.provider,)
    try:
        for provider in providers:
            check_sdk(provider)
    except RuntimeError as error:
        parser.error(str(error))

    env = clean_environment()
    for key in set(os.environ) - env.keys():
        os.environ.pop(key)
    with tempfile.TemporaryDirectory(prefix="steve-sdk-smoke-") as temp:
        root = Path(temp)
        fixture_config = root / "fixture.toml"
        fixture_config.write_text('[logging]\nlevel = "info"\njson = true\n', encoding="utf-8")
        fixture_log = root / "fixture.log"
        fixture_child = server_child = None
        try:
            with fixture_log.open("w", encoding="utf-8") as output:
                fixture_child = subprocess.Popen(
                    [str(binary), "--config", str(fixture_config), "test-upstream", "--listen", "127.0.0.1:0"],
                    stdout=output,
                    stderr=subprocess.STDOUT,
                    env=env,
                )
            fixture_addr = ready_address(fixture_child, fixture_log, "test_upstream_ready", "local_addr")

            config = root / "steve.toml"
            db_path = root / "steve.db"
            objects = root / "objects"
            journal = root / "accounting-overflow.jsonl"
            config.write_text(
                "\n".join(
                    [
                        "[server]",
                        'inference_bind = "127.0.0.1:0"',
                        'management_bind = "127.0.0.1:0"',
                        f'openai_upstream_url = "http://{fixture_addr}"',
                        f'anthropic_upstream_url = "http://{fixture_addr}"',
                        "",
                        "[database]",
                        f"url = {json.dumps(f'sqlite://{db_path}?mode=rwc')}",
                        "",
                        "[object_storage]",
                        'kind = "fs"',
                        f'root = {json.dumps(str(objects))}',
                        "",
                        "[queues]",
                        f'accounting_journal = {json.dumps(str(journal))}',
                        "",
                        "[logging]",
                        'level = "info"',
                        "json = true",
                        "",
                    ]
                ),
                encoding="utf-8",
            )
            server_log = root / "steve.log"
            with server_log.open("w", encoding="utf-8") as output:
                server_child = subprocess.Popen(
                    [str(binary), "--config", str(config), "serve"],
                    stdout=output,
                    stderr=subprocess.STDOUT,
                    env=env,
                )
            inference_addr = ready_address(server_child, server_log, "listeners_ready", "inference")

            for provider in providers:
                if provider == "anthropic":
                    anthropic_smoke(inference_addr, server_log, fixture_log)
                else:
                    openai_smoke(inference_addr, server_log, fixture_log)
        finally:
            try:
                stop(server_child)
            finally:
                stop(fixture_child)
    print(f"{', '.join(provider.title() for provider in providers)} SDK smoke passed")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, TimeoutError) as error:
        print(f"SDK smoke failed: {error}", file=sys.stderr)
        sys.exit(1)
