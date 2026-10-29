#!/usr/bin/env python3
"""Check the public EUDAMED API against https://openregulatory.github.io/eudamed-api/.

Calls the four documented endpoints in a chain (search -> device -> Basic UDI-DI -> actor)
and prints PASS / DEVIATION / FAIL per check. Standard library only.

    python3 EUDAMED/check_api.py [--tradeName catheter] [--pageSize 300] [--dump DIR]
"""
import argparse
import http.client
import json
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

BASE = "https://ec.europa.eu/tools/eudamed/api"

# Top-level keys the spec documents per endpoint.
SEARCH_KEYS = {"content", "first", "last", "number", "numberOfElements", "size", "sort", "totalElements", "totalPages"}
ITEM_KEYS = {"basicUdi", "primaryDi", "uuid", "ulid", "tradeName", "manufacturerName", "deviceStatusType"}
DEVICE_KEYS = {"uuid", "ulid", "primaryDi", "containedItem", "marketInfoLink", "secondaryDi", "tradeName", "deviceStatus",
               "sterile", "singleUse", "cndNomenclatures", "versionDate", "versionState", "latestVersion", "versionNumber", "new"}
BASIC_KEYS = {"uuid", "ulid", "udiDiData", "manufacturer", "authorisedRepresentative", "basicUdi", "deviceName", "deviceModel",
              "riskClass", "specialDeviceType", "legislation", "deviceCertificateInfoList", "deviceCertificateInfoListForDisplay",
              "clinicalInvestigationLinks", "versionDate", "versionState", "latestVersion", "versionNumber", "linkedSscp", "new"}
ACTOR_KEYS = {"importers", "nonEuManufacturers", "actorDataPublicView"}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    # Unknown IDs 302 to an internal EC host; surface that as an error instead of following it.
    def redirect_request(self, *args, **kwargs):
        return None


opener = urllib.request.build_opener(NoRedirect)
# The gateway answers the default "Python-urllib" User-Agent with 502 Bad Gateway.
# The official Data Lake API also wants Content-Type on GETs; the website backend ignores it.
opener.addheaders = [("User-Agent", "eudamed-check/1.0"), ("Accept", "application/json"), ("Content-Type", "application/json")]
dump_dir = None
# Seconds between any two requests, across all threads. The backend allows a burst of ~20, then answers 429 with
# Retry-After: 60 to every endpoint; one request every 3 s never tripped it. 0 = unpaced (fine for a few calls).
min_interval = 0
_pace_lock = threading.Lock()
_next_at = 0.0


def pace():
    global _next_at
    with _pace_lock:
        now = time.monotonic()
        wait = max(0.0, _next_at - now)
        _next_at = max(now, _next_at) + min_interval
    time.sleep(wait)


class HttpError(RuntimeError):
    def __init__(self, code, url):
        super().__init__(f"HTTP {code} for {url}")
        self.code = code


def get(path, retries=5, **params):
    """GET a JSON resource (a path under BASE, or an absolute URL such as the official API's nextLink).
    Retries 429 (after Retry-After), 5xx, network errors, timeouts, and the spurious empty search pages."""
    url = path if path.startswith("https://") else f"{BASE}{path}"
    if params:
        url += f"?{urllib.parse.urlencode(params)}"
    for attempt in range(retries):
        last = attempt == retries - 1
        pace()
        try:
            with opener.open(url, timeout=60) as r:
                body = r.read()
                if "json" not in r.headers.get("Content-Type", ""):
                    raise ValueError(f"non-JSON response ({r.headers.get('Content-Type')})")
                data = json.loads(body)
            # About half of identical search calls come back empty (totalElements=0); retrying fixes it.
            if not last and data.get("content") == [] and data.get("totalElements") == 0:
                time.sleep(1)
                continue
            if dump_dir:
                name = path.strip("/").replace("/", "_") + ("_" + "_".join(f"{k}-{v}" for k, v in params.items()))
                (dump_dir / f"{name}.json").write_text(json.dumps(data, indent=2))
            return data
        except urllib.error.HTTPError as e:
            if e.code == 429 and not last:
                wait = e.headers.get("Retry-After", "60")
                time.sleep(int(wait) if wait.isdigit() else 60)
                continue
            if e.code < 500 or last:
                raise HttpError(e.code, url) from None
        # OSError covers URLError, timeouts and dropped connections; ValueError covers truncated or non-JSON bodies.
        except (OSError, http.client.HTTPException, ValueError) as e:
            if last:
                raise RuntimeError(f"{e} for {url}") from None
        time.sleep(2 * (attempt + 1))


results = []


def report(name, status, notes):
    results.append(status)
    print(f"[{status:9}] {name}")
    for n in notes:
        print(f"            - {n}")


def check_keys(data, keys):
    missing = sorted(keys - data.keys())
    return [f"missing documented keys: {missing}"] if missing else []


def check(name, fn):
    try:
        return fn()
    except Exception as e:
        report(name, "FAIL", [str(e)])


def search(args):
    documented = dict(page=1, pageSize=args.pageSize, size=args.pageSize, iso2Code="en", languageIso2Code="en")
    if args.tradeName:
        documented["tradeName"] = args.tradeName
    data = get("/devices/udiDiData", **documented)
    problems = check_keys(data, SEARCH_KEYS)
    if data["content"]:
        problems += check_keys(data["content"][0], ITEM_KEYS)
    else:
        problems.append("documented query returned an empty page")
    if data.get("size") != args.pageSize:
        problems.append(f"requested pageSize={args.pageSize}, got size={data.get('size')}")
    if data.get("number") != 1:
        problems.append(f"page=1 came back as number={data.get('number')}")
    notes = [f"totalElements={data.get('totalElements')}, size={data.get('size')}, number={data.get('number')}"]
    # The spec calls all five params required; see which ones actually matter.
    for drop in ("languageIso2Code", "iso2Code", "page", "size", "pageSize"):
        variant = {k: v for k, v in documented.items() if k != drop}
        page = get("/devices/udiDiData", **variant)
        notes.append(f"without {drop}: {len(page['content'])} items, number={page.get('number')}")
    nulls = sum(1 for i in data["content"] if not i.get("basicUdiDataUuid"))
    notes.append(f"{nulls}/{len(data['content'])} items have basicUdiDataUuid=null")
    report("search  GET /devices/udiDiData", "DEVIATION" if problems else "PASS", problems + notes)
    return data["content"]


def device(items):
    d = get(f"/devices/udiDiData/{items[0]['uuid']}", languageIso2Code="en")
    notes = check_keys(d, DEVICE_KEYS)
    report("device  GET /devices/udiDiData/{uuid}", "DEVIATION" if notes else "PASS",
           notes + [f"tradeName: {(d.get('tradeName') or {}).get('textByDefaultLanguage')}"])


def basic(items):
    notes = []
    uuid = next((i["basicUdiDataUuid"] for i in items if i.get("basicUdiDataUuid")), None)
    if not uuid:
        # Search leaves basicUdiDataUuid null; the UI resolves it via this undocumented route instead.
        uuid = get(f"/devices/basicUdiData/udiDiData/{items[0]['uuid']}", languageIso2Code="en")["uuid"]
        notes.append("Basic UDI-DI uuid resolved via undocumented /devices/basicUdiData/udiDiData/{udiDiUuid}")
    b = get(f"/devices/basicUdiData/{uuid}", languageIso2Code="en")
    problems = check_keys(b, BASIC_KEYS)
    report("basic   GET /devices/basicUdiData/{uuid}", "DEVIATION" if problems else "PASS",
           problems + notes + [f"basicUdi={(b.get('basicUdi') or {}).get('code')}, riskClass={(b.get('riskClass') or {}).get('code')}"])
    return b


def actor(b):
    uuid = (b.get("manufacturer") or {}).get("uuid")
    if not uuid:
        return report("actor   GET /actors/{uuid}/publicInformation", "FAIL", ["Basic UDI-DI had no manufacturer.uuid"])
    a = get(f"/actors/{uuid}/publicInformation", languageIso2Code="en")
    notes = check_keys(a, ACTOR_KEYS)
    pub = a.get("actorDataPublicView") or {}
    report("actor   GET /actors/{uuid}/publicInformation", "DEVIATION" if notes else "PASS",
           notes + [f"actor: {(pub.get('name') or {}).get('textByDefaultLanguage')}, srn (eudamedIdentifier)={pub.get('eudamedIdentifier')}"])
    try:
        get("/actors/00000000-0000-0000-0000-000000000000/publicInformation", retries=1, languageIso2Code="en")
    except RuntimeError as e:
        print(f"            - unknown actor id -> {e}")


def main():
    global dump_dir
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--tradeName", default="catheter", help="search filter; '' for the empty query")
    p.add_argument("--pageSize", type=int, default=300, help="spec max is 300")
    p.add_argument("--dump", type=Path, help="write raw JSON responses here")
    args = p.parse_args()
    if args.dump:
        dump_dir = args.dump
        dump_dir.mkdir(parents=True, exist_ok=True)

    items = check("search  GET /devices/udiDiData", lambda: search(args))
    if items:
        check("device  GET /devices/udiDiData/{uuid}", lambda: device(items))
        b = check("basic   GET /devices/basicUdiData/{uuid}", lambda: basic(items))
        if b:
            check("actor   GET /actors/{uuid}/publicInformation", lambda: actor(b))
    sys.exit(1 if "FAIL" in results or len(results) < 4 else 0)


if __name__ == "__main__":
    main()
