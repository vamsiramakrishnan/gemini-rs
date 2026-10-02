#!/usr/bin/env python3
"""Verify authored gallery examples, packaged copies, and optional Studio APIs."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import urllib.request


ROOT = Path(__file__).resolve().parents[1]
GALLERY = ROOT / "apps/gemini-adk-web-rs/static/examples/flows"
PACKAGED = ROOT / "tools/gemini-adk-cli-rs/assets/web/examples/flows"


def post(base, endpoint, body):
    request = urllib.request.Request(
        f"{base.rstrip('/')}/api/flows/{endpoint}",
        data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        return json.load(response)


def preserves(authored, exported):
    """Retain authored values; serde omits empty optional descriptions."""
    if isinstance(authored, dict):
        return isinstance(exported, dict) and all(
            (key == "description" and value == "" and key not in exported)
            or (key in exported and preserves(value, exported[key]))
            for key, value in authored.items()
        )
    if isinstance(authored, list):
        return isinstance(exported, list) and len(authored) == len(exported) and all(
            preserves(a, b) for a, b in zip(authored, exported)
        )
    return authored == exported


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    target = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
    parser.add_argument("--adk", type=Path, default=target / "debug/adk")
    parser.add_argument("--base-url", help="Also exercise a running Studio server")
    parser.add_argument("--output", type=Path, default=ROOT / "target/use-case-expansion/verification.json")
    args = parser.parse_args()
    adk = args.adk.resolve()
    if not adk.is_file():
        parser.error("Build gemini-adk-cli-rs first, or pass --adk /path/to/adk")

    for source in sorted(GALLERY.rglob("*.json")):
        mirror = PACKAGED / source.relative_to(GALLERY)
        assert mirror.is_file() and source.read_bytes() == mirror.read_bytes(), (
            f"Packaged asset drift: {source.relative_to(GALLERY)}. Run just sync-web-assets."
        )
    assert {p.relative_to(GALLERY) for p in GALLERY.rglob("*.json")} == {
        p.relative_to(PACKAGED) for p in PACKAGED.rglob("*.json")
    }, "Packaged examples contain extra files"

    references = json.loads((GALLERY / "reference/index.json").read_text())
    for reference in references["files"]:
        raw = (GALLERY / "reference" / reference["file"]).read_bytes()
        assert hashlib.sha256(raw).hexdigest() == reference["sha256"], reference["file"]
        assert len(json.loads(raw)["tests"]) == reference["workflow_tests"]

    entries = json.loads((GALLERY / "index.json").read_text())["examples"]
    results = []
    for entry in entries:
        source = GALLERY / entry["file"]
        spec = json.loads(source.read_text())
        run = subprocess.run([str(adk), "spec", "test", str(source)], cwd=ROOT,
                             text=True, capture_output=True, timeout=120)
        assert run.returncode == 0, f"{entry['file']}:\n{run.stdout}\n{run.stderr}"
        counts = re.search(r"(\d+) passed, (\d+) failed", run.stdout)
        assert counts and counts[2] == "0", run.stdout
        result = {"file": entry["file"], "checks_passed": int(counts[1]),
                  "skills": len(spec.get("skills", [])),
                  "tools": sum(len(skill.get("tools", [])) for skill in spec.get("skills", [])),
                  "task_scenarios": len(spec.get("task_scenarios", []))}
        if args.base_url:
            validation = post(args.base_url, "validate", spec)
            assert validation["valid"], (entry["file"], validation)
            if spec.get("skills"):
                assert not validation["warnings"], (entry["file"], validation)
            reports = post(args.base_url, "test", spec)
            assert reports["valid"], (entry["file"], reports)
            combined = reports["reports"] + reports["scenarios"]
            assert combined and all(report["passed"] for report in combined), reports
            project = post(args.base_url, "project", {"spec": spec, "lang": "rust"})
            assert project["valid"], project
            document = next(file for file in project["files"] if file["path"] == "agent.json")
            exported = json.loads(document["contents"])
            assert preserves(spec, exported), f"{entry['file']} export lost authored fields"
            rerun = post(args.base_url, "test", exported)
            assert rerun["valid"] and all(report["passed"] for report in rerun["reports"] + rerun["scenarios"]), rerun
            result["http_and_export"] = "passed"
        results.append(result)
        print(f"{entry['file']}: {result['checks_passed']} checks passed", flush=True)

    report = {"examples": results, "reference_workflow_tests": sum(r["workflow_tests"] for r in references["files"]),
              "provider_calls": False, "external_effects": "controlled mocks only",
              "original_features": "Task-owned extraction, computed fields, watchers, patterns and memory; replay uses controlled service providers. Original workflow references are preserved."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"Verified {len(results)} gallery entries; report: {args.output}")


if __name__ == "__main__":
    main()
