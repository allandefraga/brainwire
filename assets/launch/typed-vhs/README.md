# Recreate the typed graph demo

This recording runs the published v0.24.0-beta.1 CLI with synthetic data.
VHS and Brainwire images are pinned by digest in the Dockerfile. From the
repository root, with Docker running:

```sh
docker build -t brainwire-vhs-demo:local assets/launch/typed-vhs
docker run --rm --network none --memory 1536m \
  -v "$PWD/assets/launch/typed-vhs:/vhs" \
  brainwire-vhs-demo:local /vhs/demo.tape
```

The tape creates a graph in the disposable container, reads it and shows a
type error. It writes `brainwire-demo.gif` and `brainwire-demo.mp4` to this
directory. The build needs access to the two public images; the recording does
not need network access.
