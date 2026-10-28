# EUDAMED public API — how it actually behaves

Observed on 2026-10-28 by running `check_api.py`. The older community spec at
https://openregulatory.github.io/eudamed-api/ is mostly right but wrong in a few places that matter (see Quirks).
This is the undocumented JSON backend behind the EUDAMED public website, not an official API, so it can change.

## Basics
- Base URL: `https://ec.europa.eu/tools/eudamed/api`. GET only. No auth. JSON responses.
- **Send your own `User-Agent`.** The default `Python-urllib/x.y` agent gets `502 Bad Gateway`. Plain curl works.
- Expect flakiness. Always retry: see Quirks for the empty pages and the random 500/502 errors.

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
