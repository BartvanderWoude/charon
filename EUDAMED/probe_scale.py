#!/usr/bin/env python3
"""Probe whether the EUDAMED device search can be paged in full, and how fast.

Answers: does the official Public API serve devices, do deep pages work, is the order stable,
is `sort` honoured, and what latency/error rate each concurrency level gives. Standard library only.

    python3 -u EUDAMED/probe_scale.py [--samples 12]    # takes ~30 min
"""
import argparse
import json
import random
import statistics
import time
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

from check_api import get

SEARCH = "/devices/udiDiData"
OFFICIAL = "https://api.datalake.sante.service.ec.europa.eu/eudamed"
SIZE = 300


def page(n, **params):
    return get(SEARCH, page=n, size=SIZE, languageIso2Code="en", **params)


def uuids(n, **params):
    return [i["uuid"] for i in page(n, **params)["content"]]


def official():
    def count(path, **params):
        url = f"{OFFICIAL}/{path}?{urllib.parse.urlencode({**params, 'format': 'json', 'api-version': 'v1.0'})}"
        req = urllib.request.Request(url, headers={"User-Agent": "eudamed-check/1.0", "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=60) as r:
            d = json.load(r)
        return f"{len(d['value'])} rows{' + nextLink' if d.get('nextLink') else ''}"
    srn = page(0)["content"][0]["manufacturerSrn"]
    print(f"official API: /actors?ACTOR_ID={srn} -> {count('actors', ACTOR_ID=srn)}; "
          f"/udi?MF_SRN={srn} -> {count('udi', MF_SRN=srn)}; /udi (no filter) -> {count('udi')}")


def deep_paging(last):
    for n in (0, 33, 34, 1000, 10000, last):
        t = time.time()
        d = page(n)
        print(f"deep paging: page {n:>5} -> {len(d['content'])} items, number={d['number']}, {time.time() - t:.1f}s")


def stability(n):
    runs = [uuids(n) for _ in range(3)]
    print(f"stable order: page {n} fetched 3x, identical={all(r == runs[0] for r in runs)}")
    base = runs[0]
    # Unknown sort fields are silently ignored, so bogusField shows what "ignored" looks like.
    for sort in ("primaryDi,asc", "uuid,asc", "bogusField,asc"):
        t = time.time()
        a = uuids(n, sort=sort)
        took = time.time() - t
        print(f"sort={sort}: differs from default={a != base}, repeatable={a == uuids(n, sort=sort)}, {took:.1f}s")


def throughput(last, samples):
    for workers in (1, 2, 4):
        pages = random.sample(range(last + 1), samples)
        times, errors = [], 0

        def timed(n):
            t = time.time()
            page(n)
            return time.time() - t

        start = time.time()
        with ThreadPoolExecutor(workers) as pool:
            for f in [pool.submit(timed, n) for n in pages]:
                try:
                    times.append(f.result())
                except RuntimeError:
                    errors += 1
        wall = time.time() - start
        per_page = wall / samples
        print(f"concurrency {workers}: median {statistics.median(times):.1f}s/request, {errors}/{samples} failed after retries, "
              f"{per_page:.1f}s wall/page -> full run ~{per_page * (last + 1) / 3600:.0f}h")


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--samples", type=int, default=12, help="random pages per concurrency level")
    args = p.parse_args()
    first = page(0)
    last = first["totalPages"] - 1
    print(f"totalElements={first['totalElements']}, totalPages={first['totalPages']} at size={SIZE}")
    official()
    deep_paging(last)
    stability(5000)
    throughput(last, args.samples)


if __name__ == "__main__":
    main()
