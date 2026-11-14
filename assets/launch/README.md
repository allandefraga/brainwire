# Brainwire live demo

The [20-second MP4](brainwire-live.mp4) and [looping GIF](brainwire-live.gif)
show the agent-memory example writing a project decision and retrieving its
source. They then show the connected terminal's Ontology, Graph and Memory
panes over Bolt. The fixture is synthetic and the data store is disposable.

- [Write the decision](agent-remember.png)
- [Recall the decision and its source](agent-recall.png)
- [Inspect the ontology](tui-ontology.png)
- [Follow outgoing project relationships](tui-graph.png)
- [Inspect query reservations](tui-memory.png)

The Python frames use the output of the checked-in agent-memory example. The
captions omit its ephemeral loopback endpoint from the displayed command and
shorten long IDs. Terminal frames are ANSI cells captured from the actual
connected TUI. The version printed on each frame identifies the server binary
used to record it. The edit holds each captured frame for several seconds.

On macOS, with `uv`, `ffmpeg` and a local Brainwire binary available, regenerate
from the repository root:

```sh
uv run --no-project --with 'neo4j==5.28.4' --with 'pyte==0.8.2' --with pillow \
  python assets/launch/capture.py --binary target/debug/brainwire
```

Use `--mono` and `--sans` to supply compatible fonts on a different system.
The script starts its own authenticated server on loopback and generates
temporary credentials and a temporary RocksDB directory. It checks that the
recalled decision and source IDs match the write, confirms the expected panes
and removes the server and state at the end.
