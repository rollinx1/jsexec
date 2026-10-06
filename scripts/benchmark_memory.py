#!/usr/bin/env python3
"""Compare CLI peak RSS on deterministic local inputs. Requires Linux and Python 3."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return number


def measure_worker(binary, args):
    with tempfile.TemporaryFile() as errors:
        start = time.monotonic()
        process = subprocess.Popen(
            [str(binary), *map(str, args)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=errors,
        )
        _, status, usage = os.wait4(process.pid, 0)
        process.returncode = os.waitstatus_to_exitcode(status)
        if process.returncode:
            errors.seek(0)
            raise RuntimeError(errors.read().decode("utf-8", errors="replace"))
        return {
            "peak_mib": round(usage.ru_maxrss / 1024, 2),
            "seconds": round(time.monotonic() - start, 3),
        }


def measure(binary, args):
    # A fresh helper avoids charging the fixture-generating Python process's
    # resident memory to children before exec on Linux.
    result = subprocess.run(
        [sys.executable, str(Path(__file__).resolve()), "--measure", str(binary), *map(str, args)],
        capture_output=True, text=True, check=True,
    )
    return json.loads(result.stdout)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/release/jsexec"))
    parser.add_argument("--compare-binary", type=Path)
    parser.add_argument("--functions", type=positive, default=10_000)
    parser.add_argument("--map-mib", type=positive, default=16)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("peak RSS measurement requires Linux")
    binaries = [args.binary.resolve()]
    if args.compare_binary:
        binaries.insert(0, args.compare_binary.resolve())
    for binary in binaries:
        if not binary.is_file():
            parser.error(f"binary does not exist: {binary}; build it first")

    with tempfile.TemporaryDirectory(prefix="jsexec-memory-") as directory:
        root = Path(directory)
        bundle = root / "bundle.js"
        bundle.write_text("".join(
            f'function f{i}(x) {{ const options = {{method: "GET", headers: '
            f'{{Accept: "application/json"}}}}; return fetch("/api/users/{i}", '
            'options).then(r => r.json()); }\n'
            for i in range(args.functions)
        ), encoding="utf-8")
        wrapped = root / "wrapped.js"
        wrapped.write_text("(function(){\n" + bundle.read_text(encoding="utf-8") + "})();", encoding="utf-8")
        source_map = root / "bundle.map"
        with source_map.open("w", encoding="utf-8") as destination:
            json.dump({
                "version": 3, "sources": ["app.js"],
                "sourcesContent": ["x" * args.map_mib * 1024 * 1024],
                "mappings": "AAAA;" * 1024 * 1024, "names": ["a"],
            }, destination)
        files = []
        for index in range(4):
            path = root / f"input-{index}.js"
            path.write_text("/*" + "x" * 8 * 1024 * 1024 + "*/\nfetch('/api');\n", encoding="utf-8")
            files.append(path)
        cases = [
            ("query", [bundle], ["query", "CallExpression", bundle, "--limit", "20"]),
            ("query_count", [bundle], ["query", "CallExpression", bundle, "--limit", "0"]),
            ("wrapped_query", [wrapped], ["query", "CallExpression", wrapped, "--limit", "20"]),
            ("multiple_inputs", files, ["ast", *files]),
            ("sourcemap", [source_map], ["sourcemaps", source_map]),
        ]
        results = []
        for name, inputs, command in cases:
            row = {"case": name, "input_mib": round(sum(path.stat().st_size for path in inputs) / 1024**2, 2), "builds": []}
            for binary in binaries:
                row["builds"].append({"binary": str(binary), **measure(binary, command)})
            results.append(row)
        print(json.dumps(results, indent=2))


if __name__ == "__main__":
    if len(sys.argv) > 2 and sys.argv[1] == "--measure":
        print(json.dumps(measure_worker(Path(sys.argv[2]), sys.argv[3:])))
    else:
        main()
