# Brainwire on macOS

Use the native CLI for a personal graph on your Mac. It can open a local
RocksDB directory and run Cypher without starting a server or a container.
Use [Docker Compose](container.md) when multiple processes need a Bolt
endpoint or you want a pinned Linux image with a persistent volume.

## Install with Homebrew

The public [Homebrew tap](https://github.com/allandefraga/homebrew-brainwire)
currently installs the experimental `v0.24.0-beta.2` CLI from source, along
with a sample ontology. It needs Apple Command Line Tools or Xcode and may
take several minutes to compile. There are no prebuilt bottles yet.

```sh
brew install allandefraga/brainwire/brainwire
brew test allandefraga/brainwire/brainwire
"$(brew --prefix allandefraga/brainwire/brainwire)/bin/brainwire" --version
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
brainwire_prefix="$(brew --prefix allandefraga/brainwire/brainwire)"
ontology="$brainwire_prefix/share/brainwire/company-ontology.json"
export BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=8MiB
"$brainwire_prefix/bin/brainwire" cypher --database "$demo_dir/graph" --ontology "$ontology" \
  --query "CREATE (:Project {entity_id: 'p1', name: 'Release'})"
"$brainwire_prefix/bin/brainwire" cypher --database "$demo_dir/graph" \
  --query "MATCH (p:Project {entity_id: 'p1'}) RETURN p.name"
```

The `rows` array contains `[["Release"]]`. For a persistent graph, choose
a data directory outside the Homebrew installation, such as a dedicated
directory under `$HOME/.local/share/brainwire`. An offline CLI invocation
needs exclusive access to its graph. For concurrent clients use
[`brainwire serve`](serve.md) and connect over Bolt.

For Python agent memory and Markdown vaults, see the [agent quickstart](agent-context-quickstart.md)
and [Obsidian example](../examples/obsidian/README.md). They use a running Bolt
server and their own Python dependencies.
