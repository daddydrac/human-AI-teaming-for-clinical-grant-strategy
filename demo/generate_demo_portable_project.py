#!/usr/bin/env python3
"""Generate a deterministic Grantspace portable-project archive for GUI demos."""

from __future__ import annotations

import hashlib
import json
import zipfile
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
REGISTRY_PATH = ROOT / "config" / "workflow_definitions.json"
OUTPUT_PATH = ROOT / "demo" / "RUNTIME-VALIDATED-grantspace-demo-project.zip"
ARCHIVE_MEMBER = "grantspace-project.json"
CREATED_AT = "2026-08-24T15:00:00Z"


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key in workflow registry: {key}")
        result[key] = value
    return result


def _compact_json(value: Any, *, sort_keys: bool = False) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=sort_keys,
    ).encode("utf-8")


def _sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def _step_definition(source: dict[str, Any]) -> dict[str, Any]:
    """Reproduce the Rust WorkflowStepDefinition serialization order."""
    return {
        "key": source["key"],
        "title": source["title"],
        "description": source["description"],
        "placement": source["placement"],
        "output": source["output"],
        "artifact_type": source.get("artifact_type"),
        "ui_surface": source["ui_surface"],
        "completion_evaluator": source["completion_evaluator"],
        "prerequisites": source.get("prerequisites", []),
    }


def _serialized_registry(
    source: dict[str, Any], *, include_gate_configurable: bool
) -> dict[str, Any]:
    """Match serde's typed WorkflowRegistry representation byte-for-byte."""
    modules: list[dict[str, Any]] = []
    for item in source["optional_modules"]:
        module = _step_definition(item)
        module["gate_default"] = item["gate_default"]
        if include_gate_configurable:
            module["gate_configurable"] = item.get("gate_configurable", False)
        module["runtime_implication"] = item["runtime_implication"]
        modules.append(module)

    return {
        "schema_version": source["schema_version"],
        "definition_version": source["definition_version"],
        "default_preset_key": source["default_preset_key"],
        "legacy_preset_key": source["legacy_preset_key"],
        "review_module_key": source["review_module_key"],
        "model_routing_modes": source.get("model_routing_modes", []),
        "gate_tokens": source.get("gate_tokens", []),
        "core_steps": [_step_definition(item) for item in source["core_steps"]],
        "optional_modules": modules,
        "presets": [
            {
                "key": item["key"],
                "title": item["title"],
                "enabled_modules": item.get("enabled_modules", []),
                "required_modules": item.get("required_modules", []),
            }
            for item in source["presets"]
        ],
        "review_modes": [
            {"key": item["key"], "title": item["title"]}
            for item in source["review_modes"]
        ],
        "reviewer_archetypes": [
            {
                "key": item["key"],
                "title": item["title"],
                "description": item["description"],
                "criterion_terms": item.get("criterion_terms", []),
                "grant_types": item.get("grant_types", []),
                "always_include": item.get("always_include", False),
            }
            for item in source.get("reviewer_archetypes", [])
        ],
    }


def _demo_solicitation() -> str:
    return """DEMO HEALTH FOUNDATION
Community Cancer Navigation Innovation Grant
Funding Opportunity: DHF-CCN-2026

PURPOSE
The Demo Health Foundation invites proposals to improve timely access to evidence-based cancer screening and diagnostic follow-up for adults living in rural communities. Projects must pair a community-informed navigation intervention with a practical implementation and evaluation plan.

ELIGIBILITY
Eligible lead applicants are nonprofit healthcare organizations, accredited universities, tribal health organizations, and public health agencies based in the United States. The principal investigator must be employed by an eligible lead applicant. Partnerships with community organizations are strongly encouraged.

AWARD AND PERIOD
Applicants may request up to $450,000 in direct costs for a project period of no more than 24 months. Indirect costs may not exceed 10 percent of modified total direct costs.

REQUIRED PROJECT ELEMENTS
1. Define the target rural population and document the access disparity using current evidence.
2. Describe a community-informed patient-navigation intervention and its implementation setting.
3. State measurable primary and secondary outcomes, including a diagnostic follow-up outcome.
4. Provide a feasible recruitment, retention, data-quality, and statistical-analysis plan.
5. Explain how community partners will participate in governance and interpretation.
6. Include milestones, a risk register, and a sustainability plan.

REQUIRED APPLICATION SECTIONS
- Specific Aims: 1 page
- Significance and Community Need: 2 pages
- Innovation: 1 page
- Approach and Evaluation: 6 pages
- Team, Environment, and Community Partnership: 2 pages
- Milestones and Sustainability: 2 pages
- Budget and Budget Justification
- Biographical Sketches
- Letters of Commitment

REVIEW CRITERIA
Applications will be reviewed for: importance of the documented need; responsiveness to the opportunity; rigor and feasibility of the intervention and evaluation; meaningful community partnership; qualifications and resources of the team; and likelihood that successful activities can be sustained. Reviewers will identify strengths, weaknesses, and questions for each criterion. This opportunity uses narrative assessment rather than a numeric score.

KEY DATES
Letter of intent due: October 2, 2026 at 5:00 PM Central Time
Application due: November 6, 2026 at 5:00 PM Central Time
Anticipated project start: April 1, 2027

SUBMISSION
Submit one searchable PDF containing the narrative and required attachments through the foundation portal. Late or incomplete applications will not be reviewed.

This fictional solicitation is included only to exercise the Grantspace import and grant-development workflow. It is not a real funding opportunity.
"""


def _demo_editor_sections() -> list[dict[str, Any]]:
    definitions = [
        ("specific_aims", "Specific Aims", "State the objective, central thesis, focused aims, expected outcomes, and impact."),
        ("significance_and_community_need", "Significance and Community Need", "Define the rural cancer-care disparity, affected population, evidence base, and sponsor-aligned importance."),
        ("innovation", "Innovation", "Explain what is meaningfully new about the navigation and implementation strategy."),
        ("approach_and_evaluation", "Approach and Evaluation", "Describe implementation, recruitment, outcomes, analysis, rigor, feasibility, risks, and alternatives."),
        ("team_environment_and_community_partnership", "Team, Environment, and Community Partnership", "Describe roles, resources, shared governance, and the setting needed to deliver the work."),
        ("milestones_and_sustainability", "Milestones and Sustainability", "Define measurable milestones, decision points, risks, mitigation, and a credible continuation strategy."),
    ]
    return [
        {
            "section_key": key,
            "title": title,
            "description": description,
            "position": position,
            "required": True,
            "origin": "document_editor",
            "created_at": CREATED_AT,
        }
        for position, (key, title, description) in enumerate(definitions)
    ]


def _demo_section_versions() -> list[dict[str, Any]]:
    bodies = {
        "specific_aims": """Rural adults often experience delays between an abnormal cancer-screening result and diagnostic resolution. This proposal will develop and evaluate a community-informed navigation program designed to reduce avoidable barriers to timely follow-up while creating an implementation approach that can be sustained in rural care settings.\n\nAim 1 will work with community and clinical partners to adapt the navigation workflow to the target population, referral pathways, and local service constraints. Aim 2 will implement the adapted workflow and evaluate reach, delivery, retention, data quality, and timely diagnostic follow-up. Aim 3 will identify the operational requirements, costs, and partnership commitments needed for continuation after the grant period.\n\nThe expected outcome is an evidence-informed, feasible navigation model with transparent implementation and evaluation results. [TEAM INPUT NEEDED: Define the target geography, eligible screening population, primary outcome time window, and intended sample size.]""",
        "significance_and_community_need": """Timely diagnostic follow-up is essential to realizing the benefit of cancer screening, yet rural patients may face travel, scheduling, referral, financial, communication, and care-coordination barriers. The proposed work focuses on the interval after an abnormal screening result, when fragmented handoffs can delay resolution and increase burden for patients and families.\n\nThe project will document the local disparity with current, source-verifiable data and will distinguish confirmed conditions from estimates that still require investigator input. Its significance rests on connecting a measurable access problem to a practical navigation intervention, a defined implementation setting, and outcomes that matter to patients, community partners, clinical teams, and the sponsor. [TEAM INPUT NEEDED: Add local baseline data and exact citations for the target population.]""",
        "innovation": """The proposed work is innovative in its integration of community-informed adaptation, patient navigation, implementation measurement, and diagnostic follow-up evaluation within one operational model. Rather than treating navigation as a stand-alone service, the project will specify how referrals, barrier assessment, communication, escalation, and closed-loop follow-up function across organizations.\n\nInnovation claims will be limited to differences supported by the literature and the documented local workflow. [TEAM INPUT NEEDED: Identify existing navigation services and the exact features that distinguish the proposed model.]""",
        "approach_and_evaluation": """The project will use a staged implementation and evaluation design. The team will first map the current follow-up pathway and adapt the navigation workflow with community and clinical partners. It will then launch the intervention using documented eligibility, referral, barrier-assessment, contact, escalation, and closure procedures.\n\nEvaluation will include implementation reach and fidelity, participant retention, data completeness, and a prespecified diagnostic follow-up outcome. Analyses will align the estimand, outcome definition, time origin, comparison strategy, missing-data approach, and sensitivity analyses. Milestone reviews will trigger documented adaptations when recruitment, delivery, or data quality falls outside agreed thresholds. [TEAM INPUT NEEDED: Specify the design, sample, endpoints, analysis model, and feasibility assumptions.]""",
        "team_environment_and_community_partnership": """The project will use shared governance that gives community and clinical partners defined roles in adaptation, implementation review, interpretation, and dissemination. Named owners will be assigned for navigation operations, community engagement, data management, statistical analysis, and project administration.\n\nThe final narrative will describe only facilities, personnel, partnerships, and institutional commitments that the team verifies. [TEAM INPUT NEEDED: Add confirmed organizations, personnel, facilities, letters of commitment, and decision rights.]""",
        "milestones_and_sustainability": """Milestones will cover workflow adaptation, launch readiness, recruitment, intervention delivery, follow-up completion, data quality, analysis, and dissemination. Each milestone will have an owner, target date, measurable threshold, evidence source, and response when performance is off track.\n\nSustainability planning will assess the staffing, workflow ownership, training, technology, referral relationships, and financing needed to continue successful activities. The plan will not assume continuation commitments that have not been documented. [TEAM INPUT NEEDED: Add internal milestone dates, escalation thresholds, operating costs, and verified continuation commitments.]""",
    }
    versions = []
    for version_id, section in enumerate(_demo_editor_sections(), 1):
        versions.append(
            {
                "id": version_id,
                "section_key": section["section_key"],
                "title": section["title"],
                "body": bodies[section["section_key"]],
                "html": None,
                "source": "human_imported_demo_draft",
                "editor_name": None,
                "author_user_id": None,
                "approved": False,
                "base_version_id": None,
                "restored_from_version_id": None,
                "generation_run_id": None,
                "created_at": CREATED_AT,
            }
        )
    return versions


def build_package(*, include_gate_configurable: bool) -> dict[str, Any]:
    registry_source = json.loads(
        REGISTRY_PATH.read_text(encoding="utf-8"),
        object_pairs_hook=_reject_duplicate_keys,
    )
    registry = _serialized_registry(
        registry_source,
        include_gate_configurable=include_gate_configurable,
    )
    definition_sha256 = _sha256(_compact_json(registry))

    config = {
        "schema_version": registry["schema_version"],
        "definition_version": registry["definition_version"],
        "template": registry["default_preset_key"],
        "enabled_modules": [],
        "required_modules": [],
        "review_mode": None,
        "review_required": False,
        "grant_type": "custom",
        "target_deadline": "2026-11-06T17:00:00-06:00",
        "model_routing_mode": "local_only",
        "local_model_provider": "ollama",
        "local_model": "qwen3:1.7b",
        "cloud_model": None,
        "cloud_task_kinds": [],
    }
    config_sha256 = _sha256(_compact_json(config))
    solicitation = _demo_solicitation()

    payload = {
        "project": {
            "id": "demo-community-cancer-navigation-2026",
            "title": "Community Cancer Navigation Innovation Grant",
            "sponsor": "Demo Health Foundation",
            "mechanism": "DHF-CCN-2026",
            "stage": "intake",
            "interview_generated": False,
            "created_at": CREATED_AT,
            "updated_at": CREATED_AT,
        },
        "workflow": {
            "definition_version": registry["definition_version"],
            "definition_sha256": definition_sha256,
            "config_version": 1,
            "config_sha256": config_sha256,
            "config": config,
            "created_at": CREATED_AT,
            "updated_at": CREATED_AT,
        },
        "documents": [
            {
                "id": 1,
                "name": "DHF-CCN-2026-demo-solicitation.txt",
                "kind": "funding_opportunity",
                "text": solicitation,
                "sha256": _sha256(solicitation.encode("utf-8")),
                "created_at": CREATED_AT,
            }
        ],
        "document_chunks": [],
        "requirements": [],
        "interview_questions": [],
        "interview_answers": [],
        "research_queries": [],
        "research_sources": [],
        "sections": _demo_editor_sections(),
        "generation_runs": [],
        "section_versions": _demo_section_versions(),
        "approvals": [],
        "workflow_artifacts": [],
        "evidence": [],
        "citations": [],
        "design": None,
        "clinical_study": None,
        "competitive_intelligence": None,
        "compliance_profile": None,
        "compliance_sources": [],
        "compliance_resolutions": [],
        "export_snapshots": [],
    }
    payload_sha256 = _sha256(_compact_json(payload, sort_keys=True))
    return {
        "format": "grantspace-portable-project",
        "schema_version": 2,
        "workflow_definition_version": registry["definition_version"],
        "workflow_definition_sha256": definition_sha256,
        "source_project_id": "demo-community-cancer-navigation-2026",
        "payload_sha256": payload_sha256,
        "payload": payload,
    }


def validate_package(package: dict[str, Any]) -> None:
    if package["format"] != "grantspace-portable-project":
        raise ValueError("unexpected portable-project format")
    if package["schema_version"] != 2:
        raise ValueError("unexpected portable-project schema version")
    payload = package["payload"]
    if _sha256(_compact_json(payload, sort_keys=True)) != package["payload_sha256"]:
        raise ValueError("payload checksum does not match")
    if not payload["project"]["title"].strip():
        raise ValueError("project title is empty")
    document_ids: set[int] = set()
    for document in payload["documents"]:
        if document["id"] in document_ids:
            raise ValueError("duplicate document ID")
        document_ids.add(document["id"])
        if _sha256(document["text"].encode("utf-8")) != document["sha256"]:
            raise ValueError(f"document checksum does not match: {document['name']}")


def _write_archive(package: dict[str, Any], output_path: Path) -> None:
    validate_package(package)
    raw = _compact_json(package, sort_keys=True)

    output_path.parent.mkdir(parents=True, exist_ok=True)
    archive_entry = zipfile.ZipInfo(ARCHIVE_MEMBER, date_time=(2026, 8, 24, 15, 0, 0))
    archive_entry.compress_type = zipfile.ZIP_DEFLATED
    archive_entry.create_system = 3
    archive_entry.external_attr = 0o100644 << 16
    with zipfile.ZipFile(output_path, "w") as archive:
        archive.writestr(archive_entry, raw)

    with zipfile.ZipFile(output_path, "r") as archive:
        names = [item.filename for item in archive.infolist() if not item.is_dir()]
        if names != [ARCHIVE_MEMBER]:
            raise ValueError(f"archive has unexpected members: {names}")
        archived = json.loads(archive.read(ARCHIVE_MEMBER))
        validate_package(archived)


def main() -> None:
    package = build_package(include_gate_configurable=True)
    _write_archive(package, OUTPUT_PATH)

    print(OUTPUT_PATH)
    print(
        "workflow_definition_sha256="
        f"{package['workflow_definition_sha256']}"
    )
    print(f"payload_sha256={package['payload_sha256']}")


if __name__ == "__main__":
    main()
