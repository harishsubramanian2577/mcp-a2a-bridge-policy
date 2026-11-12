#!/usr/bin/env python3
# Copyright 2026 Salesforce, Inc. All rights reserved.
"""Pre-publication check of the policy definition against Exchange's metadata rules.

`make build` does not show that Exchange will accept the metadata: an unknown asset type or an
over-long description is only rejected at publish time (HTTP 400). Run by `make publish` and
`make release` before anything is uploaded.

Usage: validate-definition.py [definition/gcl.yaml] [Cargo.toml]
"""
import re
import sys

# Exchange's allow-list for metadata/capabilities/assetTypes. The A2A v1 token is `a2av1`;
# `a2a_v1` is the API Manager endpoint spelling and is rejected by Exchange.
ALLOWED_ASSET_TYPES = {"http", "mcp", "a2a", "a2av1", "rest", "wsdl", "llm", "grpc", "websocket", "sse", "graphql"}
# The asset types this policy is built for: MCP instances (Mode 1) and A2A instances (Mode 2).
EXPECTED_ASSET_TYPES = ["mcp", "a2a", "a2av1"]
MAX_DESCRIPTION = 256


def labels(gcl_text):
    """The `metadata.labels` entries of a GCL file (single-line `key: value` scalars)."""
    block = re.search(r"^  labels:\n((?:    .*\n)+)", gcl_text, re.M)
    if not block:
        return {}
    pairs = (line.strip().split(":", 1) for line in block.group(1).splitlines() if ":" in line)
    return {key.strip(): value.strip().strip("'\"") for key, value in pairs}


def main(gcl_path="definition/gcl.yaml", cargo_path="Cargo.toml"):
    errors = []
    meta = labels(open(gcl_path).read())

    asset_types = [t.strip() for t in meta.get("metadata/capabilities/assetTypes", "").split(",") if t.strip()]
    unknown = [t for t in asset_types if t not in ALLOWED_ASSET_TYPES]
    if unknown:
        errors.append(f"assetTypes {unknown} not in Exchange's allow-list {sorted(ALLOWED_ASSET_TYPES)}")
    if len(set(asset_types)) != len(asset_types):
        errors.append(f"assetTypes has duplicates: {asset_types}")
    if asset_types != EXPECTED_ASSET_TYPES:
        errors.append(f"assetTypes is {asset_types}, expected {EXPECTED_ASSET_TYPES}")

    for name, description in [
        (f"{gcl_path} metadata.labels.description", meta.get("description", "")),
        (f"{cargo_path} description", (re.search(r'^description\s*=\s*"(.*)"', open(cargo_path).read(), re.M) or [None, ""])[1]),
    ]:
        if len(description) > MAX_DESCRIPTION:
            errors.append(f"{name} is {len(description)} characters; Exchange allows {MAX_DESCRIPTION}")
    if not meta.get("title"):
        errors.append(f"{gcl_path} has no metadata.labels.title")

    for error in errors:
        print(f"ERROR: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"Definition metadata OK: assetTypes={','.join(asset_types)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
