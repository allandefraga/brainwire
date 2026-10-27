# Releases

Brainwire uses Semantic Versioning. During the `0.x` beta series, minor
versions may introduce features and patch versions contain compatible fixes.
Prereleases use tags such as `v0.7.0-beta.1` and are not production
certifications.

## Prepare a version locally

Start from a clean `main` checkout. Review the changes and run the checks for
the release before preparing a tag:

```sh
git status --short
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 -m unittest discover -s examples/python -p 'test_*.py'
python3 -m unittest discover -s scripts -p 'test_*.py'
python3 scripts/check_docs_site.py
```

Preview the helper's SemVer suggestion:

```sh
python3 scripts/release.py
```

The helper maps breaking-change commits to a major bump, `feat:` commits to a
minor bump, and other changes to a patch bump. You can choose the bump directly
or start a prerelease series:

```sh
python3 scripts/release.py --bump patch
python3 scripts/release.py --bump minor
python3 scripts/release.py --bump major
python3 scripts/release.py --prerelease beta
```

After reviewing the preview, prepare the local release commit and annotated
tag:

```sh
python3 scripts/release.py --apply --prerelease beta
git show --stat --oneline HEAD
git show v0.7.0-beta.1 --no-patch
```

`--apply` requires a clean `main` checkout, updates the workspace version,
lockfile, both Kubernetes release overlays and marked documentation references,
runs the release checks, then creates a local commit and tag. It does not push
or publish. The helper refuses to move
or reuse an existing tag; publish fixes as a new version.

## Publish a reviewed release

Push the reviewed commit and tag together only after the repository is ready
for that release:

```sh
git push origin main --follow-tags
```

The release workflow must pass before treating the version as available. It
rechecks the source, builds a versioned container image, exercises Python/Bolt
writes and restart recovery against the image, then publishes the tested image
and attaches its source revision and digest to the release. A `-beta.N` or
`-rc.N` tag is a prerelease. No floating `latest` tag is created.

The Docker image uses the same version as the Git tag. Pulling a private image
requires a registry token; a public image can be pulled without logging in.
Repository visibility and package visibility are separate settings, so check
both before announcing a public release.

The documentation workflow publishes `docs-site/` to GitHub Pages when the
repository is public. In **Settings → Pages**, select **GitHub Actions** as the
publishing source once; each validated push to `main` then deploys the site.

## Test an image locally

Build and run the local quickstart without publishing to a registry:

```sh
docker build -t brainwire:v0.7.0-beta.1 .
BRAINWIRE_IMAGE=brainwire:v0.7.0-beta.1 docker compose -p brainwire-demo \
  -f deploy/compose.demo.yaml up --wait --wait-timeout 900
```

The helper updates only references marked `brainwire-release-version`; old
version references in historical examples remain as history.
