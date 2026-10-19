# Local releases and private delivery

## From a change to a version

1. Develop and test the change locally. Push to `main` runs **CI**; a tag push
   runs **Release**, which calls the same source-test suite before packaging.
2. Review the CI result before treating the change as ready. The repository
   currently has **no branch protection rule requiring CI to pass**, so GitHub
   reports failures but does not block a direct push or merge to `main`.
3. On a clean local `main`, run `python3 scripts/release.py` to preview the
   suggested version. `auto` maps breaking-change commits to major, `feat:` to
   minor, and other changes to patch. Review the output; use `--bump` to
   override it, or `--prerelease beta` / `--prerelease rc` for a prerelease.
4. Once the preview is right, `python3 scripts/release.py --apply` runs the
   local checks and creates a versioned commit plus annotated tag. It refuses
   to run on a dirty worktree. Inspect both before sending them to GitHub.
5. Only after you choose to publish, push the `main` commit and release tag
   together. CI runs on `main`; the tag starts Release, which reruns CI,
   builds and tests the exact image, then publishes that versioned image and
   creates the matching GitHub release.

The automation files and local helper exist, but the current remote state is
**not fully green**: the latest CI run on `b6a3eb4` failed while linking Rust
test binaries (`rust-lld` terminated with a bus error). The `v0.1.1` release
also failed on GitHub's runner before publication. A local workflow change now
limits Cargo to one build job and removes debug/incremental artifacts to reduce
runner pressure; it remains uncommitted and still needs a successful GitHub
run. See [the CI run](https://github.com/allandefraga/brainwire/actions/runs/39230467885)
and [the failed v0.1.1 release](https://github.com/allandefraga/brainwire/actions/runs/39229131214).

Brainwire versions follow Semantic Versioning. The Rust workspace version is
the source of truth; the local release command updates `Cargo.lock`, the pinned
Kubernetes overlay, and only the versioned install examples marked for updates.
Compose and Swarm use the same `BRAINWIRE_IMAGE` tag when supplied.

```sh
# Preview the next version. Nothing is modified.
python3 scripts/release.py

# Choose a bump explicitly when needed.
python3 scripts/release.py --bump patch
python3 scripts/release.py --bump minor
python3 scripts/release.py --bump major

# Prepare a beta candidate, for example v0.1.2-beta.1.
python3 scripts/release.py --prerelease beta

# After reviewing the preview and committing/stashing other work:
python3 scripts/release.py --apply --prerelease beta
```

The default `auto` lane follows commit messages since the previous release tag:
breaking changes (`type!:` or `BREAKING CHANGE:`) suggest a major bump, `feat:`
suggests minor, and every other delivery uses patch. `--bump` overrides that
suggestion. A beta/RC patch increments the same candidate series; a feature
starts a new minor candidate. During `0.x`, a declared breaking change promotes
to `1.0.0`; this is only a version number and does not claim production
readiness.

`--apply` refuses a dirty working tree, updates the managed version files, runs
the same Rust/Python checks as CI, then creates a local release commit and an
annotated Git tag. It never pushes or publishes. This protects unrelated local
work and keeps release tags immutable. It only prepares releases from `main`.
If checks fail, managed files are restored. After publishing, fix forward with
a new version instead of moving or reusing the old tag. The private GitHub workflow reruns source checks, builds the exact
versioned image, tests that container through persisted-volume restarts, and
only then pushes the tested image to the private GHCR package and creates a
GitHub release. A `-beta.N` or `-rc.N` SemVer suffix makes the GitHub release a
prerelease; a plain `MAJOR.MINOR.PATCH` tag makes it a stable release.

When you decide to send a local release to the still-private GitHub repository,
push the release commit and tag together:

```sh
git push origin main --follow-tags
```

The release workflow reruns CI, builds and loads the `linux/amd64` image,
verifies Python/Bolt writes and recovery after container restarts, then pushes
that same local image to GHCR. It writes the published digest and source commit
to the GitHub Actions summary and attaches release metadata to the GitHub
release. No floating `latest` tag is created. The repository and package remain
private; a machine pulling the image needs a GitHub token with package read
access.

Rerunning a completed tag verifies its release metadata and GHCR digest without
replacing them. If an earlier run pushed the tested image but stopped before
creating its GitHub release, the workflow resumes only when the image's source
revision matches that exact tag commit. A tag that points to a different image
revision is rejected for safety.

To test a local image without GHCR, build it with the version tag and set that
tag for Compose:

```sh
docker build -t brainwire:v0.1.2 .
BRAINWIRE_IMAGE=brainwire:v0.1.2 docker compose -p brainwire-demo \
  -f deploy/compose.demo.yaml up --wait --wait-timeout 900
```

Release version strings in the README and deployment guides are updated by the
helper only where the `brainwire-release-version` marker appears. Historical
version references, including the pre-license `v0.1.1` source tag, are kept as
history.
