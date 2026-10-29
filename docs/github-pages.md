# Preview and publish the documentation

GitHub Pages turns the static `docs-site/` folder into a website. It does not
run the Brainwire server or store graph data. The Markdown guides in `docs/`
remain repository files; the site links to them.

## Preview on your computer

From the repository root:

```sh
python3 scripts/check_docs_site.py
python3 -m http.server 8000 --directory docs-site --bind 127.0.0.1
```

Open `http://127.0.0.1:8000`. Stop the preview with `Ctrl+C`. This serves the
site only on your computer's loopback address.

## Download a preview from a private repository

The **Documentation site** workflow runs on pushes to `main` and can also be
started from **Actions → Documentation site → Run workflow**. It first checks
local site links and assets.

For a private repository, a successful validation is followed by the
`private-preview` job. It uploads the static site as an Actions artifact named
`brainwire-docs-preview-<commit>`, retained for seven days. It does not create a
Pages deployment or change repository access.

To inspect it:

1. Open a successful **Documentation site** run in the repository's Actions tab.
2. Under **Artifacts**, download `brainwire-docs-preview-<commit>`.
3. Extract the archive into an empty directory.
4. Run `python3 -m http.server 8000 --directory /path/to/extracted-site --bind 127.0.0.1`
   and open `http://127.0.0.1:8000`.

Artifact download requires access to the repository and its workflow run.
Repository access also controls the linked Markdown guides. If an artifact has
expired, run the workflow again. An artifact preview is a downloadable build,
not a hosted documentation URL.

## Publish with GitHub Pages

The current workflow publishes only when the repository is public. In that
case, its `build` and `deploy` jobs use GitHub's Pages actions after validation.
The private-preview job is skipped. A passing validation job alone does not
mean that a site was published.

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
A Pages site also does not make a private repository or its linked guides
readable to every site visitor. Review the content and links before publishing.

See GitHub's [Pages quickstart](https://docs.github.com/en/pages/quickstart),
[publishing-source setup](https://docs.github.com/en/pages/getting-started-with-github-pages/configuring-a-publishing-source-for-your-github-pages-site),
[site creation and visibility guidance](https://docs.github.com/en/pages/getting-started-with-github-pages/creating-a-github-pages-site),
and [workflow artifact downloads](https://docs.github.com/en/actions/managing-workflow-runs-and-deployments/managing-workflow-runs/downloading-workflow-artifacts).
