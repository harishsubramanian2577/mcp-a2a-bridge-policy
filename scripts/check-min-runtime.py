#!/usr/bin/env python3
"""Fail the build if the generated implementation metadata, or the playground
image, allows a Flex Gateway older than the enabled PDK features need.

PDK's build-asset-files always writes minRuntimeVersion 1.6.1, so with --write
the script first sets minRuntimeVersion in both metadata files to the declared
version, then checks them.

Usage: check-min-runtime.py [--write] <declared-min-runtime-version>
"""
import re
import sys
import tomllib
from pathlib import Path

# Lowest Flex Gateway version that supports each PDK feature this policy can enable.
# enable_stop_iteration: Flex 1.10.6 fails to load the policy with
# "FlexAbiHandler: command 'flex_enable_stop_iteration' unknown"; 1.11.0 loads it.
FEATURE_MIN_RUNTIME = {
    "enable_stop_iteration": "1.11.0",
}
BASELINE_MIN_RUNTIME = "1.6.1"

METADATA_FILES = [
    Path("target/implementation/metadata.yaml"),
    Path("target/implementation-dev/metadata.yaml"),
]
PLAYGROUND_COMPOSE = Path("playground/docker-compose.yaml")


def parse_version(text):
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", text.strip())
    if not match:
        raise ValueError(f"not a MAJOR.MINOR.PATCH version: {text!r}")
    return tuple(int(part) for part in match.groups())


def required_min_runtime():
    with open("Cargo.toml", "rb") as f:
        cargo = tomllib.load(f)
    pdk = cargo.get("dependencies", {}).get("pdk", {})
    features = pdk.get("features", []) if isinstance(pdk, dict) else []
    required, reason = BASELINE_MIN_RUNTIME, "PDK baseline"
    for feature in features:
        version = FEATURE_MIN_RUNTIME.get(feature)
        if version and parse_version(version) > parse_version(required):
            required, reason = version, f"pdk feature `{feature}`"
    return required, reason


def write_min_runtime(version):
    for path in METADATA_FILES:
        if not path.exists():
            continue
        text, count = re.subn(
            r"^minRuntimeVersion:.*$", f"minRuntimeVersion: {version}", path.read_text(), flags=re.M
        )
        if count == 0:
            text = text.rstrip("\n") + f"\nminRuntimeVersion: {version}\n"
        path.write_text(text)


def main():
    args = sys.argv[1:]
    write = args[:1] == ["--write"]
    if write:
        args = args[1:]
    if len(args) != 1:
        sys.exit(__doc__.strip())
    declared = args[0]
    parse_version(declared)
    if write:
        write_min_runtime(declared)
    required, reason = required_min_runtime()
    errors = []

    if parse_version(declared) < parse_version(required):
        errors.append(
            f"MIN_RUNTIME_VERSION {declared} is below {required}, required by {reason}"
        )

    for path in METADATA_FILES:
        if not path.exists():
            errors.append(f"{path} is missing; run `make build-asset-files`")
            continue
        match = re.search(r"^minRuntimeVersion:\s*(\S+)\s*$", path.read_text(), re.M)
        if not match:
            errors.append(f"{path} has no minRuntimeVersion")
        elif parse_version(match.group(1)) < parse_version(required):
            errors.append(
                f"{path} declares minRuntimeVersion {match.group(1)}, "
                f"but {reason} needs {required}"
            )

    match = re.search(r"image:\s*mulesoft/flex-gateway:(\S+)", PLAYGROUND_COMPOSE.read_text())
    if not match:
        errors.append(f"{PLAYGROUND_COMPOSE} has no mulesoft/flex-gateway image")
    elif parse_version(match.group(1)) < parse_version(required):
        errors.append(
            f"{PLAYGROUND_COMPOSE} uses flex-gateway:{match.group(1)}, "
            f"but {reason} needs {required}"
        )

    if errors:
        print("Runtime version check failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        sys.exit(1)
    print(f"Runtime version check passed: minRuntimeVersion {declared} (needs {required}, {reason})")


if __name__ == "__main__":
    main()
