
import os
import json
import collections
import shutil
import hashlib

import click

import whvp

from whvp.snapshot import RpycSnapshot, DumpSnapshot


@click.command()
@click.argument("crashes")
@click.argument("output")
@click.option("--snapshot", default="localhost:18861", show_default=True)
@click.option("--limit", default=50, type=int)
def cli(crashes, output, snapshot, limit):
    whvp.init_log()

    if ":" in snapshot:
        hostname, port = snapshot.split(":")
        snapshot = RpycSnapshot(hostname, int(port))
    else:
        path = snapshot
        snapshot = DumpSnapshot(path)

    context = snapshot.get_initial_context()
    params = snapshot.get_params()

    tracer = whvp.Tracer(snapshot.memory_access_callback)

    os.makedirs(output, exist_ok=True)

    traces_dir = os.path.join(output, "traces")
    os.makedirs(traces_dir, exist_ok=True)

    buckets_dir = os.path.join(output, "buckets")
    os.makedirs(buckets_dir, exist_ok=True)

    coverages = {}

    files = [crash for crash in os.listdir(crashes) if crash.endswith(".bin")]
    whvp.log(F"loaded {len(files)} crash(es)")
    whvp.log(F"gathering coverage")
    for index, f in enumerate(files):
        coverage_path = os.path.join(traces_dir, f + ".trace.json")
        if os.path.exists(coverage_path):
            whvp.log(F"coverage exists for {f}, loading from file")
            with open(coverage_path, "r") as fp:
                coverage = json.load(fp)
        else:
            whvp.log(F"doing coverage for {f}")
            whvp.log("replaying input")
            replay = os.path.join(crashes, f)
            with open(replay, "rb") as fp:
                replay_data = fp.read()

            path = os.path.splitext(replay)[0] + ".json"
            with open(path, "r") as fp:
                replay_params = json.load(fp)

            tracer.set_initial_context(context)
            params["limit"] = 0
            params["coverage"] = "no"
            params["save_context"] = False
            params["save_instructions"] = False

            whvp.log("first run to map memory")
            trace = tracer.run(params)
            tracer.restore_snapshot()

            tracer.write_virtual_memory(replay_params["input"], replay_data)

            params["limit"] = 0
            params["coverage"] = "instrs"
            params["save_context"] = False
            params["save_instructions"] = False

            whvp.log("second run to replay crash")
            tracer.set_initial_context(context)
            trace = tracer.run(params)
            tracer.restore_snapshot()

            coverage = trace.get_coverage()
            seen = trace.get_unique_addresses()
            status = trace.get_status()
            time = trace.get_elapsed_time()
            whvp.log(F"executed {len(coverage)} instruction(s), {len(seen)} were unique in {time} ({status})")

            whvp.log(F"saving trace to {coverage_path}")
            trace.save(coverage_path)

            with open(coverage_path, "r") as fp:
                coverage = json.load(fp)

        coverages[f] = coverage

    buckets = collections.defaultdict(list)

    for f in files:
        m = hashlib.sha1()
        for (address, context) in coverages[f]["coverage"][-limit:]:
            data = F"{address:016x}"
            m.update(bytes(data, encoding="utf-8"))

        bucket = m.hexdigest()
        buckets[bucket].append(f)

    whvp.log(F"triaged {len(files)} crash(es)")
    whvp.log(F"found {len(buckets)} unique crash(es)")
    for bucket, duplicates in buckets.items():
        whvp.log(F"bucket {bucket} contains {len(duplicates)} file(s)")

        bucket_path = os.path.join(buckets_dir, bucket)
        os.makedirs(bucket_path, exist_ok=True)

        for d in duplicates:
            src = os.path.join(crashes, d)
            shutil.copy(src, bucket_path)

            src = os.path.join(crashes, os.path.splitext(d)[0] + ".json")
            shutil.copy(src, bucket_path)


def entrypoint():
    cli()


if __name__ == "__main__":
    entrypoint()
