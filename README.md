# Charon

Charon exposes a Prolog knowledge base as an API. A `.pl` file is loaded into an embedded
[scryer-prolog](https://github.com/mthom/scryer-prolog) engine; any predicate documented with a
[PlDoc](https://www.swi-prolog.org/pldoc/man?section=modes) `%!` comment becomes callable over
whichever interface you start:

- `charon http` — an **HTTP/JSON API**, described by an OpenAPI 3.1 document at `/openapi.json`
- `charon mcp` — an **MCP server**, over HTTP at `POST /mcp` or over stdio with `--stdio`

Both are rendered from one description of one operation set, so a predicate exposed on either is
exposed identically on the other. Which one a process speaks is chosen when it starts, so what a
port answers is a property of how you launched it rather than something a client discovers by
probing.

## Requirements

- A recent stable Rust toolchain (edition 2024).
- Network access on first build: `scryer-prolog` is a git dependency compiled from source, which
  takes a while.

## Building

```sh
cargo build
```

## Running

```sh
charon http kb.pl                     # REST + /openapi.json on 127.0.0.1:3000
charon http kb.pl --port 8080         # a different port
charon http kb.pl --host 0.0.0.0      # reachable from other machines (see below)
charon mcp  kb.pl                     # MCP at POST /mcp on 127.0.0.1:3000
charon mcp  kb.pl --stdio             # MCP over stdin/stdout, for a desktop client
charon http kb.pl --persist           # allow changes, and write them back to the file
charon check kb.pl                    # load, report, exit — for CI
charon openapi kb.pl > openapi.json   # print the document and exit
```

`--persist` works on every subcommand. `--stdio` exists only under `mcp`, and cannot be combined
with `--host`/`--port`. Two more options are shared by every subcommand: `--max-solutions N`
bounds how many solutions one call collects (default 1000; `0` removes the bound), and
`--interpreters N` sets how many Prolog machines answer in parallel.

Sending `SIGHUP` to a running server re-reads the `.pl` file and swaps in a fresh set of
operations. A reload that fails leaves the previous version serving, so a half-saved file is not
an outage. There is no HTTP endpoint for this on purpose: a signal can only be sent by someone
who can already signal the process, while a route could be reached by anyone who can reach the
port.

The default bind address is loopback. There is **no authentication**: passing `--host 0.0.0.0`
makes the whole knowledge base queryable by anyone who can reach the port, and Charon logs a
warning when you do.

Diagnostics go to stderr on every subcommand — including `openapi`, so redirecting stdout gives
you a document and not a document with a log line in it. `--log` takes a `tracing` filter
(`--log debug`, `--log charon=trace,warn`); `RUST_LOG` overrides it.

If the knowledge base fails to load, Charon exits rather than starting: a failed consult in
scryer leaves everything after the offending clause undefined, so a server that started anyway
would answer existence errors. This is detected with a sentinel clause consulted after the file,
which has no blind spot — probing the documented predicates does, because a bad clause for a
predicate that *also* has good clauses leaves it looking perfectly well defined.

### Using it as an MCP server

```json
{
  "mcpServers": {
    "charon": {
      "command": "/path/to/charon",
      "args": ["mcp", "/path/to/knowledge_base.pl", "--stdio"]
    }
  }
}
```

Tool names are the predicate names. A `get_`/`put_`/`delete_` prefix becomes a
`readOnlyHint`/`idempotentHint`/`destructiveHint` annotation rather than being stripped — and,
since a prefixed predicate declares an intention to change something, such a tool is only listed
at all when the server was started with `--persist`.

Beyond tools, the server offers:

- **resources** — the knowledge base's own source at `charon://source`, so a model can read the
  rules it is calling rather than take the tool descriptions on faith, plus one resource per
  declared collection (and a template for reading a single member);
- **prompts** — `knowledge_base_overview`, an orientation built from the module documentation, the
  tool list and the collections;
- **completions** — `completion/complete` answers an argument's admissible values from the
  knowledge base itself, for any argument with a `@domain`.

The HTTP transport refuses cross-origin requests. A browser cannot read from a loopback port, but
it can be steered into posting to one, and Charon has no authentication to fall back on.

## Writing a knowledge base

Any predicate you want exposed needs a PlDoc mode line directly above its clauses:

```prolog
%! pim_check(+Age:int, +Drugs:list(atom), -Substance:atom, -Reason:string) is nondet.
%
%   Enumerates one solution per criterion triggered by a patient's medication list.
%
%   @arg Age Age in completed years.
%   @arg Drugs The substances to check.
%   @arg Substance The substance a criterion fired on.
%   @arg Reason Why it fired.
pim_check(Age, Drugs, Substance, Reason) :-
    Age >= 65,
    member(Substance, Drugs),
    pim(Substance, Reason).
```

### Argument types

The declared type decides how JSON becomes a Prolog term, in both directions and at every depth.

| Declared | JSON | Prolog |
| --- | --- | --- |
| `atom` | string | `'value'` — unifies with plain facts like `drug(aspirin)` |
| `string` (or `text`) | string | `"value"` — a character list |
| `int` / `integer` | number | integer |
| `float` / `number` | number | float |
| `bool` / `boolean` | boolean | `true` / `false` |
| `list(T)` | array | list of `T`, recursively |
| `list` | array | same as `list(any)` |
| `compound` | `{"functor": …, "args": […]}` | `dose(100, mg)` — nests to any depth |
| `any` | any scalar, array or compound | strings become atoms, numbers stay numbers |

Declaring the element type is what lets `?Drugs=["diazepam"]` unify with ordinary atom facts. Only
`+` (input) and `-` (output) modes can be exposed; `?` and `@` cannot, and neither can `stream` or
a custom type. If a documented predicate does not appear, the server says why at startup —
`charon check kb.pl` prints the same report without binding anything.

A `compound` argument travels in the same shape in both directions, so a structured answer can be
handed straight back as a structured argument.

The table reads both ways, including the row that has no Prolog counterpart. Prolog has no boolean
type — `true` and `false` are ordinary atoms — so an output declared `bool` is answered as a JSON
boolean rather than as the word, which is what the schema for that argument promises. An output
declared `bool` and bound to something else is left exactly as it came, so a mode line that does
not match its clauses stays visible instead of being quietly rounded to `true` or `false`.

A zero-arity predicate is written without parentheses: `%! ready is semidet.`

### Where arguments go

`GET` and `DELETE` take their arguments in the query string, `POST`/`PUT`/`PATCH` in a JSON body,
and that is what the OpenAPI document describes. At runtime both are accepted for every method,
so `curl -G` and `curl --json` both work — but giving the same argument twice is an error rather
than a silent precedence rule.

A query string has no types, so `?Age=82` is read as an integer because `Age` was declared `int`.
A JSON body is already typed and is passed straight through.

### Responses

One solution is shaped by the predicate's output arguments:

- **no output arguments** → `true`
- **one output argument** → that value, unwrapped
- **several** → an object keyed by argument name, in the order the mode line declares them

Whether the response is that solution or an **array** of them follows the *declared determinism*,
not how many solutions turned up:

| Declared | Response |
| --- | --- |
| `det` | the solution |
| `semidet` | the solution, or `null` (`false` if there are no output arguments) |
| `nondet`, `multi`, `failure`, undeclared | an array, one entry per solution, `[]` if the query failed |

Deciding this from the declaration is what makes it unambiguous. When the shape depended on the
answer count, one solution binding `[1, 2]` and two solutions binding `1` and `2` came back as
exactly the same JSON, and no client could tell them apart. It also means the OpenAPI response
schema is exact rather than a `oneOf` covering every shape the runtime might reach.

### Errors

Charon wraps every generated goal in `catch/3`, so a Prolog exception is reported as an error
response and the interpreter stays usable. Knowledge bases do not need their own guards.

Error responses are [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457) problem details, served as
`application/problem+json`, with a stable `type` a client can branch on instead of parsing prose:

```json
{
  "type": "urn:charon:argument-invalid",
  "title": "Invalid argument",
  "status": 422,
  "detail": "invalid argument `Values`: expected an integer, found a string",
  "argument": "Values"
}
```

The status says whose problem it is. A request that could not be read at all is `400`; one that
was well-formed but asked for something the rules could not do — a bad argument, or an exception
the knowledge base raised about this request — is `422`; a knowledge base calling a predicate
nobody defined is `500`, because no change to the request can fix it.

Arguments are interpolated into generated Prolog source, and every value is escaped — quotes,
backslashes and control characters included — so text that closes its own quote comes back as
text rather than being executed.

### Title and description

A `/** <module> Title ... */` comment anywhere in the file sets the OpenAPI `info` block and the
MCP server identity.

### Changing the knowledge base

A predicate whose name begins `put_`, `post_`, `patch_` or `delete_` declares an intention to
change something. **Charon exposes none of them unless it was started with `--persist`.**

Without the flag such an operation is absent from the OpenAPI document and from the MCP tool
list, and calling it anyway answers `403` — naming the flag, rather than pretending the operation
does not exist. `charon check kb.pl` lists what is being withheld, so a missing endpoint is never
a mystery.

The reason is that a change without persistence is worse than no change at all: `assertz` commits
to the running machine whether or not anyone writes it down, so the server quietly stops agreeing
with the file it claims to serve, and the difference disappears on the next restart.

Note that this reads the *prefix*, not what the predicate does. A `post_` predicate that computes
something and changes nothing is still withheld; if it does not change anything, it does not need
the prefix.

### Persistence

With `--persist`, `assertz` and `retract` against a `:- dynamic(name/arity).` predicate are
mirrored back into the source file after every call. Only single-line ground fact clauses are
rewritten; rules, multi-line clauses and anything else are left exactly as written. The file is
replaced atomically (write to a sibling temporary file, then rename), so an interrupted write
cannot leave a truncated knowledge base. Charon checks the file is writable at startup rather
than after the first change.

### Collections

An enumerating predicate — one whose arguments are all outputs — is usually not an operation but
a table. One tag says so, and both interfaces learn about it:

```prolog
%! consent_module(-Module:atom) is multi.
%
%   @collection consent_module key(Module)
%   @arg Module The module's short name.
consent_module(research).
consent_module(biobank).
```

Over HTTP that yields a collection and its members:

| Route | |
| --- | --- |
| `GET /consent_module` | every member |
| `GET /consent_module/{Module}` | one member, or `404` |
| `PUT /consent_module/{Module}` | add one — idempotent |
| `DELETE /consent_module/{Module}` | remove one, answering with what was removed |

Over MCP the same declaration becomes a **resource** — `charon://collection/consent_module`,
with a template for `charon://collection/consent_module/{Module}` — rather than another tool. A
model is meant to read resources for context and call tools to act; making it spend a tool call to
see a table it could have been handed gets that backwards.

The table is what the predicate is exposed *as*: it is not additionally called by name, so there
is no second path and no tool answering the same rows. The collection may therefore be named
after the predicate, as above, or given a name of its own — `@collection consent_modules
key(Module)` puts the same table at `/consent_modules`, and `consent_module` itself is then not a
route.

A member's body carries the columns that are *not* in the key, shaped by the usual rule: nothing
to say answers `true`, one column answers that column's value, several answer an object.

The write routes exist only when the predicate is declared `dynamic` — `assertz` against a static
predicate raises a permission error, so offering the route would be a promise Charon cannot keep —
and they are `PUT`/`DELETE`, so `--persist` gates them like any other changing operation.

### Argument domains

`@domain` says where an argument's admissible values come from, naming either a collection or a
`name/arity` predicate:

```prolog
%! put_consent_module(+Module:atom, -Added:atom) is det.
%
%   @domain Module consent_module
```

Charon answers MCP `completion/complete` for that argument from the knowledge base itself, so the
values offered are whatever the rules currently say rather than a list somebody maintained. When
the underlying predicate is *not* `dynamic`, the values cannot change while the server runs, so
they are also written into the argument's JSON Schema as an `enum` — which means a wrong value is
refused, on both interfaces, before a goal is ever built.

### Bounded answers

A knowledge base can describe an endless relation, and collecting every solution of one would
exhaust memory while holding the interpreter's lock — taking every other request with it. One
call therefore collects at most `--max-solutions` solutions (1000 by default). A capped answer
carries `X-Charon-Truncated: true` over HTTP and `structuredContent.truncated` over MCP, so it
cannot be mistaken for a complete one.

This bounds a relation that *yields* endlessly. It cannot bound a goal that never yields at all
(`loop :- loop.`), because control never returns to Charon: scryer offers no way to interrupt a
running goal, so there is no honest way to cap that from outside.

### Parallelism

A Prolog machine is a lock, so with one of them every request in the process waits for every
other. When the source contains no `assertz`, `asserta`, `retract`, `retractall` or `abolish`,
Charon runs several machines — `--interpreters N`, defaulting to the core count capped at four —
consulted from the same text, answering in parallel.

A knowledge base that changes itself gets exactly one machine, and so does `--persist`. That is
decided by reading the source rather than from a flag, so it is not something that can be switched
on incorrectly: a change reaches the machine that made it and no other.

## Testing

```sh
cargo test
cargo test exception_does_not_poison_later_requests
```

## License

MIT — see [LICENSE](LICENSE).
