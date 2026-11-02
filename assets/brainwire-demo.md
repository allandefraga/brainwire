# Animated Cypher walkthrough

`brainwire-demo.gif` is a 12-second looping illustration of a supported Cypher
pattern over synthetic customer/segment records. It is not a recording of the
TUI, a live database session, a benchmark or an agent answering questions.

The diagram represents these synthetic relationships:

- Ada → IN_SEGMENT → beta
- Maya → IN_SEGMENT → beta
- Noah → IN_SEGMENT → early access

The query shown is:

```cypher
MATCH (c:Customer)-[:IN_SEGMENT]->(s:Segment)
WHERE s.name = 'beta'
RETURN c.name, s.name
```

The illustrated rows are Ada/beta and Maya/beta. They follow from the fixture;
no query timing, latency or performance claim is shown. The animation does not
promise a client rendering interface. Customer, Segment and IN_SEGMENT match
the README demo ontology.

For README use, embed `assets/brainwire-demo.gif` with a caption identifying it
as an animated illustration over synthetic data. `brainwire-demo.mp4` is the
small H.264 alternative for sharing; `brainwire-demo-poster.png` is static.

Regenerate on macOS with SF system fonts, Pillow and ffmpeg available:

```sh
uv run --no-project --with pillow python assets/render_brainwire_demo.py
```

The renderer writes only these three asset files and removes its temporary PNG
frames. It does not start a database or access application data.
