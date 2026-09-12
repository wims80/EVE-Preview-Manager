#!/usr/bin/env python3
"""Generate the bundled category-20 subset from an English CCP SDE type table."""
import argparse
import datetime
import hashlib
import json
import pathlib

parser = argparse.ArgumentParser()
parser.add_argument("source", type=pathlib.Path)
parser.add_argument("output", type=pathlib.Path)
parser.add_argument("--generation-date", required=True)
parser.add_argument("--source-sha256", required=True)
args = parser.parse_args()
source_bytes = args.source.read_bytes()
actual_hash = hashlib.sha256(source_bytes).hexdigest()
if actual_hash != args.source_sha256.lower():
    raise SystemExit(f"source hash mismatch: expected {args.source_sha256}, got {actual_hash}")
try:
    datetime.date.fromisoformat(args.generation_date)
except ValueError as error:
    raise SystemExit(f"invalid generation date: {error}")
data = json.loads(source_bytes)
types = []
seen = set()
for raw_id, record in data.items():
    if record.get("c") != 20:
        continue
    type_id = int(raw_id)
    if type_id <= 0 or type_id in seen or not isinstance(record.get("n"), str) or not record["n"].strip() or not isinstance(record.get("g"), int) or record["g"] <= 0:
        raise SystemExit(f"malformed category-20 record {raw_id}")
    seen.add(type_id)
    types.append({"type_id": type_id, "name": record["n"], "group_id": record["g"]})
if not types:
    raise SystemExit("source contained no category-20 types")
output = {
    "source": "CCP EVE Online Static Data Export (English), category 20",
    "source_sha256": actual_hash,
    "generated": args.generation_date,
    "attribution": "EVE Online and the EVE logo are registered trademarks of CCP hf. Data from the CCP Static Data Export.",
    "types": sorted(types, key=lambda item: item["type_id"]),
}
args.output.parent.mkdir(parents=True, exist_ok=True)
args.output.write_text(json.dumps(output, separators=(",", ":")) + "\n", encoding="utf-8")
