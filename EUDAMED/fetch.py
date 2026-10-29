#!/usr/bin/env python3
"""Download every EUDAMED device into raw gzipped JSONL, then build a SQLite database from it.

Each phase is resumable: finished page files are skipped, so re-running a phase only fetches what is missing.
Standard library only. See EUDAMED/CLAUDE.md for the API behaviour this relies on.

    python3 -u EUDAMED/fetch.py official                         # does the official /udi serve devices yet?
    python3 -u EUDAMED/fetch.py list   [--pages 0-2] [--workers 4]
    python3 -u EUDAMED/fetch.py detail [--pages 0-2] [--workers 4]
    python3 -u EUDAMED/fetch.py basic  [--pages 0-2] [--workers 4]
    python3 -u EUDAMED/fetch.py actors
    python3 -u EUDAMED/fetch.py build                            # data/eudamed.sqlite from the raw files
    python3 -u EUDAMED/fetch.py verify [--pages 0-2]             # exits 1 on gaps
"""
import argparse
import gzip
import json
import sqlite3
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from pathlib import Path

import check_api
from check_api import HttpError, get

SEARCH = "/devices/udiDiData"
OFFICIAL = "https://api.datalake.sante.service.ec.europa.eu/eudamed"
SIZE = 300


def name(n):
    return f"page-{n:05d}.jsonl.gz"


def write(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    with gzip.open(tmp, "wt") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    tmp.replace(path)  # a half-written page never looks finished


def read(path):
    with gzip.open(path, "rt") as f:
        return [json.loads(line) for line in f]


def list_pages(data, pages=None):
    """(page number, path) of each list page on disk, in page order."""
    found = sorted((int(p.name[5:10]), p) for p in (data / "list").glob("page-*.jsonl.gz"))
    return [(n, p) for n, p in found if pages is None or n in pages]


def run(todo, fn, workers):
    """Run fn(page) for every page in todo. A failed page is logged and left for the next run."""
    start, done, failed = time.time(), 0, []
    pool = ThreadPoolExecutor(workers)
    try:
        futures = {pool.submit(fn, n): n for n in todo}
        for f in as_completed(futures):
            done += 1
            try:
                f.result()
            except Exception as e:
                failed.append(futures[f])
                print(f"page {futures[f]} failed, retry by re-running: {e}", flush=True)
            if done % 20 == 0 or done == len(todo):
                rate = done / (time.time() - start)
                print(f"{done}/{len(todo)} pages, {rate * 3600:.0f}/h, eta {(len(todo) - done) / rate / 3600:.1f}h, "
                      f"{len(failed)} failed", flush=True)
    finally:
        pool.shutdown(cancel_futures=True)  # Ctrl-C stops promptly instead of draining the queue
    return failed


def one(path, key, value):
    """Fetch one item. A 4xx (withdrawn or unknown) becomes an error record, so its page can still complete."""
    try:
        return get(path, languageIso2Code="en")
    except HttpError as e:
        if e.code >= 500:
            raise
        return {key: value, "error": str(e)}


def cmd_official(args):
    n = len(get(f"{OFFICIAL}/udi", format="json", **{"api-version": "v1.0"})["value"])
    print(f"official /udi: {n} rows" + (" -> it serves devices now; prefer it over the website backend" if n else
                                       " (still empty; use the website backend)"))


def cmd_list(args):
    out, meta_path = args.data / "list", args.data / "list" / "meta.json"

    def snapshot():
        d = get(SEARCH, page=0, size=SIZE, languageIso2Code="en")
        return {"totalElements": d["totalElements"], "totalPages": d["totalPages"],
                "at": datetime.now(timezone.utc).isoformat(timespec="seconds")}

    meta = json.loads(meta_path.read_text()) if meta_path.exists() else {}
    meta.setdefault("start", snapshot())
    out.mkdir(parents=True, exist_ok=True)
    meta_path.write_text(json.dumps(meta, indent=2))

    def fetch(n):
        d = get(SEARCH, page=n, size=SIZE, languageIso2Code="en")
        if not d["content"]:  # get() already retried the spurious empties; still empty means failed
            raise RuntimeError("empty page")
        write(out / name(n), d["content"])

    pages = args.pages or range(meta["start"]["totalPages"])
    failed = run([n for n in pages if not (out / name(n)).exists()], fetch, args.workers)
    meta["end"] = snapshot()
    meta_path.write_text(json.dumps(meta, indent=2))
    return failed


def cmd_detail(args):
    out = args.data / "detail"

    def fetch(n):
        rows = read(args.data / "list" / name(n))
        write(out / name(n), [one(f"{SEARCH}/{r['uuid']}", "uuid", r["uuid"]) for r in rows])

    return run([n for n, _ in list_pages(args.data, args.pages) if not (out / name(n)).exists()], fetch, args.workers)


def basic_owners(data):
    """{page: [(basic ulid, a udi uuid)]}: each Basic UDI-DI belongs to the first list page it appears on.
    Page order makes this the same on every run, so resumed runs agree on which file holds which Basic UDI-DI."""
    seen, owned = set(), {}
    for n, p in list_pages(data):
        for r in read(p):
            k = r.get("basicUdiDiDataUlid")
            if k and k not in seen:
                seen.add(k)
                owned.setdefault(n, []).append((k, r["uuid"]))
    return owned


def cmd_basic(args):
    out, owned = args.data / "basic", basic_owners(args.data)

    def fetch(n):
        write(out / name(n), [one(f"/devices/basicUdiData/udiDiData/{u}", "ulid", k) for k, u in owned[n]])

    todo = [n for n in owned if (args.pages is None or n in args.pages) and not (out / name(n)).exists()]
    return run(todo, fetch, args.workers)


def cmd_actors(args):
    rows, url = {}, f"{OFFICIAL}/actors?format=json&api-version=v1.0"
    while url:
        d = get(url)
        for r in d["value"]:
            # PRRC_* name a person (GDPR); nothing downstream needs them. Dropping them leaves one identical
            # row per PRRC, so de-duplicate on the rest.
            r = {k: v for k, v in r.items() if not k.startswith("PRRC_")}
            rows[json.dumps(r, sort_keys=True)] = r
        url = d.get("nextLink")
        print(f"{len(rows)} actors", flush=True)
    write(args.data / "actors.jsonl.gz", rows.values())


def code(d):
    return (d or {}).get("code")


def text(m):
    """English text of a multilingual {texts, textByDefaultLanguage} field, else the first language there is."""
    m = m or {}
    return m.get("textByDefaultLanguage") or next((t.get("text") for t in m.get("texts") or []), None)


SCHEMA = """
create table udi_di (uuid text primary key, ulid text, primary_di text, basic_udi text, basic_udi_ulid text,
  trade_name text, reference text, risk_class text, status text, manufacturer_name text, manufacturer_srn text,
  ar_srn text, json text,
  sterile int, single_use int, latex int, version_date text, last_updated text, detail_json text);
create table udi_cnd (udi_uuid text, cnd_code text, description text, primary key (udi_uuid, cnd_code));
create table basic_udi_di (uuid text primary key, ulid text, code text, device_name text, risk_class text,
  legislation text, implantable int, manufacturer_srn text, ar_srn text, json text);
create table actor (srn text primary key, name text, type text, status text, country text, json text);
"""
INDEXES = """
create index udi_di_primary_di on udi_di (primary_di);
create index udi_di_basic_udi_ulid on udi_di (basic_udi_ulid);
create index udi_di_manufacturer_srn on udi_di (manufacturer_srn);
create index udi_cnd_code on udi_cnd (cnd_code);
create index basic_udi_di_ulid on basic_udi_di (ulid);
"""


def ok(path):
    return [d for d in read(path) if "error" not in d]


def cmd_build(args):
    db_path = args.data / "eudamed.sqlite"
    tmp = db_path.with_suffix(".tmp")
    tmp.unlink(missing_ok=True)
    db = sqlite3.connect(tmp)
    db.executescript(SCHEMA)
    # insert or replace: rows seen twice because offsets shifted mid-run collapse onto one uuid.
    for _, p in list_pages(args.data):
        db.executemany("insert or replace into udi_di (uuid, ulid, primary_di, basic_udi, basic_udi_ulid, trade_name, "
                       "reference, risk_class, status, manufacturer_name, manufacturer_srn, ar_srn, json) "
                       "values (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                       [(r["uuid"], r.get("ulid"), r.get("primaryDi"), r.get("basicUdi"), r.get("basicUdiDiDataUlid"),
                         r.get("tradeName"), r.get("reference"), code(r.get("riskClass")),
                         code(r.get("deviceStatusType")), r.get("manufacturerName"), r.get("manufacturerSrn"),
                         r.get("authorisedRepresentativeSrn"), json.dumps(r)) for r in read(p)])
    for p in sorted((args.data / "detail").glob("page-*.jsonl.gz")):
        rows = ok(p)
        db.executemany("update udi_di set sterile=?, single_use=?, latex=?, version_date=?, last_updated=?, "
                       "detail_json=? where uuid=?",
                       [(d.get("sterile"), d.get("singleUse"), d.get("latex"), d.get("versionDate"),
                         d.get("lastUpdated"), json.dumps(d), d["uuid"]) for d in rows])
        db.executemany("insert or replace into udi_cnd values (?,?,?)",
                       [(d["uuid"], c.get("code"), text(c.get("description")))
                        for d in rows for c in d.get("cndNomenclatures") or []])
    for p in sorted((args.data / "basic").glob("page-*.jsonl.gz")):
        db.executemany("insert or replace into basic_udi_di values (?,?,?,?,?,?,?,?,?,?)",
                       [(b["uuid"], b.get("ulid"), code(b.get("basicUdi")), b.get("deviceName"),
                         code(b.get("riskClass")), code(b.get("legislation")), b.get("implantable"),
                         (b.get("manufacturer") or {}).get("srn"), (b.get("authorisedRepresentative") or {}).get("srn"),
                         json.dumps(b)) for b in ok(p)])
    actors = args.data / "actors.jsonl.gz"
    if actors.exists():
        db.executemany("insert or replace into actor values (?,?,?,?,?,?)",
                       [(a["ACTOR_ID"], a.get("NAME"), a.get("ACTOR_TYPE"), a.get("STATUS"),
                         a.get("ACT_COUNTRY_ISO2_CODE"), json.dumps(a)) for a in read(actors)])
    db.executescript(INDEXES)
    db.commit()
    for table in ("udi_di", "udi_cnd", "basic_udi_di", "actor"):
        print(f"{table}: {db.execute(f'select count(*) from {table}').fetchone()[0]} rows")
    db.close()
    tmp.replace(db_path)


def cmd_verify(args):
    gaps = []

    def check(label, problem):
        print(f"{'GAP ' if problem else 'ok  '} {label}")
        if problem:
            gaps.append(label)

    meta = json.loads((args.data / "list" / "meta.json").read_text())
    total = meta.get("end", meta["start"])
    expected = args.pages or range(total["totalPages"])
    have = dict(list_pages(args.data, expected))
    check(f"list pages: {len(have)}/{len(expected)}", len(have) < len(expected))

    uuids, ulids = set(), set()
    for p in have.values():
        for r in read(p):
            uuids.add(r["uuid"])
            ulids.add(r.get("basicUdiDiDataUlid"))
    ulids.discard(None)
    if args.pages is None:
        # shortcut: a shortfall means offsets shifted mid-run, and we can't tell which pages; re-fetch the list to fix.
        check(f"unique uuids {len(uuids)} vs totalElements start {meta['start']['totalElements']} / end "
              f"{total['totalElements']}", len(uuids) < total["totalElements"])

    for kind, key, wanted in (("detail", "uuid", uuids), ("basic", "ulid", ulids)):
        got, errors = set(), 0
        for p in (args.data / kind).glob("page-*.jsonl.gz"):
            for d in read(p):
                if "error" in d:
                    errors += 1
                else:
                    got.add(d.get(key))
        # Error records are permanent 4xx (withdrawn/unknown); to retry them, delete their page files and re-run.
        check(f"{kind}: {len(wanted & got)}/{len(wanted)} fetched, {errors} error records", wanted - got)
    sys.exit(1 if gaps else 0)


def pages(s):
    lo, _, hi = s.partition("-")
    return range(int(lo), int(hi or lo) + 1)


def main():
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("phase", choices=["official", "list", "detail", "basic", "actors", "build", "verify"])
    p.add_argument("--pages", type=pages, help="list page range such as 0-2 (default: all)")
    p.add_argument("--workers", type=int, default=4, help="concurrent pages; raise one step at a time")
    p.add_argument("--rate", type=float, default=18, help="max requests per minute, shared by all workers")
    p.add_argument("--data", type=Path, default=Path(__file__).parent / "data")
    args = p.parse_args()
    check_api.min_interval = 60 / args.rate
    failed = globals()[f"cmd_{args.phase}"](args)
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
