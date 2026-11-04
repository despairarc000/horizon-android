#!/usr/bin/env python3
"""Bounded real integrated-world acceptance test for offline Horizon bots.

Only declares success if the server actually logs each bot into the same
private world AND emits its first authoritative loaded-world survey.
Optionally samples a bounded period after both bots become loaded, to report
real harvesting attempts or pickups. This does not prove long-term survival
or invent test resources.
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


TERRAIN_SAMPLE = re.compile(
    r"Offline ecosystem (EcoAI[01]): "
    r"(?:first authoritative survey at|exploration survey position) "
    r"(-?\d+\.\d+),(-?\d+\.\d+),(-?\d+\.\d+)"
    r".*?terrain_down=(\d+) terrain_up=(\d+) current_ground=(true|false)"
)


def terrain_report(lines: list[str]) -> dict[str,dict[str,int|float|bool]]:
    observations: dict[str,list[tuple[float,float,float,int,int,bool]]] = {
        name:[] for name in BOT_NAMES
    }
    for line in lines:
        match=TERRAIN_SAMPLE.search(line)
        if match:
            name,x,y,z,lower,higher,standing=match.groups()
            observations[name].append((
                float(x),float(y),float(z),int(lower),int(higher),
                standing=="true"
            ))
    result={}
    for name,samples in observations.items():
        if not samples:
            result[name]={"samples":0,"horizontal_displacement":0.0}
            continue
        first=samples[0]
        last=samples[-1]
        # Only published server position samples count. A bot could have
        # moved between samples and returned to spawn; never claim otherwise.
        xz_distance=lambda a,b:((a[0]-b[0])**2+(a[2]-b[2])**2)**0.5
        result[name]={
            "samples":len(samples),
            "horizontal_displacement":round(xz_distance(first,last),2),
            "max_sampled_excursion":round(
                max(xz_distance(first,point) for point in samples),2
            ),
            "sampled_path_length":round(
                sum(xz_distance(a,b) for a,b in zip(samples,samples[1:])),2
            ),
            "min_sampled_y":min(point[1] for point in samples),
            "max_sampled_y":max(point[1] for point in samples),
            "first_lower_stands":first[3],
            "first_higher_stands":first[4],
            "initial_column_standable":first[5],
        }
    return result


def confirm_observed_exploration(
    lines: list[str], min_bots: int = 1, excursion_blocks: float = 1.0
) -> tuple[bool, str]:
    """Require multiple server position samples, not attempted packets."""
    if min_bots < 0 or min_bots > len(BOT_NAMES) or excursion_blocks < 0:
        raise ValueError("invalid observed exploration threshold")
    report = terrain_report(lines)
    movers = [
        name for name in BOT_NAMES
        if report[name].get("samples", 0) >= 2
        and report[name].get("max_sampled_excursion", 0.0) >= excursion_blocks
    ]
    return len(movers) >= min_bots, (
        f"{len(movers)}/{len(BOT_NAMES)} bots moved at least "
        f"{excursion_blocks:g} blocks in published server samples: {movers}"
    )


STEERING_SAMPLE = re.compile(
    r"Offline ecosystem (EcoAI[01]): steering packets_sent=(\d+) "
    r"waypoints_reached=(\d+) rejected=(\d+) last_error=(.*)"
)


def steering_report(lines: list[str]) -> dict[str, dict[str,int|str|bool]]:
    # Packet counts are diagnostics, NOT acknowledged movement.
    latest: dict[str,dict[str,int|str|bool]] = {
        name: {"reported": False} for name in BOT_NAMES
    }
    for line in lines:
        if match := STEERING_SAMPLE.search(line):
            name,sent,reached,rejected,error=match.groups()
            latest[name]={
                "reported": True,
                "packets_sent": int(sent),
                "waypoints_reached": int(reached),
                "rejected": int(rejected),
                "last_error": error.strip(),
            }
    return latest


MINING_START = re.compile(
    r"Offline ecosystem (EcoAI[01]): server mining START "
    r"target=\[[^\]]+\] in_reach=(true|false) "
    r"spawn_protected=(true|false) inside_border=(true|false) spectator=(true|false)"
)
MINING_STOP = re.compile(
    r"Offline ecosystem (EcoAI[01]): server mining STOP "
    r".*?decision=(Break|Gone|Other|Delay\(\d+\)) "
)


def server_mining_report(lines: list[str]) -> dict[str,int]:
    """Log observations, not proof of a successful block destruction."""
    result = {
        "server_starts_observed": 0,
        "starts_out_of_reach": 0,
        "starts_spawn_protected": 0,
        "starts_outside_border": 0,
        "stops_observed": 0,
        "stops_break": 0,
        "stops_delay": 0,
        "stops_other": 0,
        "stops_gone": 0,
    }
    for line in lines:
        if start := MINING_START.search(line):
            _, reach, spawn, border, _ = start.groups()
            result["server_starts_observed"] += 1
            result["starts_out_of_reach"] += int(reach == "false")
            result["starts_spawn_protected"] += int(spawn == "true")
            result["starts_outside_border"] += int(border == "false")
        if stop := MINING_STOP.search(line):
            result["stops_observed"] += 1
            kind = stop.group(2)
            key = "stops_delay" if kind.startswith("Delay") else "stops_" + kind.lower()
            result[key] += 1
    return result


DESTROY_RESULT = re.compile(
    r"Offline ecosystem (EcoAI[01]): server destroy result=(.*?) "
    r"target=\[[^\]]+\] world_changed=(true|false) "
    r"after_block=([^\s]+) after_air=(true|false)"
)


def server_destroy_report(lines: list[str]) -> dict[str, int]:
    """Only an observed state mutation can establish block removal."""
    result = {
        "destroy_invocations": 0,
        "world_mutations_observed": 0,
        "world_unchanged": 0,
        "actor_unavailable": 0,
        "returned_errors": 0,
        "became_air": 0,
    }
    for line in lines:
        if not (match := DESTROY_RESULT.search(line)):
            continue
        _, outcome, changed, _, air = match.groups()
        result["destroy_invocations"] += 1
        result["world_mutations_observed"] += int(changed == "true")
        result["world_unchanged"] += int(changed == "false")
        result["actor_unavailable"] += int(outcome == "actor-unavailable")
        result["returned_errors"] += int(outcome.startswith("error:"))
        result["became_air"] += int(air == "true")
    return result


SERVER_ITEM_DROPS = re.compile(
    r"Offline ecosystem (EcoAI[01]): server destroy drop_entities=(\d+) sample=(.*)"
)
ITEM_PICKUP_GEOMETRY = re.compile(
    r"Offline ecosystem (EcoAI[01]): item pickup geometry "
    r"local_drops=(\d+) horizontal_near=(\d+) "
    r"actually_touchable=(\d+) sampled_y=(.*)"
)


def item_pickup_report(lines: list[str]) -> dict[str, int]:
    # Spatial eligibility is not a successful give() or inventory gain.
    result = {
        "new_server_drop_entities": 0,
        "sampled_pickup_checks": 0,
        "max_local_drop_entities": 0,
        "max_horizontally_near": 0,
        "max_touchable": 0,
    }
    for line in lines:
        if match := SERVER_ITEM_DROPS.search(line):
            result["new_server_drop_entities"] += int(match.group(2))
        if match := ITEM_PICKUP_GEOMETRY.search(line):
            result["sampled_pickup_checks"] += 1
            result["max_local_drop_entities"] = max(
                result["max_local_drop_entities"], int(match.group(2))
            )
            result["max_horizontally_near"] = max(
                result["max_horizontally_near"], int(match.group(3))
            )
            result["max_touchable"] = max(
                result["max_touchable"], int(match.group(4))
            )
    return result


DOWNHILL_ATTEMPT = re.compile(
    r"Offline ecosystem (EcoAI[01]): "
    r"(?:attempting normal downhill movement to|exploring downhill toward) verified surface"
)
DOWNHILL_CONFIRMED = re.compile(
    r"Offline ecosystem (EcoAI[01]): server-authoritative downhill landing"
)
DOWNHILL_UNCONFIRMED = re.compile(
    r"Offline ecosystem (EcoAI[01]): downhill motion unconfirmed"
)
DOWNHILL_REJECTED = re.compile(
    r"Offline ecosystem (EcoAI[01]): downhill packet rejected"
)


VERTICAL_LANDING = re.compile(
    r"Offline ecosystem (EcoAI[01]): server-authoritative "
    r"(?:uphill|downhill) landing at \\[(-?\\d+),\\s*(-?\\d+),\\s*(-?\\d+)\\]"
)


def vertical_cycle_report(lines: list[str]) -> dict[str, dict[str, int]]:
    """Detect A-B-A-B real landing cycles, not merely requested jumps.

    An eight-landing alternation in the same two cells is a repeat-loop
    regression, not meaningful exploration. Other physical paths remain
    accepted; all reports use only server-confirmed landings.
    """
    landings: dict[str, list[tuple[int, int, int]]] = {
        bot: [] for bot in BOT_NAMES
    }
    for line in lines:
        if match := VERTICAL_LANDING.search(line):
            bot, x, y, z = match.groups()
            landings[bot].append((int(x), int(y), int(z)))
    result = {}
    for bot, path in landings.items():
        current = best = 0
        for i in range(2, len(path)):
            if path[i] == path[i - 2] and path[i] != path[i - 1]:
                current = current + 1 if current else 3
                best = max(best, current)
            else:
                current = 0
        result[bot] = {
            "server_confirmed_vertical_landings": len(path),
            "longest_two_surface_alternation": best,
        }
    return result


def downhill_report(lines: list[str]) -> dict[str,int]:
    """Only the authoritative landing trace counts as a successful descent."""
    result = {
        "downhill_attempts": 0,
        "server_confirmed_landings": 0,
        "unconfirmed_timeouts": 0,
        "rejected_packets": 0,
    }
    for line in lines:
        result["downhill_attempts"] += bool(DOWNHILL_ATTEMPT.search(line))
        result["server_confirmed_landings"] += bool(DOWNHILL_CONFIRMED.search(line))
        result["unconfirmed_timeouts"] += bool(DOWNHILL_UNCONFIRMED.search(line))
        result["rejected_packets"] += bool(DOWNHILL_REJECTED.search(line))
    return result


UPHILL_ATTEMPT = re.compile(
    r"Offline ecosystem (EcoAI[01]): "
    r"(?:attempting normal uphill jump to|approaching real trunk uphill toward|exploring uphill toward) "
    r"verified surface"
)
UPHILL_LANDING = re.compile(
    r"Offline ecosystem (EcoAI[01]): server-authoritative uphill landing"
)
UPHILL_TIMEOUT = re.compile(
    r"Offline ecosystem (EcoAI[01]): uphill motion unconfirmed"
)
UPHILL_REJECTED = re.compile(
    r"Offline ecosystem (EcoAI[01]): uphill packet rejected"
)
UPHILL_INVALID_ARC = re.compile(
    r"Offline ecosystem (EcoAI[01]): uphill arc invalid before verified landing"
)


def uphill_report(lines: list[str]) -> dict[str, int]:
    """Transport attempts never substitute for server-confirmed landing."""
    return {
        "uphill_attempts": sum(bool(UPHILL_ATTEMPT.search(x)) for x in lines),
        "server_confirmed_uphill_landings": sum(
            bool(UPHILL_LANDING.search(x)) for x in lines
        ),
        "unconfirmed_uphill_timeouts": sum(bool(UPHILL_TIMEOUT.search(x)) for x in lines),
        "rejected_uphill_packets": sum(bool(UPHILL_REJECTED.search(x)) for x in lines),
        "invalid_uphill_arcs": sum(bool(UPHILL_INVALID_ARC.search(x)) for x in lines),
    }


BLOCKED_PICKUP_RELEASE = re.compile(
    r"Offline ecosystem (EcoAI[01]): observed drop still inaccessible "
    r"after 8 server surveys; no pickup or inventory credit"
)


def blocked_pickup_report(lines: list[str]) -> dict[str, int]:
    """Abandoning unreachable loot is recovery, NEVER a resource gain."""
    return {
        "blocked_drop_releases": sum(
            bool(BLOCKED_PICKUP_RELEASE.search(line)) for line in lines
        ),
    }


ACCESS_CLASSIFICATION = re.compile(
    r"Offline ecosystem (EcoAI[01]): mining attempt started at "
    r"\[[^\]]+\] access_rank=(\d+)"
)


def harvest_access_report(lines: list[str]) -> dict[str, int]:
    """Only server-observed verified/clearable access may start mining.

    Access rank is pre-harvest *planning* evidence, never a completed
    server pickup. This detects unguarded mining regressions on live runs.
    """
    result = {
        "attempts": 0,
        "connected_pickup_stand": 0,
        "clearing_opens_stand": 0,
        "mined_without_known_access": 0,
        "unclassified_attempts": 0,
    }
    for line in lines:
        if "mining attempt started at" not in line:
            continue
        result["attempts"] += 1
        match = ACCESS_CLASSIFICATION.search(line)
        if match is None:
            result["unclassified_attempts"] += 1
        else:
            rank = int(match.group(2))
            key = {
                0: "connected_pickup_stand",
                1: "clearing_opens_stand",
            }.get(rank, "mined_without_known_access")
            result[key] += 1
    return result


def harvest_report(lines: list[str]) -> dict[str,int]:
    return {
        "mining_attempts":sum("mining attempt started at" in line for line in lines),
        "exact_blocks_removed":sum("exact server block observation confirmed" in line for line in lines),
        "correlated_wood_pickups":sum("correlated log removal and real inventory gain" in line for line in lines),
        "unverified_harvests":sum("mining unverified" in line for line in lines),
    }


def run(binary: pathlib.Path, resources: pathlib.Path, scratch: pathlib.Path,
        timeout_seconds: float, post_ready_seconds: float = 0,
        min_bots: int = 0, excursion_blocks: float = 1.0) -> int:
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
        ready_at: float | None = None
        explanation = "deadline exceeded"
        held_for_requested_period = post_ready_seconds == 0
        try:
            while time.monotonic() - started < timeout_seconds:
                if ready_at is not None and time.monotonic() - ready_at >= post_ready_seconds:
                    held_for_requested_period = True
                    break
                try:
                    lines.append(received.get(timeout=0.25))
                except queue.Empty:
                    if process.poll() is not None:
                        break
                    continue
                reached, explanation = check_events(lines)
                if "not_whitelisted" in explanation:
                    break
                if reached and ready_at is None:
                    ready_at = time.monotonic()
            # Drain all already-received lines for accurate failure reporting.
            while not received.empty():
                lines.append(received.get_nowait())
            reached, explanation = check_events(lines)
            if ready_at is not None and time.monotonic()-ready_at >= post_ready_seconds:
                held_for_requested_period = True
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
    print(f"Terrain elevation and authoritative movement: {terrain_report(lines)}")
    print(f"Bot steering transport diagnostics (not confirmed motion): {steering_report(lines)}")
    print(f"Server-side survival mining decisions: {server_mining_report(lines)}")
    print(f"Actual server destruction outcomes: {server_destroy_report(lines)}")
    print(f"Survival downhill movement evidence: {downhill_report(lines)}")
    print(f"Survival uphill movement evidence: {uphill_report(lines)}")
    cycles=vertical_cycle_report(lines)
    print(f"Repeated authoritative vertical landing cycles: {cycles}")
    loop_ok=all(v["longest_two_surface_alternation"]<8 for v in cycles.values())
    if not loop_ok:
        print("ERROR: repeated confirmed landings show an eight-step two-surface exploration cycle")
    print(f"Physical drops and pickup geometry (not confirmed inventory): {item_pickup_report(lines)}")
    print(f"Inaccessible dropped item recovery (no pickup): {blocked_pickup_report(lines)}")
    print(f"Additional real-world harvesting observations: {harvest_report(lines)}")
    access=harvest_access_report(lines)
    print(f"Preharvest resource-access classification (not pickup proof): {access}")
    access_ok=access["mined_without_known_access"]==0 and access["unclassified_attempts"]==0
    if not access_ok:
        print("ERROR: mining proceeded without a verified or provisional access route")
    if not reached or not held_for_requested_period:
        print(f"Headless process exit: {process.returncode}; log: {console}")
    movement_ok, movement_note = confirm_observed_exploration(
        lines, min_bots, excursion_blocks
    )
    print(f"Authoritative movement acceptance: {movement_note}")
    return 0 if reached and held_for_requested_period and movement_ok and access_ok and loop_ok else 1


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=pathlib.Path, required=True)
    parser.add_argument("--resources", type=pathlib.Path, required=True)
    parser.add_argument("--scratch", type=pathlib.Path, required=True)
    parser.add_argument("--timeout-seconds", type=float, default=140)
    parser.add_argument("--post-ready-seconds", type=float, default=0)
    parser.add_argument("--min-moving-bots", type=int, default=0)
    parser.add_argument("--min-excursion-blocks", type=float, default=1.0)
    args = parser.parse_args()
    if not args.binary.is_file() or not args.resources.is_dir():
        parser.error("compiled game binary and prepared resources are required")
    if not (1 <= args.timeout_seconds <= 300):
        parser.error("timeout must be between 1 and 300 seconds")
    if not (0 <= args.post_ready_seconds <= 120):
        parser.error("post-ready period must be between 0 and 120 seconds")
    if args.post_ready_seconds >= args.timeout_seconds:
        parser.error("post-ready period must be shorter than deadline")
    if not (0 <= args.min_moving_bots <= len(BOT_NAMES)):
        parser.error("min-moving-bots must be between 0 and two")
    if not (0 < args.min_excursion_blocks <= 30):
        parser.error("min-excursion-blocks must be between 0 and 30")
    return run(args.binary, args.resources, args.scratch,
               args.timeout_seconds, args.post_ready_seconds,
               args.min_moving_bots, args.min_excursion_blocks)


if __name__ == "__main__":
    sys.exit(main())
