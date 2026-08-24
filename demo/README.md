# Demo portable project

Upload `RUNTIME-VALIDATED-grantspace-demo-project.zip` to **Start → Import a local project**.

The archive contains one valid root file named `grantspace-project.json`, as required by the importer. It creates a shared fictional grant with its funding-opportunity document plus six editable, ordered proposal sections. Each section has a substantive starting draft and explicit team-input markers, so the collaborative editor, renaming, reordering, comments, guided rewrites, version switching, and publishing can be tested immediately.

The checked-in archive targets the workflow registry in this source tree. Rebuild the core service after pulling code changes before importing it into an older running deployment.

Regenerate the archive after changing its demo content:

```bash
python3 demo/generate_demo_portable_project.py
```
