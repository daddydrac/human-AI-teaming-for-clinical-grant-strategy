---
title: Simulate review and publish
description: Run a solicitation-grounded synthetic review, resolve revision work, and publish the approved grant.
---

<div class="guide-layout">
<aside class="toc">
<strong>On this page</strong>
<a href="#before-review">Before review</a>
<a href="#build-the-panel">Build the panel</a>
<a href="#read-the-results">Read the results</a>
<a href="#causal-analysis">Causal analysis</a>
<a href="#revise">Revise</a>
<a href="#publish">Publish</a>
</aside>
<article class="guide-content" markdown="1">

<div class="eyebrow">Decision support</div>
# Simulate review and publish

When the Review simulator tool is enabled, Grantspace freezes a proposal snapshot, applies reviewer roles derived from the approved solicitation, validates the returned critiques, and assembles an advisory panel summary.

## Before review

Confirm that the solicitation profile, rubric, and proposal sections reflect the current sponsor ask. A review run is immutable and remains tied to the proposal snapshot it evaluated.

## Build the panel

Choose the review depth configured for the grant. Derive roles from the approved solicitation and versioned registry, inspect their criterion mappings, then approve the panel plan before execution.

Each synthetic reviewer runs independently. The consensus pass receives validated reviews—not private chain-of-thought—and preserves meaningful disagreement.

## Read the results

<figure class="screenshot">
  <img src="{{ '/assets/images/review-publish.png' | relative_url }}" alt="Grantspace synthetic review summary with panel findings and revision backlog">
  <figcaption>Every usable critique should point to a solicitation criterion and proposal location.</figcaption>
</figure>

Review:

- criterion-specific strengths and weaknesses;
- proposal and solicitation anchors;
- score distributions when the rubric is numeric;
- panel disagreements and confidence;
- prioritized revision tasks.

<div class="callout warning"><strong>Synthetic means advisory.</strong> The simulator does not represent named real reviewers, reveal private deliberations, predict an award, or replace sponsor and institutional review.</div>

## Causal analysis

For intervention or causal claims, inspect the proposed graph, assumptions, threats, and mitigations. A methodologist can correct the model and confirm a new version. Unconfirmed model output stays labeled as inferred.

## Revise

Accept useful findings into the revision backlog, assign an owner, and link the work to the affected section. Resolve the task only after the saved proposal contains the agreed correction.

Run another simulation against a new snapshot when the proposal changes materially; do not overwrite the old run.

## Publish

Choose the green **Publish grant · DOCX + PDF** button from the editor. Before publishing:

<ul class="checklist">
  <li>Read the complete assembled document in order.</li>
  <li>Confirm that required sections and attachments are represented.</li>
  <li>Verify claims, citations, budget references, and deadlines.</li>
  <li>Resolve or explicitly accept outstanding team guidance.</li>
  <li>Confirm the final human-approved version.</li>
</ul>

Download the generated DOCX and PDF and perform the sponsor portal's own validation before submission.

</article>
</div>

