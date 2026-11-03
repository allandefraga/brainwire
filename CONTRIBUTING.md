# Contributing to Brainwire

Bug reports, documentation fixes and focused code changes are welcome.
Brainwire is an experimental beta; start with a small change whose behavior can
be explained and verified. Check the [current limits](README.md#current-limits)
before proposing compatibility or performance claims.

## Report a problem

Use the [bug report](https://github.com/allandefraga/brainwire/issues/new?template=bug_report.yml)
and include the release tag or commit, operating system, CPU architecture,
installation command and a minimal reproduction with synthetic data. Include
expected and actual output. For a container problem, include Docker/Compose
versions and sanitized startup logs. Never attach credentials, customer data,
private queries or a real database volume.

For a suspected security issue, follow [SECURITY.md](SECURITY.md).
For a feature, describe the workload and what is difficult today in a
[feature request](https://github.com/allandefraga/brainwire/issues/new?template=feature_request.yml).

## Make a change

1. Follow the [local setup](docs/mvp-quickstart.md) and create a focused branch.
2. Read the code's callers and existing tests. Add a regression test for a bug
   or a behavior test for non-trivial new logic.
3. Update the README or relevant guide when behavior changes. Use English for
   code, comments and documentation; use generic examples and synthetic data.
4. Run the relevant checks and describe their results in the pull request.

The main Rust checks are:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

Fetch dependencies first with `cargo fetch --locked` if they are not cached.
Some tests start temporary processes and bind loopback sockets. Use an isolated
development environment and never point acceptance tests at a database you keep.

For Python changes, install the [local client](docs/python-package.md) and run:

```sh
python -m unittest discover -s examples/python -p 'test_*.py'
python scripts/check_docs_links.py
python scripts/check_docs_site.py
```

Report skipped or unavailable checks. Benchmark results need a reproducible
fixture, exact versions, resource settings and limitations; a local result does
not establish a production guarantee. See [benchmarks](BENCHMARKS.md).

## Review and releases

Keep pull requests focused and explain the user-visible result, validation and
remaining risks. Maintainers manage release tags and publishing through the
[release process](docs/releasing.md). Do not include generated credentials,
local databases or unrelated changes.

Contributions are provided under the repository's [Apache-2.0 license](LICENSE).
There is no guaranteed review or support response time.
