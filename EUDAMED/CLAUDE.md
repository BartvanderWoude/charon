# EUDAMED public API — how it actually behaves

Observed on 2026-10-28 by running `check_api.py`. The older community spec at
https://openregulatory.github.io/eudamed-api/ is mostly right but wrong in a few places that matter (see Quirks).
This is the undocumented JSON backend behind the EUDAMED public website, not an official API, so it can change.

## Basics
- Base URL: `https://ec.europa.eu/tools/eudamed/api`. GET only. No auth. JSON responses.
- **Send your own `User-Agent`.** The default `Python-urllib/x.y` agent gets `502 Bad Gateway`. Plain curl works.
- Expect flakiness. Always retry: see Quirks for the empty pages and the random 500/502 errors.
- **Rate limit: about 20 requests per minute per client, across all endpoints.** See "Rate limit" below.

## Endpoints
| Purpose | Path | Notes |
|---|---|---|
| Search UDI-DIs (devices) | `/devices/udiDiData` | Paged; filters such as `tradeName=catheter` work |
| UDI-DI detail | `/devices/udiDiData/{udiDiUuid}` | |
| Basic UDI-DI for a UDI-DI | `/devices/basicUdiData/udiDiData/{udiDiUuid}` | **Not in the spec.** This is how to get the Basic UDI-DI uuid |
| Basic UDI-DI detail | `/devices/basicUdiData/{basicUdiUuid}` | Same body as the previous row |
| Actor (manufacturer, etc.) | `/actors/{actorUuid}/publicInformation` | |

Pass `languageIso2Code=en` everywhere. Multilingual fields come back in that language.

### Search params (`/devices/udiDiData`)
- `size`: **this is the page size.** The default is 20 and the cap is 300 (`size=301` gives 300). `pageSize` is ignored.
- `page`: **0-based.** Leaving it out returns page 0. `page=1` is the *second* page, even though the spec says "starts at 1".
- `iso2Code` and `languageIso2Code` are not actually required for search. Send `languageIso2Code=en` anyway.
- Totals: 4,619,548 UDI-DIs with no filter, and 30,886 for `tradeName=catheter`.
- Response: Spring-style page `{content, first, last, number, numberOfElements, size, sort, totalElements,
  totalPages, empty, pageable}`.
- Item keys include `uuid`, `ulid`, `primaryDi`, `basicUdi` (string), `tradeName` (string), `manufacturerName`,
  `manufacturerSrn`, `authorisedRepresentativeName/Srn`, `riskClass`, `deviceStatusType.code`, `basicUdiDataUuid`
  (**always null in practice**), and `basicUdiDiDataUlid`.

## Following IDs from one endpoint to the next
1. Search → `content[i].uuid` → `/devices/udiDiData/{uuid}` (detail).
2. The same `uuid` → `/devices/basicUdiData/udiDiData/{uuid}` → Basic UDI-DI, whose own uuid is `.uuid`.
   Don't rely on `basicUdiDataUuid` from search: it is null. Passing a ULID to `/devices/basicUdiData/{id}` gives 500.
3. Basic UDI-DI `.manufacturer.uuid` → `/actors/{uuid}/publicInformation`.
   `.manufacturer` also carries `name`, `srn` and `actorType` directly. The same goes for `.authorisedRepresentative`.

## Response shapes worth knowing
- Multilingual text, such as `tradeName` in the detail view and the actor's `name`:
  `{texts: [{language: {isoCode}, text}], textByDefaultLanguage}`. Read `textByDefaultLanguage`. It can be null
  when no English text exists, so fall back to `texts[0].text`.
- Codes are refdata strings, for example `riskClass.code = "refdata.risk-class.class-i"` and
  `deviceStatusType.code = "refdata.device-model-status.on-the-market"`.
- Basic UDI-DI: `basicUdi` is an object `{uuid, code, issuingAgency, type}`, not a string. It also has
  `riskClass`, `legislation`, `deviceCertificateInfoList`, `manufacturer` and `authorisedRepresentative`.
- Actor: `{importers, nonEuManufacturers, actorDataPublicView}`. In `actorDataPublicView` the SRN is called
  `eudamedIdentifier` (for example `PK-MF-000041619`), `name` is multilingual, and there are also `type`,
  `country`, `actorAddress`, `certificates`, `authorisedRepresentatives` and more.

## Quirks
- **Spurious empty pages:** about half of identical search calls return `200` with `content: []` and
  `totalElements: 0`. Retry until you get content; `check_api.py`'s `get()` already does this.
- Random `500` on endpoints that work a moment later. Retry with backoff.
- Unknown actor uuid → `404`. A malformed path → `302` to an internal `*.cec.eu.int` HTML page. Don't follow
  redirects.
- Error bodies are JSON: `{httpStatus, httpStatusCode, messageCode, technicalErrorMessage, ...}`.

## Official Public API (Data Lake): devices not available yet
Observed 2026-10-28. `https://api.datalake.sante.service.ec.europa.eu/eudamed/{udi,actors,reference}?format=json|csv&api-version=v1.0`
is the Commission's official, documented API ([user guide PDF](https://webgate.ec.europa.eu/eudamed-play-help/en/files/Public%20API%20-%20user%20guide.pdf),
[OpenAPI](https://webgate.ec.europa.eu/eudamed-help/en/files/Swagger%20OpenAPI%20file.yaml)). No key is needed, but send
`Content-Type: application/json`. Responses are `{value: [...], nextLink}`, with 1000 rows per page and cursor
paging (`$after=` inside `nextLink`; follow the link rather than building it yourself).
- `/actors` and `/reference` work. For example, `ACT_COUNTRY_ISO2_CODE=NL` returns 1000 rows plus a `nextLink`.
  The rows are flat (`ACTOR_ID` = SRN, address, PRRC name).
- **`/udi` returns `{"value": []}` for every query**, including no filter, `MF_SRN`, `PRIMARY_DI`, `BASIC_UDI` and
  `RISK_CLASS_ID`, even for devices and manufacturers the website shows. A community MCP server reported it working
  on 2026-10-14. Re-check before each scrape: if `/udi` is back, use it instead of the website backend (official,
  cursor-paged, 1000 rows per page, CSV available).
- `/reference?CODE=RISK_CLASS_ID&LANGUAGE=en` lists the 13 risk classes, which is how the partition below was found.

## Full-scale retrieval of the device list (website backend)
Measured 2026-10-28 by `probe_scale.py` and ad-hoc calls.
- **Deep paging works** up to the last page (`page=15398` at `size=300` returns the final 148 rows). There is no
  10k-result window.
- **Latency grows with the page offset:** about 0.3 to 1 s for pages up to ~1000, ~20 s at page 7000 to 10000, and ~35 s at the
  last page. It also swings a lot from call to call.
- **Filters the UI uses** (from the `search-device` JS chunk): `riskClassCode`, `deviceStatusCode`, `applicableLegislation`,
  `srn`, `tradeName`, `primaryDi`, `basicUdi`, `reference`, `deviceModel`, `includeHistoricalVersion`, and others. An
  unknown value gives `400`.
- **Risk class partitions the data exactly.** The 13 `riskClassCode` values (`refdata.risk-class.` + `class-i`, `class-iia`,
  `class-iib`, `class-iii`, `class-a`…`class-d`, `aimdd`, `ivd-general`, `ivd-annex-ii-list-a`, `ivd-annex-ii-list-b`,
  `ivd-devices-self-testing`) sum to exactly the unfiltered 4,619,548. Class I alone is 2,217,450.
- **Partitioning does not make deep pages cheaper.** Class-I page 7000 is as slow as global page 7000. `srn` queries are
  slow even on page 0 (median ~14 s over 10 manufacturers), so per-manufacturer slicing (32,808 manufacturers via
  `/eos`) is slower than plain paging.
- **Sort: use the default order (send no `sort`).**
  - The default order is index-backed, cheap on shallow pages, and identical across repeated fetches of the same page.
  - A real sort (`primaryDi,asc|desc`, `uuid,asc`) is honoured and repeatable, but adds a fixed ~7 s to *every* request,
    even page 0. That rules out the trick of fetching the second half in reverse order.
  - Unknown sort fields (`versionDate`, `lastUpdateDate`, anything else) are silently ignored, so "same as default"
    does not mean the server sorted by that field. `lastUpdateDate` is null in the search items.
- **Throughput:** 12 random pages per level, `size=300`, run while other probes were also hitting the API:

  | concurrency | median per request | wall time per page | full run (15,399 pages) | failures after retries |
  |---|---|---|---|---|
  | 1 | 20.2 s | 22.6 s | ~97 h | 0/12 |
  | 2 | 21.4 s | 11.8 s | ~50 h | 0/12 |
  | 4 | 16.7 s | 6.2 s | **~27 h** | 0/12 |

  Per-request latency did not rise from 1 to 4 workers. No 429s were seen, because deep list pages are slow enough
  that 4 workers stay under the rate limit (about 12 requests per minute). Fast endpoints do hit it; see below.
- **Completeness:** offset paging over a run of about a day can shift if devices are added or removed mid-run. Read
  `totalElements` at the start and the end, de-duplicate by `uuid`, and compare the unique count to `totalElements`.
  Only trust an empty page when `page >= totalPages`; `get()` already retries the spurious empty ones.
- **Reuse and data protection (not fully checked):** the EUDAMED-specific legal notice was not found. The general
  Commission reuse policy (Decision 2011/833/EU) allows reuse unless a site says otherwise. Search-list rows are mostly
  company data, but actor rows from `/actors` include named people (PRRC names, emails), which brings in GDPR.

### Recommendation for the scraper
1. First call the official `/udi`. If it returns rows, use it (cursor via `nextLink`, 1000 rows per page, CSV) and skip the rest.
2. Otherwise, page `/devices/udiDiData` in the default order with `size=300`, `page=0..totalPages-1`, using
   `check_api.get()` in a `ThreadPoolExecutor(4)`. Write one JSONL file per page and skip pages already on disk, so
   the run can be resumed. Expect about one day.
3. Then do the completeness check above and re-fetch any pages that failed.
4. Possible speed-up, untested: page each of the 13 `riskClassCode` slices separately. If latency follows the offset
   *within* a slice, total cost falls to about a third (Σ slice² / total²). The one comparison made so far (class-I
   page 7000 against global page 7000) was inconclusive, so measure a full slice before relying on it.

## Rate limit (measured 2026-10-28)
- After a 2-minute cooldown, a burst of **20** fast requests (UDI-DI detail) succeeds. The 21st gets
  `429` with `Retry-After: 60`, and so does the list search. The bucket is shared by every endpoint.
- One request every 3 s, 4 s or 6 s: 30 out of 30 OK. One request per second starts failing after about 17
  requests. So the sustained budget is roughly 20 per minute. 1 per 2 s has not been tested.
- `check_api.get()` retries 429 after `Retry-After`. `check_api.min_interval` paces every request
  across threads, and `scrape.py --rate` sets it (default 18 per minute).
- **What this means for a full scrape at ~18 per minute** (about 26k requests per day):
  - list: 15,399 requests. Bounded by deep-page latency, so about 1 day, as measured above.
  - Basic UDI-DI: one request per distinct Basic UDI-DI. That is about 16% of UDI-DIs in a 900-row sample, so about
    740k requests, about **4 weeks**.
  - UDI-DI detail: 4.6M requests, about **6 months**.

## `scrape.py`
It keeps raw gzipped JSONL in `EUDAMED/data/` (gitignored) and builds `data/eudamed.sqlite` from it. Every phase can
be resumed:
- Finished page files are skipped.
- Files are written to `.tmp` and then renamed.
- A page that fails transiently is left for the next run.
- A 4xx on a single item is written as an `{"uuid"|"ulid": …, "error": …}` record.
```sh
python3 -u EUDAMED/scrape.py official      # /udi still empty? (if not, switch to the official API)
python3 -u EUDAMED/scrape.py list          # data/list/page-NNNNN.jsonl.gz + meta.json (totals at start/end)
python3 -u EUDAMED/scrape.py detail        # data/detail/page-NNNNN.jsonl.gz, one detail per list row
python3 -u EUDAMED/scrape.py basic         # data/basic/…, each Basic UDI-DI once, on the first list page it appears on
python3 -u EUDAMED/scrape.py actors        # data/actors.jsonl.gz from the official /actors (PRRC person fields dropped)
python3 -u EUDAMED/scrape.py build         # data/eudamed.sqlite: udi_di, udi_cnd, basic_udi_di, actor
python3 -u EUDAMED/scrape.py verify        # coverage report, exits 1 on gaps
```
- Options: `--pages 0-2` limits any phase to those list pages, `--workers` defaults to 4, and `--rate` defaults to 18
  requests per minute.
- Each table keeps the full raw object in a `json` or `detail_json` column. Query anything else with `json_extract` or
  `json_each`. For example, markets are in `json_each(detail_json, '$.marketInfoLink.msWhereAvailable')`.
- `python3 EUDAMED/test_scrape.py` is an offline self-check of `build` and the Basic UDI-DI assignment.

## `check_api.py`
```sh
python3 EUDAMED/check_api.py                         # tradeName=catheter, size 300
python3 EUDAMED/check_api.py --tradeName '' --pageSize 25   # empty query
python3 EUDAMED/check_api.py --dump some/dir          # also save raw JSON for analysis
```
It runs search → device → Basic UDI-DI → actor and prints one line per check:
- `PASS`: the documented keys are present and the request behaved as the spec says.
- `DEVIATION`: the endpoint answered, but keys are missing or the paging or page size is not as documented.
- `FAIL`: the endpoint errored even after retries.

Each line is followed by notes, such as which params matter. The script exits 1 on any FAIL. It uses only the
standard library.

## `probe_scale.py`
`python3 -u EUDAMED/probe_scale.py [--samples 12]` (about 30 min). It reuses `check_api.get()` and prints one line
each for: the official API's `/udi` status, deep-page latency, order stability and sort cost, and throughput at
concurrency 1, 2 and 4 with an estimate for a full run.
