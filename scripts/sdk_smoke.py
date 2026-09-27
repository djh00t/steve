#!/usr/bin/env python3
"""Exercise the Anthropic Python SDK against Steve and its local fixture."""

import argparse
import importlib.metadata
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

SDK_VERSION = "1.2.0"
READY_TIMEOUT_SECONDS = 15


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
    if child is None or child.poll() is not None:
        return
    child.terminate()
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/steve"))
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"Steve binary not found: {binary} (build it with cargo build first)")

    try:
        import anthropic
    except ImportError as error:
        parser.error(f"SDK dependencies are missing; install tests/requirements-sdk.txt ({error})")
    installed_version = importlib.metadata.version("anthropic")
    if installed_version != SDK_VERSION:
        parser.error(f"anthropic=={SDK_VERSION} is required, found {installed_version}")

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
                    assert streamed_text == "steve-test-response", streamed_text
                    assert final_message.stop_reason == "end_turn", final_message.stop_reason
                    final_text = [
                        block.text for block in final_message.content if block.type == "text"
                    ]
                    assert final_text == ["steve-test-response"], final_message.content
                    client.close()
            except Exception as error:
                raise RuntimeError(
                    f"Anthropic SDK smoke failed: {error}\n"
                    f"Steve logs:\n{log_tail(server_log)}\n"
                    f"Fixture logs:\n{log_tail(fixture_log)}"
                ) from error
        finally:
            try:
                stop(server_child)
            finally:
                stop(fixture_child)
    print("Anthropic SDK smoke passed")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, TimeoutError) as error:
        print(f"SDK smoke failed: {error}", file=sys.stderr)
        sys.exit(1)
