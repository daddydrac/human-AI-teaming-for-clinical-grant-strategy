---
title: Create a shared grant
description: Use the Grantspace wizard to capture a sponsor ask, configure the workflow, and generate a first draft.
---

<div class="guide-layout">
<aside class="toc">
<strong>On this page</strong>
<a href="#start-or-open">Start or open</a>
<a href="#describe-the-grant">Describe the grant</a>
<a href="#add-the-sponsor-ask">Add the sponsor ask</a>
<a href="#choose-the-workflow">Choose the workflow</a>
<a href="#choose-model-routing">Choose model routing</a>
<a href="#create-and-draft">Create and draft</a>
</aside>
<article class="guide-content" markdown="1">

<div class="eyebrow">Grant creation</div>
# Create a shared grant

The wizard saves one workflow configuration for the whole team. Existing grants reopen with their persisted files, sections, versions, guidance, and decisions.

## Start or open

From **Grants & wizard**, choose one action:

- **Create a new shared grant** for a new proposal.
- **Open an existing grant** to resume saved work.
- **Import a local project** when you have a compatible Grantspace export.

## Describe the grant

Enter the working title, sponsor, mechanism, deadline, and grant type. These fields orient drafting but do not replace the authoritative sponsor document.

## Add the sponsor ask

Upload the RFA, NOFO, RFI, or application form; enter its public URL; or paste the text. Add supporting sponsor guidance and approved institutional materials when they should influence the draft.

<figure class="screenshot">
  <img src="{{ '/assets/images/grant-wizard.png' | relative_url }}" alt="Grantspace wizard showing the sponsor source upload screen">
  <figcaption>Use the source format that preserves the complete sponsor instructions most reliably.</figcaption>
</figure>

<div class="callout warning"><strong>Check the source before drafting.</strong> Scanned PDFs, access-controlled URLs, and incomplete pasted text may omit requirements. Review the normalized grant ask in the saved workspace.</div>

## Choose the workflow

The five core outcomes stay available:

1. Analyze the grant ask.
2. Build the research plan.
3. Capture the aims.
4. Organize supporting evidence.
5. Draft, review, approve, and publish the proposal.

Optional tools are off by default. Include only the tools the team wants, such as clinical design, compliance, institutional memory, funder fit, competitive intelligence, deadline automation, or synthetic review. An included optional tool adds a workspace capability but does not secretly gate unrelated work.

When **Review simulator and causal critique** is skipped, the wizard also skips review configuration. When it is included, select the advisory review depth; reviewer roles are derived later from the approved solicitation.

## Choose model routing

Choose `local_only`, `hybrid`, or `claude_only` within the limits enabled by the deployment. The selected policy is stored with the grant and enforced on its model calls.

## Create and draft

On **Workflow preview**, confirm the grant, selected tools, collaborators, and routing policy. **Create shared grant** then:

1. validates and persists the project;
2. saves the authoritative sponsor source;
3. derives the initial section outline;
4. drafts sections through bounded model requests;
5. assembles and verifies the saved proposal.

Stay on the progress screen until drafting succeeds or a specific error is shown. Local generation on an 8 GB M2 is expected to take longer than cloud or larger-GPU generation.

<div class="callout"><strong>Humans remain in control.</strong> Generated text is a starting draft. Save corrections, use guidance to request rewrites, and publish only the version your team accepts.</div>

</article>
</div>

