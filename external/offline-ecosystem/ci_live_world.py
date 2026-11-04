#!/usr/bin/env python3
"""Bounded real integrated-world acceptance test for offline Horizon bots.

Only declares success if the server actually logs each bot into the same
private world AND emits its first authoritative loaded-world survey.
Stops the controlled test process after that milestone; this does not test
autosave, Android rendering, harvesting, or long-term survival.
"""
from __future__ import annotations

import argparse
import pathlib
import queue
import re
import subprocess
import threading
import time
import os
import sys

BOT_NAMES = ("EcoAI0", "EcoAI1")
SERVER_LOGIN = re.compile(r"\b(EcoAI[01])\[/[^\]]+\]\s+logged in with entity id (\d+)")
OBSERVED = re.compile(r"Offline ecosystem (EcoAI[01]): first authoritative survey")


def check_events(lines: list[str]) -> tuple[bool, str]:
    logged: dict[str, str] = {}
    surveyed: set[str] = set()
    started = False
    for line in lines:
        if "Offline ecosystem: started 2 opt-in local bots" in line:
            started = True
        match = SERVER_LOGIN.search(line)
        if match:
            logged[match.group(1)] = match.group(2)
        seen = OBSERVED.search(line)
        if seen:
            surveyed.add(seen.group(1))
        if "not_whitelisted" in line:
            return False, "bot rejected by private-world admission gate"
    if len(set(logged.values())) != len(logged):
        return False, "bot entity IDs collided"
    absent_login = [name for name in BOT_NAMES if name not in logged]
    absent_observations = [name for name in BOT_NAMES if name not in surveyed]
    if started and not absent_login and not absent_observations:
        return True, "two distinct local bots entered the real world and received server surveys"
    return False, (
        f"startup={started}, missing logins={absent_login}, "
        f"missing loaded-world surveys={absent_observations}"
    )


def run(binary: pathlib.Path, resources: pathlib.Path, scratch: pathlib.Path,
        timeout_seconds: float) -> int:
    scratch.mkdir(parents=True, exist_ok=True)
    console = scratch / "console.log"
    command = [
        str(binary.resolve()), "--resources", str(resources.resolve()),
        "--headless", "--world", "EcoWorld", "--seed", "42",
        "--frames", "1000000", "--timeout-seconds", "240",
        "--render-distance", "2", "--name", "EcoHost", "--no-pause",
        "--game-dir", str(scratch.resolve()),
        "--home", str(scratch.resolve()),
    ]
    env = dict(os.environ, HORIZON_ECOSYSTEM_EXPERIMENTAL_BOTS="2")
    started = time.monotonic()
    received: queue.Queue[str] = queue.Queue()
    lines: list[str] = []
    with console.open("w", encoding="utf-8") as log:
        process = subprocess.Popen(
            command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace", bufsize=1, env=env,
        )
        assert process.stdout is not None

        def capture() -> None:
            for line in process.stdout:
                log.write(line)
                log.flush()
                received.put(line)
                # Keep only salient progress on the CI console. The complete
                # trace remains local and is never published as an artifact.
                if any(word in line for word in (
                    "Offline ecosystem", "logged in with entity id",
                    "Horizon: frame-limited run timed out", "Error",
                )):
                    print(line, end="", flush=True)
        reader = threading.Thread(target=capture, daemon=True)
        reader.start()

        reached = False
        explanation = "deadline exceeded"
        try:
            while time.monotonic() - started < timeout_seconds:
                try:
                    lines.append(received.get(timeout=0.25))
                except queue.Empty:
                    if process.poll() is not None:
                        break
                    continue
                reached, explanation = check_events(lines)
                if reached or "not_whitelisted" in explanation:
                    break
            # Drain all already-received lines for accurate failure reporting.
            while not received.empty():
                lines.append(received.get_nowait())
            reached, explanation = check_events(lines)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=8)
            reader.join(timeout=2)

    print(f"Headless integrated-world acceptance: {explanation}")
    print(f"Observed {len(lines)} console lines in {time.monotonic()-started:.1f}s")
    if not reached:
        print(f"Headless process exit: {process.returncode}; log: {console}")
    return 0 if reached else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--resources", type=pathlib.Path, required=True)
    parser.add_argument("--scratch", type=pathlib.Path, required=True)
    parser.add_argument("--timeout-seconds", type=float, default=140)
    args = parser.parse_args()
    if not args.binary.is_file() or not args.resources.is_dir():
        parser.error("compiled game binary and prepared resources are required")
    if not (1 <= args.timeout_seconds <= 300):
        parser.error("timeout must be between 1 and 300 seconds")
    return run(args.binary, args.resources, args.scratch, args.timeout_seconds)


if __name__ == "__main__":
    sys.exit(main())
