# Brainwire on macOS

Use the native CLI for a personal graph on your Mac. It can open a local
RocksDB directory and run Cypher without starting a server or a container.
Use [Docker Compose](container.md) when multiple processes need a Bolt
endpoint or you want a pinned Linux image with a persistent volume.

## Install from the local tap

The repository contains a [Homebrew formula](../packaging/homebrew/Formula/brainwire.rb)
for the experimental `v0.24.0-beta.1` release. This source formula builds
the CLI and installs a sample company ontology. It needs Apple Command Line
Tools or Xcode and may take several minutes to compile. A public tap and
prebuilt Homebrew bottles are not yet published.

Run this from a checkout containing the formula. Keep the local tap directory
for as long as you want to use it with Homebrew.

```sh
tap_dir="$(mktemp -d "$HOME/brainwire-homebrew.XXXXXXXX")"
mkdir "$tap_dir/Formula"
cp packaging/homebrew/Formula/brainwire.rb "$tap_dir/Formula/"
git -C "$tap_dir" init
git -C "$tap_dir" add Formula/brainwire.rb
git -C "$tap_dir" -c user.name=Brainwire -c user.email=build@example.invalid \
  commit -m 'Add Brainwire formula'
brew tap brainwire-local/tap "$tap_dir"
brew install brainwire-local/tap/brainwire
brew test brainwire-local/tap/brainwire
brainwire --version
```

Homebrew may install or update build dependencies. The fully qualified
formula name selects this local tap. To use `brew install brainwire` by its
short name, first run `brew trust --formula brainwire-local/tap/brainwire`.

## Create a graph and query it

```sh
demo_dir="$(mktemp -d)"
ontology="$(brew --prefix brainwire)/share/brainwire/company-ontology.json"
export BRAINWIRE_MEMORY_CACHE=64MiB BRAINWIRE_MEMORY_MEMTABLES=8MiB
brainwire cypher --database "$demo_dir/graph" --ontology "$ontology" \
  --query "CREATE (:Project {entity_id: 'p1', name: 'Release'})"
brainwire cypher --database "$demo_dir/graph" \
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
