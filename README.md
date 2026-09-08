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
charon http kb.pl --persist           # write assertz/retract changes back to the file
charon check kb.pl                    # load, report, exit — for CI
charon openapi kb.pl > openapi.json   # print the document and exit
```

`--persist` works on every subcommand. `--stdio` exists only under `mcp`, and cannot be combined
with `--host`/`--port`.

The default bind address is loopback. There is **no authentication**: passing `--host 0.0.0.0`
makes the whole knowledge base queryable by anyone who can reach the port, and Charon logs a
warning when you do.

Diagnostics go to stderr on every subcommand — including `openapi`, so redirecting stdout gives
you a document and not a document with a log line in it. `--log` takes a `tracing` filter
(`--log debug`, `--log charon=trace,warn`); `RUST_LOG` overrides it.

If the knowledge base fails to load, Charon exits rather than starting: a failed consult in
scryer leaves *every* predicate undefined, so a server that started anyway would answer nothing.

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
`readOnlyHint`/`idempotentHint`/`destructiveHint` annotation rather than being stripped. The
knowledge base's source is offered as a resource at `charon://source`.

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
| `any` | any scalar or array | strings become atoms, numbers stay numbers |

Declaring the element type is what lets `?Drugs=["diazepam"]` unify with ordinary atom facts. Only
`+` (input) and `-` (output) modes can be exposed; `?` and `@` cannot, and neither can `compound`,
`stream`, or a custom type. If a documented predicate does not appear, the server says why at
startup — `charon check kb.pl` prints the same report without binding anything.

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

### Exceptions

Charon wraps every generated goal in `catch/3`, so a Prolog exception is reported as an error
response and the interpreter stays usable. Knowledge bases do not need their own guards.

Arguments are interpolated into generated Prolog source, and every value is escaped — quotes,
backslashes and control characters included — so text that closes its own quote comes back as
text rather than being executed.

### Title and description

A `/** <module> Title ... */` comment anywhere in the file sets the OpenAPI `info` block and the
MCP server identity.

### Persistence

With `--persist`, `assertz` and `retract` against a `:- dynamic(name/arity).` predicate are
mirrored back into the source file after every call. Only single-line ground fact clauses are
rewritten; rules, multi-line clauses and anything else are left exactly as written. The file is
replaced atomically (write to a sibling temporary file, then rename), so an interrupted write
cannot leave a truncated knowledge base. Charon checks the file is writable at startup rather
than after the first change.

## Testing

```sh
cargo test
cargo test exception_does_not_poison_later_requests
```

## License

MIT — see [LICENSE](LICENSE).
