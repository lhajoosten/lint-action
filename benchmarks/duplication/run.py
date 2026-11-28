#!/usr/bin/env python3
"""Measure real CLI invocations and compare their complete output with checked-in oracles."""

import argparse
import io
import json
import pathlib
import statistics
import subprocess
import sys
import tarfile
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
REVISION = "8c4ff5a195dc2b757760d59db1298e4358cbe378"


def execute(binary, args):
    start = time.perf_counter()
    result = subprocess.run(
        [str(binary), *map(str, args)], check=False, capture_output=True
    )
    elapsed = time.perf_counter() - start

    def output(data):
        if not data:
            return None
        try:
            parsed = json.loads(data)
            if parsed is None:
                raise RuntimeError("literal JSON null is not empty output")
            return parsed
        except (ValueError, UnicodeDecodeError) as exc:
            raise RuntimeError(f"output is not JSON: {data[:500]!r}") from exc

    return {
        "status": result.returncode,
        "stdout": output(result.stdout),
        "stderr": output(result.stderr),
    }, elapsed


def write_files(root, files):
    for name, value in files.items():
        if not name.startswith("$TREE0/"):
            raise ValueError(f"unsupported fixture path {name}")
        path = root / name.removeprefix("$TREE0/")
        if value is None:
            path.unlink(missing_ok=True)
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        data = value.get("text")
        if data is None:
            data = (
                value.get("prefix", "")
                + value["repeat"] * value["count"]
                + value.get("suffix", "")
            )
        path.write_text(data, encoding="utf-8")


def check(label, actual, expected):
    if json.dumps(actual, sort_keys=True) != json.dumps(expected, sort_keys=True):
        with tempfile.NamedTemporaryFile(
            mode="w", prefix="duplication-actual-", suffix=".json", delete=False
        ) as f:
            json.dump(actual, f, indent=2, ensure_ascii=False)
            f.write("\n")
        raise RuntimeError(
            f"{label}: output differs from oracle; actual written to {f.name}"
        )


def show(label, samples):
    print(
        f"{label}: runs [{', '.join(f'{x:.3f}' for x in samples)}]s, median {statistics.median(samples):.3f}s",
        flush=True,
    )


def fixture(binary, name, repeats):
    fixture_path = HERE.parents[1] / "tests" / "fixtures" / "cli" / name
    steps = json.loads(fixture_path.read_text())
    timings = [[] for _ in steps]
    for _ in range(repeats):
        with tempfile.TemporaryDirectory(prefix="duplication-fixture-") as directory:
            root = pathlib.Path(directory)
            for i, step in enumerate(steps):
                write_files(root, step["files"])
                args = [str(root) if a == "$TREE0" else a for a in step["args"]]
                actual, elapsed = execute(binary, args)
                check(
                    f"{name} step {i + 1}",
                    actual,
                    {k: step[k] for k in ("status", "stdout", "stderr")},
                )
                timings[i].append(elapsed)
    for i, samples in enumerate(timings):
        show(f"{name} step {i + 1} ({steps[i]['args'][0]})", samples)


def rolling_release(binary, repo, repeats):
    # git archive reads the named commit; the checkout and index are never changed.
    archive = subprocess.run(
        ["git", "-C", str(repo), "archive", "--format=tar", REVISION, "src"],
        check=True,
        capture_output=True,
    ).stdout
    oracle_path = HERE / "rolling-release.json"
    expected = json.loads(oracle_path.read_text())
    with tempfile.TemporaryDirectory(
        prefix="duplication-rolling-release-"
    ) as directory:
        root = pathlib.Path(directory)
        with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
            tar.extractall(root, filter="data")
        for minimum in (35, 50, 75):
            (root / ".lint-action.json").write_text(
                json.dumps(
                    {
                        "mode": "report-only",
                        "limits": {},
                        "duplication": {"minimum_tokens": minimum},
                        "include": ["src/**"],
                        "exclude": ["**/generated/**"],
                    }
                )
            )
            samples = []
            for _ in range(repeats):
                actual, elapsed = execute(binary, ["scan", "--root", root])
                check(f"rolling release {minimum}", actual, expected[str(minimum)])
                samples.append(elapsed)
            show(f"rolling release minimum {minimum}", samples)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--cli",
        required=True,
        type=pathlib.Path,
        help="absolute path to packaged release CLI",
    )
    parser.add_argument(
        "--rolling-release",
        type=pathlib.Path,
        help="local git repository containing the pinned commit (optional)",
    )
    parser.add_argument(
        "--repeat", type=int, default=3, help="timed invocations per case (default: 3)"
    )
    args = parser.parse_args()
    if not args.cli.is_absolute() or not args.cli.is_file():
        parser.error(
            "--cli must be an absolute path to the packaged release executable"
        )
    if args.repeat < 1:
        parser.error("--repeat must be positive")
    for name in (
        "source_duplication_consolidates_repeated_runs.json",
        "source_duplication_cross_file_repetition_and_third_copy.json",
        "source_duplication_baseline_is_pair_and_content_specific.json",
        "baseline_review_emits_no_patch_on_duplication_resource_error.json",
    ):
        fixture(args.cli, name, args.repeat)
    if args.rolling_release:
        rolling_release(args.cli, args.rolling_release.resolve(), args.repeat)
    else:
        print("Rolling Release corpus skipped (pass --rolling-release to include it).")
    print("All selected outputs match their oracles.")


if __name__ == "__main__":
    try:
        main()
    except (
        RuntimeError,
        OSError,
        subprocess.CalledProcessError,
        json.JSONDecodeError,
    ) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        sys.exit(1)
