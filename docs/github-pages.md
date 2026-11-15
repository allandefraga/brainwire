# Preview and publish the documentation

GitHub Pages hosts the public Brainwire documentation. The build combines the
landing pages in `docs-site/` with HTML generated from the Markdown sources in
`docs/`. The site does not run the Brainwire server or store graph data.
Historical implementation plans are kept in the repository source and excluded
from the Pages build.

## Preview on your computer

Use Python 3.12 from the repository root. Build into a new or empty directory.

```sh
python3.12 -m venv .venv-docs
.venv-docs/bin/python -m pip install -r scripts/docs-site-requirements.txt
.venv-docs/bin/python scripts/build_docs_site.py /tmp/brainwire-docs-preview
.venv-docs/bin/python scripts/check_docs_site.py /tmp/brainwire-docs-preview
.venv-docs/bin/python -m http.server 8000 --directory /tmp/brainwire-docs-preview --bind 127.0.0.1
```

Open `http://127.0.0.1:8000`. Stop the preview with `Ctrl+C`. This serves the
site only on your computer's loopback address.

## Download a preview from a private repository

The **Documentation site** workflow runs on pushes to `main` and can also be
started from **Actions → Documentation site → Run workflow**. It generates the
site and checks its local links, assets, and heading anchors.

For a private repository, the workflow uploads the generated site as an Actions
artifact named `brainwire-docs-preview-<commit>`, retained for seven days.

To inspect it:

1. Open a successful **Documentation site** run in the repository's Actions tab.
2. Under **Artifacts**, download `brainwire-docs-preview-<commit>`.
3. Extract the archive into an empty directory.
4. Run `python3 -m http.server 8000 --directory /path/to/extracted-site --bind 127.0.0.1`
   and open `http://127.0.0.1:8000`.

Artifact download requires access to the repository and its workflow run. If an
artifact has expired, run the workflow again.

## Publish with GitHub Pages

The current workflow publishes only when the repository is public. After the
checks pass, GitHub uploads the generated site and the `deploy` job publishes
it. Check the reported URL in the deploy job to confirm publication.

Before enabling a hosted site, check its intended audience. GitHub Pages sites
can be public even when their source repository is private. GitHub Free supports
Pages from public repositories; private-repository Pages requires a supporting
plan such as Pro, Team or Enterprise. Private website access controls have
additional Enterprise Cloud organization requirements. Do not change repository
visibility merely to make a documentation job pass.

When publication is intended and supported:

1. Open the repository's **Settings → Pages**.
2. Under **Build and deployment**, select **GitHub Actions** as the source.
3. Confirm that the workflow's visibility condition matches the intended setup.
   This repository currently keeps automatic deployment limited to public source.
4. Run **Documentation site**, then check the `deploy` job and its reported URL.

Publishing from private source would require an explicit workflow and access
policy change; the artifact path above remains available without that change.
Review the generated content and links before publishing.

See GitHub's [Pages quickstart](https://docs.github.com/en/pages/quickstart),
[publishing-source setup](https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site),
[site creation and visibility guidance](https://docs.github.com/en/pages/getting-started-with-github-pages/creating-a-github-pages-site),
and [workflow artifact downloads](https://docs.github.com/en/actions/managing-workflow-runs-and-deployments/managing-workflow-runs/downloading-workflow-artifacts).
