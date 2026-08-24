# Grantspace clinician guide

This directory is the source for the Grantspace GitHub Pages site. Jekyll builds
the Markdown guides while Playwright renders fictional interface walkthroughs
from repository-owned HTML and CSS.

## Render walkthrough images

```bash
python3 scripts/render_screenshots.py
```

Run the command from this directory. It writes PNG files to `assets/images/`.
Every screenshot uses fictional people, organizations, grants, and email
addresses; never replace them with credentials or protected grant content.

## Preview locally

```bash
bundle install
bundle exec jekyll serve
```

Open `http://127.0.0.1:4000`. GitHub Actions builds and deploys this directory
when guide files change on the default branch.

