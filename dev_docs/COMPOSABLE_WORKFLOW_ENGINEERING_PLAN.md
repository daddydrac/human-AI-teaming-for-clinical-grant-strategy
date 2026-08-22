# Composable Grant Workflow and Review Simulator — Engineering Plan

Status: proposed for product/engineering review  
Date: 2026-08-22  
Repository: Clinical Grant Workbench

## 1. Outcome

Replace the fixed nine-tab workbench as the default experience with a project-creation wizard that composes the actual workflow for each grant.

Every project always includes five core outcomes:

1. Read and analyze the RFA/RFI/NOFO or other grant ask.
2. Generate and approve the research-plan framework.
3. Capture and approve the researcher's key aims.
4. Complete a literature search derived from both the solicitation and aims.
5. Generate, review, approve, and export the proposal.

Optional modules selected in the wizard are persisted on the project, inserted into its workflow, displayed in its navigation, and included in its completion gates. Unselected modules must be absent from both the UI and backend prerequisites. The current detailed workbench remains available only when the user selects the advanced-workbench option.

The same program will also provide a shared grant workspace, versioned team editing, project chat, approval routing, and a solicitation-grounded simulated review panel with causal analysis.

### 1.1 Non-negotiable requirements from the approved workflow brief

This plan explicitly retains all requirements in the attached workflow brief:

| Brief requirement | Engineering-plan location |
|---|---|
| Wizard opens before the existing interface | Section 4 |
| Create or open a shared grant | Wizard Screen 1 |
| Five core stages cannot be removed | Sections 1, 4, 5, and 6 |
| Selections determine navigation, stages, and gates | Sections 4 and 5 |
| Unselected modules are not shown or secretly required | Sections 1, 5, 12, and 16 |
| Existing grants restore the same workflow for every teammate | Sections 4, 5, and Phase 2 |
| RFA summary, eligibility, requirements, criteria, deadlines, attachments, and human questions | Sections 4 and 6.1 |
| Editable framework with criterion mapping, argument, evidence gaps, inputs, and ownership | Section 6.2 |
| Structured, approved aims with fact/estimate/assumption distinctions | Section 6.3 |
| Literature research driven by both the RFA and approved aims | Section 6.4 |
| Evidence by aim/section, citations, contradictions, quality, gaps, and audit trail | Section 6.4 |
| Draft → team review → revisions → approval | Sections 6.5 and 8 |
| Final proposal contains only exact approved versions | Sections 6.5, 9, and 16 |
| Full current workbench is an explicit optional view | Sections 1, 4, 11, and 15 |
| Slack-like project chat and section threads | Section 8.3 |
| Mentions, assignments, review requests, presence, notifications, and activity | Sections 8.3 and 9 |
| Named edit authorship and role-based approval | Sections 8.2, 8.3, and 9 |
| Concurrent stale-edit detection and reconciliation | Sections 6.5, 8.3, and 13 |
| Compare and restore without deleting history | Sections 6.5, 8.3, 9, and 13 |
| One shared server rather than isolated local databases | Section 8.1 |
| Durable differentiation from a generic Claude project | Section 1.2 below |

### 1.2 Product boundary: AI engine versus grant system

Claude or another configured model may perform selected reasoning and drafting tasks, but the model is replaceable infrastructure. The product differentiation that must remain outside any single model conversation is:

- sponsor-derived, enforced workflow state;
- versioned and human-approved framework and aims;
- requirement/aim/section evidence relationships;
- exact source and citation provenance;
- deterministic compliance and submission validation;
- shared team ownership, discussion, assignments, and approvals;
- safe concurrent editing, comparison, and rollback;
- reproducible assembly from exact approved versions;
- a persistent audit record separating human, deterministic, and AI actions.

Acceptance and architecture reviews should reject implementations that collapse these records into chat history or rely on a model to enforce them.

## 2. Important pre-implementation checkpoint

There is a paused, incomplete prototype in the current worktree touching:

- `core/src/main.rs`
- `core/src/storage.rs`
- `ui/app.py`

Those changes began adding collaboration tables/routes and wizard helpers before the workflow was clarified. They are not a completed implementation and have not been validated. Before Phase 1, review them line by line and either:

1. retain only pieces consistent with this plan behind tests, or
2. revert the partial prototype and implement from the approved data model below.

Do not build new phases on an uncompiled intermediate state.

## 3. Product research translated into requirements

The research is used as product-pattern input, not as a requirement to reproduce the vendors' full products.

### 3.1 Cayuse patterns to adopt

Cayuse emphasizes fewer manual errors, a centralized source of truth, approval routing and notifications, audit-ready activity/reporting, AI-assisted drafting, and workflows that scale. Its current article also frames proactive validation and centralized information as risk controls.

Relevant implementation decisions:

- Keep one authoritative project record for requirements, dates, documents, decisions, and approvals.
- Extract deadlines and deliverables once, then generate tasks and reminders rather than requiring repeated entry.
- Route framework, aims, sections, compliance, and final submission through named owners and approvers.
- Validate required fields and gates before users advance.
- Record every state transition in an append-only event stream.
- Provide a project health view: overdue tasks, blocked gates, stale evidence, pending approvals, and submission risks.
- Separate deterministic automation from model-generated recommendations.

Source: [Cayuse, “5 Ways Automation Improves Grant Management”](https://www.cayuse.com/blog/5-ways-automation-improves-grant-management/).

### 3.2 Instrumentl patterns to adopt

Instrumentl connects discovery, funder research, proposal work, institutional memory, task/deadline tracking, document storage, permissions, and collaboration. It also uses funder context to refine recommendations and reuses prior approved content.

Relevant implementation decisions:

- Add an optional **Opportunity and funder fit** module to the wizard.
- Generate a transparent fit assessment from mission, eligibility, geography, program area, award size, deadline, prior awards, and the organization's capabilities.
- Add an optional **Institutional memory** module containing approved snippets, prior proposals, bios, capabilities, facilities, and reusable evidence.
- Reuse only human-approved material and retain its origin and last-review date.
- Extract tasks, milestones, and deadlines from the solicitation into the shared project tracker.
- Make funder context an explicit input to framework generation and review simulation.
- Never describe fit as a probability of winning unless a separately validated predictive model exists.

Sources: [Instrumentl platform overview](https://www.instrumentl.com/product-overview), [Instrumentl Apply](https://www.instrumentl.com/capability/apply), and [Instrumentl home page](https://www.instrumentl.com/).

### 3.3 Submittable patterns to adopt

Submittable emphasizes configurable forms, eligibility checks, customizable multi-stage review, reviewer scoring, real-time collaboration, in-app communication, and full-lifecycle oversight.

Relevant implementation decisions:

- Generate solicitation-specific intake fields and eligibility checks.
- Add a configurable **Review simulator** module to the wizard.
- Represent review as multiple stages and multiple reviewer roles, not one generic prompt.
- Generate a rubric from the solicitation and require every score and critique to point to a rubric item and proposal evidence.
- Support individual reviews followed by a simulated panel/consensus pass.
- Keep real team comments distinct from simulated reviewer comments.
- Design assignments, scorecards, and review statuses so real internal reviewers can use the same underlying workflow later.

Sources: [Submittable grant management software](https://www.submittable.com/solutions/grants) and [Submittable platform](https://www.submittable.com/).

### 3.4 Authoritative review-process patterns

The review simulator must use the solicitation's stated criteria first. For NIH mechanisms, the criteria and factor structure vary by activity code and due date. NIH's simplified framework for many research project grants centers review on importance of the research, rigor/feasibility, and expertise/resources; NIH also instructs applicants to consult the NOFO for additional criteria.

Implementation decisions:

- Version rubric templates by sponsor, mechanism, and effective date.
- Treat the parsed solicitation as authoritative when it adds or overrides criteria.
- Keep scored criteria, additional review criteria, compliance checks, and mission/programmatic considerations separate.
- Never imply that a simulated score predicts an actual panel score or award decision.

Sources: [NIH peer-review background](https://grants.nih.gov/policy-and-compliance/policy-topics/peer-review/simplifying-review/background), [NIH initial review policy](https://grants.nih.gov/grants/policy/nihgps/HTML5/section_2/2.4.1_initial_review.htm), and [NIH application-section advice](https://grants.nih.gov/grants-process/write-application/advice-on-application-sections).

## 4. Wizard specification

The wizard is a large modal/lightbox and is the only default entry point. It is not a cosmetic onboarding overlay.

### Screen 1 — Start

- Create a new shared grant.
- Open an existing grant.
- Import/migrate an existing local project.
- Capture the user's authenticated identity and organization.

Opening an existing grant skips configuration and loads its persisted workflow.

### Screen 2 — Grant ask

- Working title, sponsor, mechanism, and deadline if known.
- Upload, URL, or paste for RFA/RFI/NOFO/application form.
- Supporting guidance and sponsor strategic-plan files/URLs.
- Grant type: research, clinical trial, implementation, training, center/program, foundation, RFI response, or custom.
- Privacy/routing notice showing which selected model providers will receive content.

### Screen 3 — Core workflow

Show the five mandatory steps, their outputs, and approval points. They are visible but locked on.

### Screen 4 — Optional modules

Each card must state where it appears, what it produces, prerequisites, runtime/data implications, and whether it adds a completion gate.

Recommended cards:

| Module | Placement | Adds a gate? | Main output |
|---|---|---:|---|
| Opportunity and funder fit | Before RFA commitment | No | Fit/mismatch analysis and go/no-go decision |
| Structured investigator interview | Between framework and aims lock | Yes | Resolved information gaps with provenance |
| Clinical design and statistics | After aims | Yes | Typed study design and feasibility assessment |
| Institutional memory | Cross-cutting | No | Approved reusable facts, snippets, and files |
| Competitive applicant intelligence | After literature search | Optional by configuration | Evidence-backed differentiation strategy |
| Sponsor compliance and submission | Intake through export | Yes | Rules, attachment checklist, and preflight |
| Review simulator and causal critique | After first complete draft, repeatable | Yes only if selected as required | Individual reviews, panel summary, causal findings, revision plan |
| Team collaboration and approvals | Cross-cutting | Yes when approval rules are configured | Chat, assignments, comments, named approvals |
| Deadline and task automation | Cross-cutting | No | Owners, due dates, reminders, escalation |
| Full advanced workbench | View option | No | Existing diagnostic and specialist controls |

Provide presets such as **Lean research proposal**, **Clinical study**, **Full institutional submission**, and **RFI/strategy response**. Presets only preselect cards; the final configuration remains inspectable.

### Screen 5 — Review configuration

Shown only when the review simulator is selected:

- Sponsor/mechanism rubric template detected from the grant ask.
- Reviewer roles proposed from the solicitation.
- Review modes: quick red-team, full individual panel, consensus panel, causal-methods review.
- Whether reviewer identities are role archetypes or organization-specific archetypes.
- Explicit notice: simulated reviewers are synthetic and are not representations of named real people.
- Choose whether a passing simulated review is advisory or a required project gate.

### Screen 6 — Team and routing

- Invite team members or generate an administrator-mediated invite link.
- Assign project owner, PI, scientific writer, statistician/methodologist, research administrator, and approver roles.
- Choose required approval points.
- Set target submission date and derive internal deadlines.
- Configure notifications.

### Screen 7 — Workflow preview and creation

Render the exact ordered workflow, cross-cutting capabilities, approval gates, model-routing/privacy summary, and estimated expensive operations. The user confirms this composition before the project is created.

Configuration is persisted server-side. UI visibility must always be derived from that configuration, never from browser-only state.

## 5. Composable workflow architecture

### 5.1 Why the current stage model must change

`core/src/workflow.rs` currently defines one totally ordered `Stage` enum. `core/src/main.rs` and `core/src/storage.rs` also hard-code interview, clinical, competitive-intelligence, compliance, and review prerequisites. The current readiness expression requires every module.

That architecture cannot support optional modules safely. Hiding tabs without changing those gates would create dead-end projects.

### 5.2 Target model

Introduce a registry of workflow step definitions:

```rust
pub struct WorkflowStepDefinition {
    pub key: String,
    pub version: u32,
    pub placement: StepPlacement,
    pub required_by_default: bool,
    pub prerequisites: Vec<GateRef>,
    pub outputs: Vec<ArtifactType>,
    pub ui_surface: String,
}
```

Project configuration stores enabled step keys, versions, order, required/advisory status, and per-module options. A gate evaluator computes step state from persisted artifacts:

```text
not_started → available → in_progress → awaiting_review → complete
                                      ↘ blocked
```

Rules:

- Only enabled steps may add prerequisites or readiness failures.
- The five core definitions are always enabled.
- Cross-cutting modules do not need artificial positions.
- Step completion is derived from authoritative records, not manually toggled.
- Gate responses return machine-readable blocker codes plus user-facing remediation.
- Workflow changes after project creation require an impact preview and audit event.
- Removing a module hides it from active workflow but never deletes its historical data.

### 5.3 Backward compatibility

- Map legacy projects to a `legacy_full_workbench_v1` workflow with all existing modules enabled.
- Keep `Stage` as a read-only compatibility projection during migration.
- Move endpoint guards one operation at a time to the new gate evaluator.
- Remove the legacy readiness expression only after migration tests prove parity for legacy projects.

## 6. Core workflow implementation

### 6.1 RFA analysis

Reuse:

- `ui/app.py` file extraction and intake controls.
- `ingestion/app.py` rendered URL ingestion.
- requirement decomposition and exact provenance in `core/src/main.rs`, `storage.rs`, and `source_locator.rs`.
- sponsor rule compilation in `compliance.rs`.

Add:

- A versioned RFA summary artifact.
- A normalized solicitation profile: sponsor, mechanism, purpose, eligibility, review rubric, required sections, dates, budget, attachments, and open questions.
- Distinct statuses for model-extracted, deterministically located, human-corrected, and human-approved facts.
- A generated intake form for missing solicitation-specific fields.

Completion gate: authoritative solicitation ingested; requirements and review rubric human-approved.

### 6.2 Research-plan framework

Add a first-class, versioned framework rather than treating `default_sections.json` as the plan.

Each framework node contains:

- section/subsection key and order;
- sponsor requirement and review-criterion mappings;
- narrative purpose and key argument;
- linked aims;
- evidence needs;
- missing investigator input;
- owner and approver;
- target length/page allocation;
- dependencies.

The UI needs outline editing, drag/reorder, mapping warnings, version history, and explicit lock/approval. Framework approval creates/updates `project_sections` without discarding section history.

Completion gate: every mandatory requirement and scored rubric item maps to at least one framework node; framework version approved.

### 6.3 Core aims capture

Separate core aims from the optional full clinical-study model. The current `ClinicalStudy` object is too broad to be a mandatory aims primitive.

Core aim fields:

- overall objective;
- central hypothesis or thesis;
- aim title and statement;
- rationale;
- approach summary;
- expected outcome;
- impact;
- innovation;
- dependencies;
- fact/estimate/assumption classification;
- supporting evidence IDs.

Preserve version history and an aims-level approval. The clinical module extends, but does not replace, this model.

Completion gate: at least one aim, no unresolved required fields, aims version approved.

### 6.4 Literature research

Refactor the existing research planner so its query plan is derived from:

- approved RFA requirements and rubric;
- approved framework;
- approved aims;
- known evidence gaps;
- optional clinical and competitive contexts.

Add search-plan approval, deduplication, source-quality rules, per-aim/criterion evidence views, contradiction handling, and a reproducible research manifest. Keep the current exact-excerpt validation and citation provenance.

Completion gate: required evidence needs are supported, explicitly waived, or marked as unresolved risks; research run is fresh relative to RFA/framework/aim versions.

### 6.5 Draft, review, approval, and export

Draft each section only from the exact approved upstream versions recorded in a compilation manifest. Continue append-only section versions and exact-version approvals.

Add:

- author and base-version IDs;
- restored-from version ID;
- section status and assignment;
- inline comments anchored to version/range;
- diff/compare view;
- stale-edit conflict UI with three-way merge assistance;
- final snapshot manifest containing all upstream artifact versions.

Core completion gate: all required sections approved and final deterministic checks for enabled modules pass.

## 7. Review simulator

### 7.1 Purpose and boundaries

The simulator is a structured red-team tool. It must not claim to impersonate actual reviewers, forecast award probability, or know private deliberations. It simulates role-based review behavior from public evidence and the solicitation.

Every output must distinguish:

- solicitation-derived criterion;
- public sponsor/funder context;
- proposal-derived observation;
- model inference;
- unsupported or unavailable information.

### 7.2 Reviewer panel construction

Create a `ReviewerPanelPlan` from the RFA/RFI, sponsor guidance, public strategic priorities, stated review process, and grant type. Suggested archetypes include:

- scientific importance/mission reviewer;
- methods, rigor, and feasibility reviewer;
- clinical/statistical reviewer when applicable;
- implementation/community-impact reviewer when applicable;
- investigator/team/resources reviewer;
- research-administration/compliance reviewer;
- program officer or mission-portfolio perspective.

The engine may infer which roles are useful, but the user approves the panel before execution. Do not create personas named after real staff or claim their private preferences.

### 7.3 Review pipeline

1. Freeze a proposal snapshot and its upstream version manifest.
2. Compile the solicitation rubric and reviewer instructions.
3. Generate each review independently to reduce artificial consensus.
4. Require structured strengths, weaknesses, questions, evidence anchors, and criterion scores.
5. Run deterministic validation: criterion coverage, score range, anchor validity, and prohibited claims.
6. Run a panel pass that sees the validated individual reviews, not hidden chain-of-thought.
7. Produce a simulated summary statement, disagreement map, score distribution, and prioritized revision backlog.
8. Link accepted revision tasks to affected framework nodes and sections.
9. Preserve the run as immutable and rerun only against a new proposal snapshot.

### 7.4 Causal analysis

Support two distinct modes:

1. **Program/argument causality:** Does the proposed chain from need → activity/intervention → mechanism → output → outcome → impact make sense?
2. **Causal-study validity:** If the proposal claims an effect, can the design identify that effect under stated assumptions?

Build a human-editable causal graph with nodes for intervention/exposure, population, outcome, mediators, moderators, confounders, selection, measurement, and context. Edges must point to proposal or literature evidence.

Checks include:

- temporal ordering;
- consistency between aims, estimand, intervention/exposure, endpoints, and analysis;
- missing common causes and residual confounding;
- selection and attrition mechanisms;
- conditioning on colliders or post-treatment variables;
- measurement validity and differential misclassification;
- positivity/overlap and sample support;
- interference and treatment-version assumptions;
- transportability from study population to target population;
- alternative causal explanations;
- sensitivity analyses and negative controls where applicable;
- whether causal language exceeds what the design can support.

The simulator outputs:

- causal DAG/logic-model visualization;
- explicit assumptions register;
- claim-to-identification table;
- critical versus addressable threats;
- proposed design/analysis mitigations;
- reviewer-facing questions;
- section-specific revision suggestions.

The model proposes the graph and findings; the UI must label them as inferred until a researcher/methodologist confirms them.

### 7.5 Structured result contract

```json
{
  "snapshot_id": "...",
  "rubric_version_id": "...",
  "panel_plan_id": "...",
  "reviews": [{
    "reviewer_archetype": "methods_and_feasibility",
    "criterion_scores": [{
      "criterion_id": "...",
      "score": 0,
      "strengths": [],
      "weaknesses": [],
      "proposal_anchors": [],
      "solicitation_anchors": [],
      "confidence": 0.0
    }],
    "overall_assessment": "...",
    "questions": []
  }],
  "causal_analysis": {
    "mode": "causal_study_validity",
    "graph": {"nodes": [], "edges": []},
    "assumptions": [],
    "threats": [],
    "claim_checks": []
  },
  "panel_summary": {},
  "revision_tasks": []
}
```

Scores must remain nullable when a solicitation uses narrative or categorical review.

## 8. Collaboration and shared deployment

### 8.1 Deployment model

Separate local copies cannot collaborate automatically. The supported collaborative topology is:

```text
Browsers → TLS reverse proxy/SSO → one UI/API deployment → shared database/object storage
                                             ↘ model/research services
```

The UI may remain Gradio for the first increment, but the service must run on a shared host. Keep `core`, renderer, ingestion, database, and model ports private; expose only the authenticated gateway. The current loopback-only Compose bindings are safe for local use but do not provide team access.

### 8.2 Identity and authorization

Do not treat a display-name textbox as authentication. Implement:

- OIDC/SAML-capable identity at the gateway;
- stable user and organization IDs;
- roles: owner, PI, contributor, reviewer, approver, research administrator, viewer;
- project-level membership and section/task assignments;
- server-side authorization on every project route;
- secure invite lifecycle and revocation;
- audit records for role and membership changes.

### 8.3 Collaboration features

- Per-project general channel.
- Framework/aim/section threads.
- Inline version-anchored comments.
- Mentions, assignments, and review requests.
- Activity feed and notifications.
- Append-only section versions with optimistic locking.
- Rollback by creating a new version copied from an older version.
- Compare any two versions and show author/AI source.
- Named, role-checked approval records.
- Presence is advisory; edit safety relies on base-version checks.

Start with polling through the existing Gradio timer pattern. Add SSE/WebSocket delivery only after the event and authorization model is stable.

## 9. Persistence changes

Use explicit SQLite migrations; retain SQLite/WAL for a bounded single-node deployment. Define a migration path to PostgreSQL before multi-instance scaling.

Proposed tables or equivalent normalized structures:

- `workflow_definitions`
- `project_workflows`
- `project_workflow_steps`
- `workflow_events`
- `solicitation_profiles` and history
- `review_rubrics` and history
- `research_frameworks` and history
- `research_framework_nodes`
- `project_aim_sets` and history
- `project_aims`
- `users`, `organizations`, `project_members`
- `channels`, `messages`, `comments`, `mentions`
- `tasks`, `task_dependencies`, `notifications`
- `review_panel_plans`
- `review_simulation_runs`
- `simulated_reviews`, `simulated_review_findings`, `simulated_scores`
- `causal_models`, `causal_nodes`, `causal_edges`, `causal_findings`

Extend:

- `section_versions`: `author_user_id`, `base_version_id`, `restored_from_version_id`, `generation_run_id`.
- `approvals`: `approver_user_id`, role-at-approval, decision, notes.
- export snapshots: workflow definition/version and all approved upstream artifact IDs.

Never overwrite history tables during rollback or module removal.

## 10. API changes

Representative endpoints:

```text
POST   /api/projects                         create with workflow configuration
GET    /api/projects/{id}/workflow
POST   /api/projects/{id}/workflow/impact    preview configuration change
PATCH  /api/projects/{id}/workflow           apply audited change
GET    /api/projects/{id}/workflow/status

GET/POST /api/projects/{id}/framework
POST     /api/projects/{id}/framework/{version}/approve
GET/POST /api/projects/{id}/aims
POST     /api/projects/{id}/aims/{version}/approve

GET/POST /api/projects/{id}/tasks
GET/POST /api/projects/{id}/channels/{channel}/messages
GET/POST /api/projects/{id}/sections/{section}/comments
GET      /api/projects/{id}/sections/{section}/versions
POST     /api/projects/{id}/sections/{section}/restore

POST /api/projects/{id}/review-panel/plan
POST /api/projects/{id}/review-panel/plan/{plan}/approve
POST /api/projects/{id}/review-simulations
GET  /api/projects/{id}/review-simulations/{run}
POST /api/projects/{id}/review-simulations/{run}/tasks
```

All mutating endpoints accept an idempotency key and expected/base version where applicable. Conflict responses include current version metadata.

## 11. Codebase work map

| Area | Existing files | Planned work |
|---|---|---|
| Workflow engine | `core/src/workflow.rs`, `main.rs`, `storage.rs` | Step registry, project configuration, gate evaluator, legacy adapter |
| Data contracts | `core/src/domain.rs`, new modules | Framework, aims, collaboration, review, causal contracts |
| Solicitation parsing | `main.rs`, `compliance.rs`, `source_locator.rs` | Unified solicitation profile and review rubric with provenance |
| Literature/evidence | `research.rs`, `context_compiler.rs`, retrieval files | Aim/rubric-driven plans, manifests, freshness, evidence-gap state |
| Aims/clinical | `clinical.rs`, new `aims.rs` | Extract core aims model; clinical module becomes optional extension |
| Review simulator | new `review_simulator.rs`, `causal.rs` | Panel plan, independent reviews, validator, consensus, causal graph |
| Model routing | `models.rs`, `config/mlx-runtime.in` | New task kinds, provider/privacy disclosures, bounded structured output |
| Persistence | `storage.rs` initially | Versioned artifacts, users/team, events, reviews; split modules if file grows |
| UI | `ui/app.py` | Wizard, project-derived navigation, core surfaces, module cards, review dashboard |
| Renderer | `renderer/app.py` | Framework/report views, review summary, causal diagram, diff/annotation exports |
| Deployment | `docker-compose.yml`, env templates, scripts | Authenticated shared profile, private internal ports, backups, migration checks |
| Documentation | `README.md`, `QUICKSTART.md`, `CODEBASE_INDEX.md` | Shared-host setup, workflow concepts, security and review-simulation limits |

## 12. Delivery phases

### Phase 0 — Reconcile and baseline

- Resolve the paused partial prototype.
- Run existing validation and capture baseline behavior.
- Add architecture decision records for composable gates, identity, and shared deployment.

Exit: clean compiling baseline, existing tests green, migration plan approved.

### Phase 1 — Workflow configuration and migration

- Add workflow definitions/configuration storage.
- Add gate evaluator and machine-readable statuses.
- Migrate legacy projects to full-workbench configuration.
- Refactor readiness and route guards so only enabled modules gate progress.

Exit: exhaustive module-combination tests prove no hidden module can block a project.

### Phase 2 — Wizard and dynamic navigation

- Implement the seven-screen modal.
- Persist configuration during project creation.
- Render project navigation from enabled definitions.
- Add presets, workflow preview, impact preview, and advanced-workbench option.

Exit: reopening the same grant in a different browser produces the same workflow.

### Phase 3 — Five core workflow surfaces

- Add solicitation profile, framework, and core aims artifacts.
- Refactor literature planning and drafting around approved upstream versions.
- Add core status/gate UI and compilation manifests.

Exit: a lean project completes all five core steps with every optional module disabled.

### Phase 4 — Collaboration foundation

- Add real identities, membership, RBAC, project channels, tasks, comments, authorship, diffs, conflict handling, and rollback-as-new-version.
- Add shared deployment profile and operational guidance.

Exit: two authenticated browser sessions can edit safely, detect conflicts, discuss, review, approve, and restore with a complete audit trail.

### Phase 5 — Optional module adapters

- Adapt clinical, competitive, compliance, institutional-memory, task automation, and advanced views to the registry.
- Add opportunity/funder-fit analysis.

Exit: each module can be independently enabled, disabled, and migrated without affecting unrelated gates.

### Phase 6 — Review simulator and causal analysis

- Implement rubric extraction/versioning and panel-plan approval.
- Add individual review, deterministic validation, consensus, causal graph, findings, and revision-task generation.
- Add sponsor/mechanism templates starting with NIH plus one foundation/RFI template.

Exit: every critique is grounded, every score uses the correct rubric, causal claims are traceable, and simulation limitations are visible.

### Phase 7 — Hardening and release

- Security/threat modeling, authorization tests, audit export, backup/restore, load tests, accessibility, model-evaluation suite, and release migration drills.

Exit: release acceptance passes for both local-single-user and authenticated-shared profiles.

## 13. Testing and evaluation

### Deterministic tests

- Migration from current schema and legacy project reconstruction.
- All supported workflow presets plus pairwise/all combinations of optional modules.
- Property: disabled modules never appear in blockers/readiness.
- Property: core modules cannot be disabled.
- Gate invalidation when RFA, framework, or aims change.
- Optimistic edit conflict with two clients.
- Rollback preserves all versions and creates a new head.
- Role authorization for view/edit/review/approve/export.
- Message/comment tenant and project isolation.
- Export snapshot reproducibility.

### Model evaluations

- Requirement/rubric recall against labeled solicitations.
- No fabricated solicitation quotes or reviewer instructions.
- Framework coverage of requirements and rubric.
- Search-query relevance to both aims and RFA.
- Citation/excerpt exactness.
- Reviewer role differentiation without unsupported biography/preferences.
- Critique-to-proposal anchor validity.
- Score/critique consistency and calibration disclaimers.
- Causal DAG recovery on synthetic benchmark studies.
- Detection of confounding, selection, temporal-order, measurement, and transportability flaws.
- Stability across repeated runs; preserve genuine disagreement rather than forcing consensus.

### Human acceptance

- PI can create a lean workflow without seeing specialist screens.
- Research administrator can create a full submission workflow.
- RFI user can use narrative/mission review without forced NIH scoring.
- Methodologist can correct the causal graph and mark findings resolved.
- Internal reviewers can distinguish simulated feedback from human comments at a glance.

## 14. Security, privacy, and governance

- Authenticate before any shared project content is returned.
- Enforce authorization in Rust API handlers, not only in Gradio visibility.
- Encrypt traffic and shared storage; keep secrets out of project data and logs.
- Record model/provider used for every generated artifact.
- Show exactly what context is sent to a cloud provider before a high-value run.
- Do not train on project content by default.
- Add retention/export/deletion policies and protected audit retention.
- Treat reviewer simulation as decision support and prohibit automated submission or funding claims.
- Do not infer protected traits or simulate reviewer bias based on them.
- Keep real human communications out of review-simulator prompts unless explicitly selected and authorized.

### 14.1 Local and hybrid model profiles

Model routing is an explicit deployment choice and must be shown in the wizard's
privacy/routing summary:

| Hardware profile | Local runtime | Default routing | Intended responsibility |
|---|---|---|---|
| M4, 24 GB, OLMo 3 | Native MLX, 7B 4-bit | Hybrid with Claude required | Local bounded drafting; Claude for sponsor interpretation, evidence validation, compliance, causal/review work, and high-value synthesis |
| M4, 24 GB, Qwen 3 | Native MLX, 8B 4-bit | Hybrid with Claude required | Same routing boundary, with Qwen as the replaceable local drafting engine |
| M2, 8 GB | Native Ollama, Qwen 3 1.7B Q4 plus CPU embeddings | Local-only by explicit choice; hybrid recommended for high-stakes use | Privacy-first, bounded drafting and development; do not present the small model as equivalent to the M4 or Claude review path |

The provider-neutral `LOCAL_LLM_*` contract supersedes model-family-specific
names while retaining `OLMO_*` aliases during migration. A local-only setting
must prevent all proposal content from being sent to Claude. A hybrid setting
must show which task kinds are cloud-routed before execution and record the
provider/model on every generated artifact.

## 15. Initial release scope and exclusions

Recommended first release:

- five core workflow steps;
- project-persisted optional modules;
- full-workbench legacy mode;
- team membership, project chat, tasks, version compare/restore, and named approvals;
- NIH-oriented review simulator with generic foundation/RFI mode;
- editable causal logic model and causal-validity checks;
- existing DOCX/PDF export and deterministic compliance integration when selected.

Defer:

- grant marketplace/database competitive parity with Instrumentl;
- direct submission to sponsor portals;
- post-award accounting and payment management;
- automatic email/calendar integrations until notification permissions are designed;
- award-probability prediction;
- multi-region or active-active deployment;
- persona simulation of named real reviewers.

## 16. Definition of done

The initiative is complete when:

1. The wizard determines the persisted workflow and visible UI for the grant.
2. The five core steps work with all add-ons disabled.
3. No hidden/unselected module can block drafting or export.
4. Existing projects migrate without losing approvals, versions, citations, or exports.
5. Multiple authenticated team members share one project with safe conflict handling and complete edit/approval history.
6. Rollback always creates an auditable new version.
7. The simulator uses solicitation-specific rubrics and clearly labels synthetic feedback.
8. Causal findings are structured, evidence-linked, editable, and never presented as certain when inferred.
9. Final exports are reproducible from exact approved artifact versions.
10. Security, migration, module-combination, concurrency, model-evaluation, and release-acceptance suites pass.
