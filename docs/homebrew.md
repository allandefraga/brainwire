# Brainwire on macOS

Choose the Homebrew package when you want to open a graph on your Mac and run
Cypher directly from the terminal. The `brainwire` binary handles this on its
own, without Docker, a running server, or a Python environment.

Choose Docker Compose when you want a reproducible Linux server for an
application or several clients. The server owns the graph's RocksDB directory,
and clients connect over Bolt. A named volume keeps the data when the container
stops or is replaced. [Docker guide](container.md)

## Install with Homebrew

The public [Homebrew tap](https://github.com/allandefraga/homebrew-brainwire)
currently installs the experimental `v0.24.0-beta.2` CLI from source, along
with a sample ontology. It needs Apple Command Line Tools or Xcode and may
take several minutes to compile. There are no prebuilt bottles yet.

```sh
brew install allandefraga/brainwire/brainwire
brainwire_prefix="$(brew --prefix allandefraga/brainwire/brainwire)"
export PATH="$brainwire_prefix/bin:$PATH"
brainwire --version
```

Homebrew may install or update build dependencies. The formula compiled and
passed `brew test` on Apple Silicon in an isolated Homebrew prefix using an
existing Rust toolchain; automatic dependency installation and Intel Macs have
not been tested. The fully qualified name selects the public tap. To use
`brew install brainwire` by its short name, tap and trust it first:

```sh
brew tap allandefraga/brainwire
brew trust --formula allandefraga/brainwire/brainwire
brew install brainwire
```

## Create a graph and query it

```sh
demo_dir="$(mktemp -d)"
ontology="$brainwire_prefix/share/brainwire/company-ontology.json"
brainwire cypher --database "$demo_dir/graph" --ontology "$ontology" \
  --query "CREATE (:Project {entity_id: 'p1', name: 'Release'})"
brainwire cypher --database "$demo_dir/graph" \
  --query "MATCH (p:Project {entity_id: 'p1'}) RETURN p.name"
```

The `rows` array contains `[["Release"]]`. Both commands use the same graph
directory, `$demo_dir/graph`. For a graph you want to keep, choose a permanent
directory outside the Homebrew installation, such as a dedicated directory
under `$HOME/.local/share/brainwire`.

Only one process can open a RocksDB graph directory at a time. When multiple
clients need access, run `brainwire serve` from the same native installation or
use Docker Compose. Connect clients over Bolt and keep offline `brainwire cypher`
commands away from graph directories the server has open. [Native server configuration](serve.md)

Python is optional. Use it if you are writing a Python application or running
the Python examples. Those clients connect to a running Bolt server, either
native or in Docker. [Python client guide](python-client.md)
