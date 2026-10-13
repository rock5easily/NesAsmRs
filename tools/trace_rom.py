"""Record real-ROM MCP tracing and round trips without replacing existing sources."""
import argparse
import collections
import hashlib
import json
import pathlib
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", required=True)
    parser.add_argument("--root", required=True)
    parser.add_argument("--annotations", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--inspect", help="Print instructions at bank:hex-address instead of exporting")
    args = parser.parse_args()
    root = pathlib.Path(args.root).resolve()
    output = pathlib.Path(args.output)
    if output.is_absolute() or ".." in output.parts or output.suffix != ".asm":
        parser.error("output must be a relative .asm path inside the root")
    config = None
    original = None
    before = None
    if not args.inspect:
        config = json.loads((root / args.annotations).read_text(encoding="utf-8-sig"))
        original = (root / args.annotations).parent / config["rom"]
        before = original.read_bytes()
        protected = [original]
        if config.get("base"):
            protected.append((root / args.annotations).parent / config["base"])
        if any((root / output.with_suffix(".nes")).resolve() == path.resolve() for path in protected):
            parser.error("the rebuilt ROM must have a different path from the input")
    (root / output).parent.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen(
        [args.server, "--root", str(root)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, encoding="utf-8",
    )
    request_id = 0

    def request(method, params):
        nonlocal request_id
        request_id += 1
        process.stdin.write(json.dumps(dict(jsonrpc="2.0", id=request_id,
                                           method=method, params=params)) + "\n")
        process.stdin.flush()
        while True:
            line = process.stdout.readline()
            if not line:
                raise RuntimeError("MCP server exited before responding")
            reply = json.loads(line)
            if reply.get("id") == request_id:
                if "error" in reply:
                    raise RuntimeError(reply["error"])
                return reply["result"]

    def call(name, arguments):
        result = request("tools/call", dict(name=name, arguments=arguments))
        value = result.get("structuredContent")
        if value is None:
            value = json.loads(result["content"][0]["text"])
        if not value.get("success"):
            raise RuntimeError(value)
        return value

    try:
        request("initialize", dict(protocolVersion="2025-11-25", capabilities={},
                                   clientInfo=dict(name="trace-rom", version="1")))
        process.stdin.write(json.dumps(dict(jsonrpc="2.0", method="notifications/initialized")) + "\n")
        process.stdin.flush()
        if args.inspect:
            bank, address = args.inspect.split(":")
            result = request("tools/call", dict(name="inspect_rom", arguments=dict(
                rom=args.annotations, disassemble=dict(bank=int(bank), address=int(address, 16), length=100))))
            value = result.get("structuredContent", result)
            for line in value.get("disassembly", []):
                print(f'{line["address"]:04X}  {line["bytes"]:<9} {line["instruction"]}')
            if "disassembly" not in value:
                print(json.dumps(value, ensure_ascii=False))
            return
        arguments = dict(annotations=args.annotations, output=output.as_posix())
        report = call("disassemble", dict(**arguments, issues=dict(limit=1000),
                      routines=dict(limit=1000), ram=dict(limit=1000),
                      view=dict(bank=30, **{"from": "$C400", "to": "$C600"})))
        for key in ("issues", "routines", "ram", "diffs"):
            page = report.get(key)
            if page:
                while len(page["items"]) < page["total"]:
                    more = call("disassemble", dict(**arguments, **{key: dict(
                        offset=len(page["items"]), limit=1000)}))
                    if not more[key]["items"]:
                        raise RuntimeError(f"Empty page while collecting {key}")
                    page["items"].extend(more[key]["items"])
        report["export"] = call("export_disassembly", arguments)
        report["check"] = call("check", dict(input=output.as_posix()))
        rebuilt = output.with_suffix(".nes")
        report["assemble"] = call("assemble", dict(input=output.as_posix(), output=rebuilt.as_posix()))
        after = (root / rebuilt).read_bytes()
        assert original.read_bytes() == before, "Input ROM changed during verification"
        report["independent_verification"] = dict(
            matched=before == after, size=len(before),
            input_sha256=hashlib.sha256(before).hexdigest(),
            rebuilt_sha256=hashlib.sha256(after).hexdigest())
        assert before == after, "Rebuilt ROM differs"
        assert report["verification"]["matched"]
        assert report["export"]["verification"]["matched"]
        sources = [(root / name).read_text(encoding="utf-8") for name in report["files"]]
        lines = [line for source in sources for line in source.splitlines()]
        assert not any(line.startswith((" ", "\t\t", "\t ")) for line in lines), "Expected one tab of indentation"
        includes = [line.strip() for line in lines if line.strip().startswith(".incbin ")]
        if config.get("base"):
            assert len(includes) == 2, "Expected one base PRG include and one CHR include"
            assert output.with_suffix(".chr").name in sources[0].splitlines()[-1]
        if config.get("options", {}).get("mapper4_fixed_prg_16k"):
            assert any(line.strip().startswith(".catbank ") for line in sources[0].splitlines())
        chr_file = root / output.with_suffix(".chr")
        if chr_file.exists():
            prg_size = before[4] * 16384
            chr_size = before[5] * 8192
            chr_start = 16 + (512 if before[6] & 4 else 0) + prg_size
            assert chr_file.read_bytes() == before[chr_start:chr_start + chr_size]
        report["source_checks"] = dict(tab_indentation=True, incbin_count=len(includes),
                                       chr_matched=chr_file.exists())
        report["annotations_snapshot"] = config
        banks = [b for b in report["summary"]["banks"] if not b["chr"]]
        report["metrics"] = dict(
            instructions=report["summary"]["instructions"],
            code_bytes=sum(b["code_bytes"] for b in banks),
            unknown_bytes=sum(b["unknown_bytes"] for b in banks),
            issues=collections.Counter(i["kind"] for i in report["issues"]["items"]))
        (root / output.with_suffix(".report.json")).write_text(
            json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps(dict(metrics=report["metrics"], verification=report["independent_verification"])))
    finally:
        process.stdin.close()
        process.wait(timeout=10)
        process.stdout.close()


if __name__ == "__main__":
    main()
