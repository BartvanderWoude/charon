# Charon

Charon exposes a Prolog knowledge base as an HTTP/JSON API. A `.pl` file is loaded into an
embedded [scryer-prolog](https://github.com/mthom/scryer-prolog) engine; any predicate documented
with a [PlDoc](https://www.swi-prolog.org/pldoc/man?section=modes) `%!` comment becomes an HTTP
endpoint, and an OpenAPI description of those endpoints is served alongside them.


## Requirements

- A recent stable Rust toolchain (edition 2024).
- Network access on first build: `scryer-prolog` is pulled in as a git dependency and compiled
  from source, which takes a while.

## Building

```sh
cargo build
```

## Writing a knowledge base

Any predicate you want exposed needs a PlDoc mode line directly above its clause(s):

```prolog
%! my_length(+List:list, -Length:int) is det.
%
%   Length is the number of elements in List.
%
%   @arg List The list to measure.
%   @arg Length The resulting length.
my_length(List, Length) :- length(List, Length).
```

No Rust changes are needed — the predicate is picked up automatically the next time the server
starts. If an expected endpoint doesn't show up in `/openapi.json`, it's usually because one of
its arguments uses an unsupported mode (`?`/`@`) or type (anything other than `atom`, `int`/
`integer`, `float`/`number`, `list`, or `any`).

A file-level `/** <module> Title ... */` comment (see `association.pl` for an example) sets the
API's title and description in `/openapi.json`.


## Running

```sh
cargo run -- path/to/file.pl
```

This loads `file.pl`, parses out its documented predicates, and serves them on
`0.0.0.0:3000`. 

Then, with the server running:

```sh
curl http://localhost:3000/openapi.json
```

lists every exposed endpoint, and each documented predicate is callable at
`GET /{predicate_name}` with its input arguments passed as query parameters.

## Testing

```sh
cargo test
```

To run a single test:

```sh
cargo test test_parse_basic_pldoc
```

## License

MIT — see [LICENSE](LICENSE).
