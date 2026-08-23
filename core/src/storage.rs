use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::clinical::ClinicalStudy;
use crate::competitive::{CompetitiveConfig, CompetitiveProfile, CompetitiveRunOutput};
use crate::competitive_updates::CompetitiveDelta;
use crate::compliance::{evaluate as evaluate_compliance, ComplianceFacts, ComplianceProfile};
use crate::domain::{InterviewQuestionDraft, RequirementDraft, RetrievalRecord};
use crate::models::{GenerationAudit, StructuredOutputContract};
use crate::research::FetchedSource;
use crate::source_locator::SourceDocument;
use crate::workflow::{WorkflowConfig, WorkflowRegistry, WorkflowStepDefinition};
use uuid::Uuid;

pub struct Store {
    path: PathBuf,
    workflow_registry: WorkflowRegistry,
}

pub enum IdempotencyClaim {
    New,
    InProgress,
    Replay {
        status_code: u16,
        content_type: String,
        body: Vec<u8>,
    },
    Conflict,
}

#[derive(Clone, Debug)]
pub struct InternalAccountRecord {
    pub id: String,
    pub organization_id: String,
    pub username: String,
    pub email: String,
    pub display_name: String,
    pub password_hash: String,
    pub system_role: String,
    pub must_change_password: bool,
    pub active: bool,
    pub locked: bool,
}

#[derive(Clone, Debug)]
pub struct InternalSessionRecord {
    pub account: InternalAccountRecord,
    pub expires_at: String,
}

#[derive(Clone, Debug)]
pub struct StagedResearchSource {
    pub source: FetchedSource,
    pub validation_status: String,
    pub confidence: f64,
    pub supporting_excerpt: String,
    pub explanation: String,
}

#[derive(Clone, Debug)]
pub struct StagedResearchQuery {
    pub query: crate::workflow_artifacts::LiteratureQueryRecord,
    pub terminal_status: String,
    pub sources: Vec<StagedResearchSource>,
}

#[derive(Clone, Debug)]
pub struct StagedResearchRun {
    pub id: String,
    pub search_plan_version: i64,
    pub solicitation_profile_version: i64,
    pub framework_version: i64,
    pub aim_set_version: i64,
    pub search_provider: String,
    pub started_at: String,
    pub completed_at: String,
    pub queries: Vec<StagedResearchQuery>,
    pub failures: Vec<String>,
}

impl Store {
    fn configure(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(15))?;
        Ok(())
    }

    fn conn(&self) -> Result<Connection> {
        let conn = Connection::open(&self.path)?;
        Self::configure(&conn)?;
        Ok(conn)
    }

    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut st = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = st.query_map([], |r| r.get::<_, String>(1))?;
        for row in rows {
            if row? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
        Ok(conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",[table],|row|row.get::<_,i64>(0))?>0)
    }

    fn migrate(conn: &Connection) -> Result<()> {
        let current: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version),0) FROM schema_migrations",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if !Self::has_column(conn, "projects", "stage")? {
            conn.execute(
                "ALTER TABLE projects ADD COLUMN stage TEXT NOT NULL DEFAULT 'intake'",
                [],
            )?;
        }
        if !Self::has_column(conn, "projects", "updated_at")? {
            conn.execute("ALTER TABLE projects ADD COLUMN updated_at TEXT", [])?;
            conn.execute(
                "UPDATE projects SET updated_at=created_at WHERE updated_at IS NULL",
                [],
            )?;
        }
        if !Self::has_column(conn, "projects", "interview_generated")? {
            conn.execute(
                "ALTER TABLE projects ADD COLUMN interview_generated INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !Self::has_column(conn, "project_sections", "origin")? {
            conn.execute(
                "ALTER TABLE project_sections ADD COLUMN origin TEXT NOT NULL DEFAULT 'configured'",
                [],
            )?;
        }
        if !Self::has_column(conn, "section_versions", "editor_name")? {
            conn.execute(
                "ALTER TABLE section_versions ADD COLUMN editor_name TEXT",
                [],
            )?;
        }
        if !Self::has_column(conn, "approvals", "approved_by")? {
            conn.execute("ALTER TABLE approvals ADD COLUMN approved_by TEXT", [])?;
        }
        if !Self::has_column(conn, "section_versions", "base_version_id")? {
            conn.execute(
                "ALTER TABLE section_versions ADD COLUMN base_version_id INTEGER",
                [],
            )?;
        }
        if !Self::has_column(conn, "section_versions", "restored_from_version_id")? {
            conn.execute(
                "ALTER TABLE section_versions ADD COLUMN restored_from_version_id INTEGER",
                [],
            )?;
        }
        if !Self::has_column(conn, "section_versions", "generation_run_id")? {
            conn.execute(
                "ALTER TABLE section_versions ADD COLUMN generation_run_id TEXT",
                [],
            )?;
        }
        if !Self::has_column(conn, "project_workflows", "definition_sha256")? {
            conn.execute(
                "ALTER TABLE project_workflows ADD COLUMN definition_sha256 TEXT",
                [],
            )?;
        }
        if !Self::has_column(conn, "project_workflows", "config_version")? {
            conn.execute("ALTER TABLE project_workflows ADD COLUMN config_version INTEGER NOT NULL DEFAULT 1",[])?;
        }
        if !Self::has_column(conn, "project_workflows", "config_sha256")? {
            conn.execute(
                "ALTER TABLE project_workflows ADD COLUMN config_sha256 TEXT",
                [],
            )?;
        }
        if current < 14 {
            if Self::table_exists(conn,"project_members")?&&!Self::table_exists(conn,"legacy_project_members")?{
                conn.execute("ALTER TABLE project_members RENAME TO legacy_project_members",[])?;
            }
            if Self::table_exists(conn,"project_messages")?&&!Self::table_exists(conn,"legacy_project_messages")?{
                conn.execute("ALTER TABLE project_messages RENAME TO legacy_project_messages",[])?;
            }
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS organizations(
              id TEXT PRIMARY KEY,name TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS users(
              id TEXT PRIMARY KEY,organization_id TEXT NOT NULL,email TEXT,display_name TEXT NOT NULL,
              active INTEGER NOT NULL DEFAULT 1,created_at TEXT DEFAULT CURRENT_TIMESTAMP,last_seen_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(organization_id) REFERENCES organizations(id)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_users_org_email ON users(organization_id,email) WHERE email IS NOT NULL;
            CREATE TABLE IF NOT EXISTS project_members(
              project_id TEXT NOT NULL,user_id TEXT NOT NULL,role TEXT NOT NULL,
              invited_by_user_id TEXT,joined_at TEXT DEFAULT CURRENT_TIMESTAMP,last_seen_at TEXT DEFAULT CURRENT_TIMESTAMP,
              PRIMARY KEY(project_id,user_id),FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
              FOREIGN KEY(user_id) REFERENCES users(id),FOREIGN KEY(invited_by_user_id) REFERENCES users(id)
            );
            CREATE TABLE IF NOT EXISTS project_invites(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,email TEXT NOT NULL,role TEXT NOT NULL,token_sha256 TEXT NOT NULL UNIQUE,
              invited_by_user_id TEXT NOT NULL,expires_at TEXT NOT NULL,accepted_by_user_id TEXT,accepted_at TEXT,revoked_at TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(invited_by_user_id) REFERENCES users(id),FOREIGN KEY(accepted_by_user_id) REFERENCES users(id)
            );
            CREATE TABLE IF NOT EXISTS channels(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,kind TEXT NOT NULL,subject_key TEXT,name TEXT NOT NULL,created_by_user_id TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              UNIQUE(project_id,kind,subject_key),FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(created_by_user_id) REFERENCES users(id)
            );
            CREATE TABLE IF NOT EXISTS messages(
              id INTEGER PRIMARY KEY AUTOINCREMENT,channel_id TEXT NOT NULL,author_user_id TEXT NOT NULL,body TEXT NOT NULL,
              parent_message_id INTEGER,edited_at TEXT,deleted_at TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(channel_id) REFERENCES channels(id) ON DELETE CASCADE,FOREIGN KEY(author_user_id) REFERENCES users(id),FOREIGN KEY(parent_message_id) REFERENCES messages(id)
            );
            CREATE INDEX IF NOT EXISTS idx_messages_channel ON messages(channel_id,id);
            CREATE TABLE IF NOT EXISTS comments(
              id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,artifact_type TEXT NOT NULL,artifact_key TEXT NOT NULL,
              version_id INTEGER NOT NULL,start_offset INTEGER,end_offset INTEGER,quoted_text TEXT,author_user_id TEXT NOT NULL,body TEXT NOT NULL,
              parent_comment_id INTEGER,resolved_by_user_id TEXT,resolved_at TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(author_user_id) REFERENCES users(id),FOREIGN KEY(resolved_by_user_id) REFERENCES users(id),FOREIGN KEY(parent_comment_id) REFERENCES comments(id)
            );
            CREATE INDEX IF NOT EXISTS idx_comments_artifact ON comments(project_id,artifact_type,artifact_key,version_id,id);
            CREATE TABLE IF NOT EXISTS mentions(
              id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,user_id TEXT NOT NULL,message_id INTEGER,comment_id INTEGER,read_at TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              CHECK((message_id IS NOT NULL) != (comment_id IS NOT NULL)),FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(user_id) REFERENCES users(id),FOREIGN KEY(message_id) REFERENCES messages(id),FOREIGN KEY(comment_id) REFERENCES comments(id)
            );
            CREATE TABLE IF NOT EXISTS tasks(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,title TEXT NOT NULL,description TEXT NOT NULL DEFAULT '',owner_user_id TEXT NOT NULL,
              source TEXT NOT NULL,status TEXT NOT NULL DEFAULT 'open',priority TEXT NOT NULL DEFAULT 'normal',due_at TEXT,completed_at TEXT,created_by_user_id TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP,updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(owner_user_id) REFERENCES users(id),FOREIGN KEY(created_by_user_id) REFERENCES users(id)
            );
            CREATE TABLE IF NOT EXISTS task_dependencies(
              task_id TEXT NOT NULL,depends_on_task_id TEXT NOT NULL,PRIMARY KEY(task_id,depends_on_task_id),CHECK(task_id<>depends_on_task_id),
              FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE,FOREIGN KEY(depends_on_task_id) REFERENCES tasks(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS notifications(
              id INTEGER PRIMARY KEY AUTOINCREMENT,user_id TEXT NOT NULL,project_id TEXT NOT NULL,kind TEXT NOT NULL,payload_json TEXT NOT NULL,read_at TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(user_id) REFERENCES users(id),FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
            );
            "#)?;
            if !Self::has_column(conn,"section_versions","author_user_id")?{conn.execute("ALTER TABLE section_versions ADD COLUMN author_user_id TEXT",[])?;}
            if !Self::has_column(conn,"approvals","approver_user_id")?{conn.execute("ALTER TABLE approvals ADD COLUMN approver_user_id TEXT",[])?;}
            if !Self::has_column(conn,"approvals","role_at_approval")?{conn.execute("ALTER TABLE approvals ADD COLUMN role_at_approval TEXT",[])?;}
            if !Self::has_column(conn,"approvals","decision")?{conn.execute("ALTER TABLE approvals ADD COLUMN decision TEXT NOT NULL DEFAULT 'approved'",[])?;}
            if !Self::has_column(conn,"approvals","notes")?{conn.execute("ALTER TABLE approvals ADD COLUMN notes TEXT",[])?;}
        }
        if current < 15 {
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS generation_runs(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,task_kind TEXT NOT NULL,
              routing_mode TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,
              prompt_sha256 TEXT NOT NULL,response_sha256 TEXT,high_value INTEGER NOT NULL DEFAULT 0,
              status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),
              error TEXT,started_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_generation_runs_project ON generation_runs(project_id,id);
            "#)?;
        }
        if current < 16 {
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS idempotency_keys(
              user_id TEXT NOT NULL,key TEXT NOT NULL,method TEXT NOT NULL,path TEXT NOT NULL,request_sha256 TEXT NOT NULL,
              state TEXT NOT NULL CHECK(state IN ('in_progress','complete')),
              status_code INTEGER,content_type TEXT,response_body BLOB,
              created_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
              PRIMARY KEY(user_id,key),FOREIGN KEY(user_id) REFERENCES users(id)
            );
            CREATE INDEX IF NOT EXISTS idx_idempotency_created ON idempotency_keys(created_at);
            "#)?;
        }
        if current < 17 && Self::table_exists(conn, "legacy_project_members")? {
            conn.execute("INSERT OR IGNORE INTO organizations(id,name) VALUES('legacy-unclaimed','Legacy collaboration identities awaiting administrator reconciliation')",[])?;
            let mut members=Vec::new();
            {
                let mut statement=conn.prepare("SELECT project_id,member_name,role FROM legacy_project_members")?;
                let rows=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?;
                for row in rows{members.push(row?);}
            }
            for (project,name,legacy_role) in members {
                let user_id=format!("legacy-{}",sha256_hex(format!("{project}\0{name}").as_bytes()));
                let role=match legacy_role.trim().to_ascii_lowercase().replace(' ',"_").as_str(){
                    "owner"=>"owner","pi"|"principal_investigator"=>"pi","reviewer"=>"reviewer","approver"=>"approver","research_administrator"|"administrator"=>"research_administrator","viewer"=>"viewer",_=>"contributor"
                };
                conn.execute("INSERT OR IGNORE INTO users(id,organization_id,display_name,active) VALUES(?1,'legacy-unclaimed',?2,0)",params![user_id,name])?;
                conn.execute("INSERT OR IGNORE INTO project_members(project_id,user_id,role) VALUES(?1,?2,?3)",params![project,user_id,role])?;
            }
            if Self::table_exists(conn,"legacy_project_messages")?{
                let mut messages=Vec::new();
                {
                    let mut statement=conn.prepare("SELECT project_id,author,body,created_at FROM legacy_project_messages ORDER BY id")?;
                    let rows=statement.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?)))?;
                    for row in rows{messages.push(row?);}
                }
                for (project,author,body,created_at) in messages{
                    let user_id=format!("legacy-{}",sha256_hex(format!("{project}\0{author}").as_bytes()));
                    conn.execute("INSERT OR IGNORE INTO users(id,organization_id,display_name,active) VALUES(?1,'legacy-unclaimed',?2,0)",params![user_id,author])?;
                    conn.execute("INSERT OR IGNORE INTO project_members(project_id,user_id,role) VALUES(?1,?2,'contributor')",params![project,user_id])?;
                    let channel_id=format!("legacy-general-{}",sha256_hex(project.as_bytes()));
                    conn.execute("INSERT OR IGNORE INTO channels(id,project_id,kind,subject_key,name,created_by_user_id) VALUES(?1,?2,'general',NULL,'General',?3)",params![channel_id,project,user_id])?;
                    conn.execute("INSERT INTO messages(channel_id,author_user_id,body,created_at) SELECT ?1,?2,?3,?4 WHERE NOT EXISTS(SELECT 1 FROM messages WHERE channel_id=?1 AND author_user_id=?2 AND body=?3 AND created_at=?4)",params![channel_id,user_id,body,created_at])?;
                }
            }
        }
        if current < 18 {
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS review_panel_plans(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,solicitation_profile_version INTEGER NOT NULL,
              registry_definition_version INTEGER NOT NULL,mode TEXT NOT NULL,roles_json TEXT NOT NULL,
              status TEXT NOT NULL CHECK(status IN ('draft','approved')),created_by_user_id TEXT NOT NULL,
              approved_by_user_id TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,approved_at TEXT,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS proposal_review_snapshots(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,snapshot_json TEXT NOT NULL,content_sha256 TEXT NOT NULL,
              created_by_user_id TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS review_simulation_runs(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,snapshot_id TEXT NOT NULL,panel_plan_id TEXT NOT NULL,
              rubric_version_id TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),
              result_json TEXT,result_sha256 TEXT,error TEXT,created_by_user_id TEXT NOT NULL,
              created_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
              FOREIGN KEY(snapshot_id) REFERENCES proposal_review_snapshots(id),FOREIGN KEY(panel_plan_id) REFERENCES review_panel_plans(id)
            );
            CREATE TABLE IF NOT EXISTS causal_models(
              id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,review_run_id TEXT NOT NULL,version INTEGER NOT NULL,
              body_json TEXT NOT NULL,content_sha256 TEXT NOT NULL,author_user_id TEXT NOT NULL,confirmed INTEGER NOT NULL DEFAULT 0,
              created_at TEXT DEFAULT CURRENT_TIMESTAMP,UNIQUE(review_run_id,version),
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(review_run_id) REFERENCES review_simulation_runs(id)
            );
            CREATE INDEX IF NOT EXISTS idx_review_runs_project ON review_simulation_runs(project_id,created_at DESC);
            "#)?;
        }
        if current < 19 && !Self::has_column(conn,"review_panel_plans","synthetic_review_notice")? {
            conn.execute("ALTER TABLE review_panel_plans ADD COLUMN synthetic_review_notice TEXT NOT NULL DEFAULT ''",[])?;
            conn.execute("UPDATE review_panel_plans SET synthetic_review_notice=?1 WHERE synthetic_review_notice=''",[crate::workflow_artifacts::SYNTHETIC_REVIEW_NOTICE])?;
        }
        if current < 20 {
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS artifact_approval_decisions(
              project_id TEXT NOT NULL,artifact_type TEXT NOT NULL,artifact_version INTEGER NOT NULL,
              approver_user_id TEXT NOT NULL,role_at_approval TEXT NOT NULL,decision TEXT NOT NULL CHECK(decision IN ('approved','rejected')),
              notes TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              PRIMARY KEY(project_id,artifact_type,artifact_version,approver_user_id),
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(approver_user_id) REFERENCES users(id)
            );
            CREATE INDEX IF NOT EXISTS idx_artifact_approval_decisions ON artifact_approval_decisions(project_id,artifact_type,artifact_version);
            "#)?;
        }
        if current < 21 {
            if !Self::has_column(conn,"users","username")?{conn.execute("ALTER TABLE users ADD COLUMN username TEXT",[])?;}
            if !Self::has_column(conn,"users","password_hash")?{conn.execute("ALTER TABLE users ADD COLUMN password_hash TEXT",[])?;}
            if !Self::has_column(conn,"users","system_role")?{conn.execute("ALTER TABLE users ADD COLUMN system_role TEXT NOT NULL DEFAULT 'user'",[])?;}
            if !Self::has_column(conn,"users","must_change_password")?{conn.execute("ALTER TABLE users ADD COLUMN must_change_password INTEGER NOT NULL DEFAULT 0",[])?;}
            if !Self::has_column(conn,"users","password_changed_at")?{conn.execute("ALTER TABLE users ADD COLUMN password_changed_at TEXT",[])?;}
            if !Self::has_column(conn,"users","disabled_at")?{conn.execute("ALTER TABLE users ADD COLUMN disabled_at TEXT",[])?;}
            if !Self::has_column(conn,"users","failed_login_count")?{conn.execute("ALTER TABLE users ADD COLUMN failed_login_count INTEGER NOT NULL DEFAULT 0",[])?;}
            if !Self::has_column(conn,"users","locked_until")?{conn.execute("ALTER TABLE users ADD COLUMN locked_until TEXT",[])?;}
            conn.execute_batch(r#"
            CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username_nocase ON users(username COLLATE NOCASE) WHERE username IS NOT NULL;
            CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_nocase ON users(email COLLATE NOCASE) WHERE email IS NOT NULL AND password_hash IS NOT NULL;
            CREATE TABLE IF NOT EXISTS internal_auth_bootstrap(
              singleton INTEGER PRIMARY KEY CHECK(singleton=1),admin_user_id TEXT NOT NULL UNIQUE,
              completed_at TEXT DEFAULT CURRENT_TIMESTAMP,FOREIGN KEY(admin_user_id) REFERENCES users(id)
            );
            CREATE TABLE IF NOT EXISTS auth_sessions(
              token_sha256 TEXT PRIMARY KEY,user_id TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              expires_at TEXT NOT NULL,last_seen_at TEXT DEFAULT CURRENT_TIMESTAMP,revoked_at TEXT,
              FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_auth_sessions_user ON auth_sessions(user_id,expires_at);
            CREATE TABLE IF NOT EXISTS password_reset_tokens(
              token_sha256 TEXT PRIMARY KEY,user_id TEXT NOT NULL,purpose TEXT NOT NULL,
              expires_at TEXT NOT NULL,used_at TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(user_id) REFERENCES users(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_password_reset_user ON password_reset_tokens(user_id,expires_at);
            CREATE TABLE IF NOT EXISTS account_audit_events(
              id INTEGER PRIMARY KEY AUTOINCREMENT,actor_user_id TEXT,target_user_id TEXT,event_type TEXT NOT NULL,
              detail_json TEXT NOT NULL DEFAULT '{}',created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(actor_user_id) REFERENCES users(id),FOREIGN KEY(target_user_id) REFERENCES users(id)
            );
            CREATE INDEX IF NOT EXISTS idx_account_audit_events ON account_audit_events(target_user_id,id DESC);
            "#)?;
        }
        if current < 22 && !Self::has_column(conn,"idempotency_keys","request_sha256")? {
            // Existing keys were created before requests were content-bound. Leaving
            // the new value NULL makes every attempted reuse conflict safely.
            conn.execute("ALTER TABLE idempotency_keys ADD COLUMN request_sha256 TEXT",[])?;
        }
        if current < 23 {
            if !Self::has_column(conn,"generation_runs","input_manifest_json")?{conn.execute("ALTER TABLE generation_runs ADD COLUMN input_manifest_json TEXT",[])?;}
            if !Self::has_column(conn,"generation_runs","input_manifest_sha256")?{conn.execute("ALTER TABLE generation_runs ADD COLUMN input_manifest_sha256 TEXT",[])?;}
        }
        if current < 24 {
            if !Self::has_column(conn,"generation_runs","output_contract_name")?{conn.execute("ALTER TABLE generation_runs ADD COLUMN output_contract_name TEXT",[])?;}
            if !Self::has_column(conn,"generation_runs","output_contract_version")?{conn.execute("ALTER TABLE generation_runs ADD COLUMN output_contract_version INTEGER",[])?;}
            if !Self::has_column(conn,"generation_runs","output_schema_json")?{conn.execute("ALTER TABLE generation_runs ADD COLUMN output_schema_json TEXT",[])?;}
            if !Self::has_column(conn,"generation_runs","output_schema_sha256")?{conn.execute("ALTER TABLE generation_runs ADD COLUMN output_schema_sha256 TEXT",[])?;}
        }
        if current < 25 {
            if !Self::has_column(conn,"research_queries","run_id")?{conn.execute("ALTER TABLE research_queries ADD COLUMN run_id TEXT",[])?;}
            if !Self::has_column(conn,"research_queries","plan_query_id")?{conn.execute("ALTER TABLE research_queries ADD COLUMN plan_query_id TEXT",[])?;}
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS research_runs(
              id TEXT PRIMARY KEY,project_id TEXT NOT NULL,search_plan_version INTEGER NOT NULL,
              input_manifest_json TEXT NOT NULL,input_manifest_sha256 TEXT NOT NULL,search_provider TEXT NOT NULL,
              status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),failure_json TEXT NOT NULL DEFAULT '[]',
              started_by_user_id TEXT NOT NULL,started_at TEXT NOT NULL,completed_at TEXT,
              manifest_artifact_version INTEGER,manifest_sha256 TEXT,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_research_runs_project ON research_runs(project_id,started_at DESC,id);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_research_runs_active ON research_runs(project_id) WHERE status='running';
            CREATE TABLE IF NOT EXISTS research_query_sources(
              query_id INTEGER NOT NULL,source_id INTEGER NOT NULL,validation_status TEXT NOT NULL,
              confidence REAL NOT NULL,supporting_excerpt TEXT NOT NULL,explanation TEXT NOT NULL,
              PRIMARY KEY(query_id,source_id),FOREIGN KEY(query_id) REFERENCES research_queries(id) ON DELETE CASCADE,
              FOREIGN KEY(source_id) REFERENCES research_sources(id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS research_query_requirements(
              query_id INTEGER NOT NULL,requirement_external_id TEXT NOT NULL,
              PRIMARY KEY(query_id,requirement_external_id),
              FOREIGN KEY(query_id) REFERENCES research_queries(id) ON DELETE CASCADE
            );
            "#)?;
        }
        if current < 26 {
            conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS artifact_approval_events(
              id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,artifact_type TEXT NOT NULL,
              artifact_version INTEGER NOT NULL,actor_user_id TEXT NOT NULL,role_at_decision TEXT NOT NULL,
              decision TEXT NOT NULL CHECK(decision IN ('approved','rejected')),notes TEXT,
              created_at TEXT DEFAULT CURRENT_TIMESTAMP,
              FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
              FOREIGN KEY(actor_user_id) REFERENCES users(id)
            );
            CREATE INDEX IF NOT EXISTS idx_artifact_approval_events
              ON artifact_approval_events(project_id,artifact_type,artifact_version,id);
            INSERT INTO artifact_approval_events(
              project_id,artifact_type,artifact_version,actor_user_id,role_at_decision,decision,notes,created_at
            )
            SELECT d.project_id,d.artifact_type,d.artifact_version,d.approver_user_id,
                   d.role_at_approval,d.decision,d.notes,d.created_at
            FROM artifact_approval_decisions d
            WHERE NOT EXISTS(
              SELECT 1 FROM artifact_approval_events e
              WHERE e.project_id=d.project_id AND e.artifact_type=d.artifact_type
                AND e.artifact_version=d.artifact_version AND e.actor_user_id=d.approver_user_id
                AND e.decision=d.decision AND e.created_at=d.created_at
            );
            "#)?;
        }
        if current < 27 {
            if !Self::has_column(conn,"projects","archived_at")? {
                conn.execute("ALTER TABLE projects ADD COLUMN archived_at TEXT",[])?;
            }
            if !Self::has_column(conn,"projects","archived_by_user_id")? {
                conn.execute("ALTER TABLE projects ADD COLUMN archived_by_user_id TEXT",[])?;
            }
            conn.execute("CREATE INDEX IF NOT EXISTS idx_projects_active_updated ON projects(archived_at,updated_at DESC)",[])?;
        }
        // Section catalog backfill is idempotent and safe on every startup.
        conn.execute_batch(
            r#"
        INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required)
        SELECT project_id,section_key,title,position,1 FROM (
          SELECT project_id,section_key,MAX(title) title,
                 ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY MIN(id)) - 1 AS position
          FROM section_versions GROUP BY project_id,section_key
        );
        "#,
        )?;
        // Legacy stage reconstruction is a one-time migration only. Re-running it
        // on every process start could regress reviewed/exported projects to writing.
        if current < 4 {
            conn.execute_batch(r#"
            UPDATE projects SET stage='documents' WHERE stage='intake' AND EXISTS(SELECT 1 FROM documents d WHERE d.project_id=projects.id);
            UPDATE projects SET stage='requirements' WHERE stage IN ('intake','documents') AND EXISTS(SELECT 1 FROM requirements r WHERE r.project_id=projects.id);
            UPDATE projects SET stage='interview' WHERE stage='requirements' AND EXISTS(SELECT 1 FROM requirements r WHERE r.project_id=projects.id) AND NOT EXISTS(SELECT 1 FROM requirements r WHERE r.project_id=projects.id AND r.approved=0);
            UPDATE projects SET interview_generated=1 WHERE EXISTS(SELECT 1 FROM interview_questions q WHERE q.project_id=projects.id);
            UPDATE projects SET stage='research' WHERE stage='interview' AND interview_generated=1 AND NOT EXISTS(SELECT 1 FROM interview_questions q WHERE q.project_id=projects.id AND q.status='open');
            UPDATE projects SET stage='writing' WHERE stage NOT IN ('review','export') AND EXISTS(SELECT 1 FROM section_versions sv WHERE sv.project_id=projects.id);
            UPDATE projects SET stage='review' WHERE stage='writing' AND EXISTS(SELECT 1 FROM project_sections ps WHERE ps.project_id=projects.id) AND NOT EXISTS(
              SELECT 1 FROM project_sections ps WHERE ps.project_id=projects.id AND ps.required=1 AND NOT EXISTS(
                SELECT 1 FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1));
            "#)?;
        }
        Ok(())
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let workflow_registry = WorkflowRegistry::load()?;
        let conn = Connection::open(&path_buf)?;
        Self::configure(&conn)?;
        conn.execute_batch(r#"
        CREATE TABLE IF NOT EXISTS projects(
          id TEXT PRIMARY KEY,title TEXT NOT NULL,sponsor TEXT,mechanism TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE IF NOT EXISTS schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT DEFAULT CURRENT_TIMESTAMP);

        CREATE TABLE IF NOT EXISTS project_sections(
          project_id TEXT NOT NULL, section_key TEXT NOT NULL, title TEXT NOT NULL,
          position INTEGER NOT NULL, required INTEGER NOT NULL DEFAULT 1, origin TEXT NOT NULL DEFAULT 'configured',
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          PRIMARY KEY(project_id,section_key), FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_project_section_position ON project_sections(project_id,position);

        CREATE TABLE IF NOT EXISTS section_versions(
          id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,section_key TEXT NOT NULL,title TEXT NOT NULL,
          body TEXT NOT NULL,html TEXT,source TEXT NOT NULL,editor_name TEXT,approved INTEGER NOT NULL DEFAULT 0,
          base_version_id INTEGER,restored_from_version_id INTEGER,generation_run_id TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_section_versions ON section_versions(project_id,section_key,id DESC);

        CREATE TABLE IF NOT EXISTS documents(
          id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,
          text TEXT NOT NULL,sha256 TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_document_hash ON documents(project_id,sha256);
        CREATE TABLE IF NOT EXISTS document_chunks(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, document_id INTEGER NOT NULL,
          ordinal INTEGER NOT NULL, start_word INTEGER NOT NULL, end_word INTEGER NOT NULL, text TEXT NOT NULL,
          UNIQUE(document_id,ordinal), FOREIGN KEY(document_id) REFERENCES documents(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_document_chunks_project ON document_chunks(project_id,document_id,ordinal);
        CREATE TABLE IF NOT EXISTS analyses(id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,kind TEXT NOT NULL,content TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP);
        CREATE TABLE IF NOT EXISTS approvals(
          id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,section_key TEXT NOT NULL,
          version_id INTEGER NOT NULL,approved_by TEXT,approved_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(version_id) REFERENCES section_versions(id)
        );
        CREATE TABLE IF NOT EXISTS project_members(
          project_id TEXT NOT NULL, member_name TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'Contributor',
          joined_at TEXT DEFAULT CURRENT_TIMESTAMP,last_seen_at TEXT DEFAULT CURRENT_TIMESTAMP,
          PRIMARY KEY(project_id,member_name),FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS project_messages(
          id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,author TEXT NOT NULL,body TEXT NOT NULL,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_project_messages ON project_messages(project_id,id DESC);

        CREATE TABLE IF NOT EXISTS project_workflows(
          project_id TEXT PRIMARY KEY, definition_version INTEGER NOT NULL DEFAULT 1,
          definition_sha256 TEXT, config_version INTEGER NOT NULL DEFAULT 1, config_sha256 TEXT,
          config_json TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS workflow_events(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL,
          event_type TEXT NOT NULL, actor TEXT, payload_json TEXT NOT NULL DEFAULT '{}',
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_workflow_events ON workflow_events(project_id,id DESC);
        CREATE TABLE IF NOT EXISTS workflow_artifacts(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL,
          artifact_type TEXT NOT NULL, version INTEGER NOT NULL, body_json TEXT NOT NULL,
          content_sha256 TEXT NOT NULL, source TEXT NOT NULL, author TEXT,
          approved INTEGER NOT NULL DEFAULT 0, approved_by TEXT, approved_at TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id,artifact_type,version),
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_workflow_artifacts ON workflow_artifacts(project_id,artifact_type,version DESC);
        CREATE TABLE IF NOT EXISTS generation_runs(
          id TEXT PRIMARY KEY,project_id TEXT NOT NULL,task_kind TEXT NOT NULL,
          routing_mode TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,
          prompt_sha256 TEXT NOT NULL,response_sha256 TEXT,high_value INTEGER NOT NULL DEFAULT 0,
          status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),
          input_manifest_json TEXT,input_manifest_sha256 TEXT,
          output_contract_name TEXT,output_contract_version INTEGER,output_schema_json TEXT,output_schema_sha256 TEXT,
          error TEXT,started_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_generation_runs_project ON generation_runs(project_id,id);
        CREATE TABLE IF NOT EXISTS idempotency_keys(
          user_id TEXT NOT NULL,key TEXT NOT NULL,method TEXT NOT NULL,path TEXT NOT NULL,request_sha256 TEXT NOT NULL,
          state TEXT NOT NULL CHECK(state IN ('in_progress','complete')),
          status_code INTEGER,content_type TEXT,response_body BLOB,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
          PRIMARY KEY(user_id,key),FOREIGN KEY(user_id) REFERENCES users(id)
        );
        CREATE INDEX IF NOT EXISTS idx_idempotency_created ON idempotency_keys(created_at);
        CREATE TABLE IF NOT EXISTS artifact_approval_decisions(
          project_id TEXT NOT NULL,artifact_type TEXT NOT NULL,artifact_version INTEGER NOT NULL,
          approver_user_id TEXT NOT NULL,role_at_approval TEXT NOT NULL,decision TEXT NOT NULL CHECK(decision IN ('approved','rejected')),
          notes TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          PRIMARY KEY(project_id,artifact_type,artifact_version,approver_user_id),
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(approver_user_id) REFERENCES users(id)
        );
        CREATE INDEX IF NOT EXISTS idx_artifact_approval_decisions ON artifact_approval_decisions(project_id,artifact_type,artifact_version);
        CREATE TABLE IF NOT EXISTS review_panel_plans(
          id TEXT PRIMARY KEY,project_id TEXT NOT NULL,solicitation_profile_version INTEGER NOT NULL,
          registry_definition_version INTEGER NOT NULL,mode TEXT NOT NULL,roles_json TEXT NOT NULL,
          synthetic_review_notice TEXT NOT NULL,
          status TEXT NOT NULL CHECK(status IN ('draft','approved')),created_by_user_id TEXT NOT NULL,
          approved_by_user_id TEXT,created_at TEXT DEFAULT CURRENT_TIMESTAMP,approved_at TEXT,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS proposal_review_snapshots(
          id TEXT PRIMARY KEY,project_id TEXT NOT NULL,snapshot_json TEXT NOT NULL,content_sha256 TEXT NOT NULL,
          created_by_user_id TEXT NOT NULL,created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS review_simulation_runs(
          id TEXT PRIMARY KEY,project_id TEXT NOT NULL,snapshot_id TEXT NOT NULL,panel_plan_id TEXT NOT NULL,
          rubric_version_id TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),
          result_json TEXT,result_sha256 TEXT,error TEXT,created_by_user_id TEXT NOT NULL,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
          FOREIGN KEY(snapshot_id) REFERENCES proposal_review_snapshots(id),FOREIGN KEY(panel_plan_id) REFERENCES review_panel_plans(id)
        );
        CREATE TABLE IF NOT EXISTS causal_models(
          id INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL,review_run_id TEXT NOT NULL,version INTEGER NOT NULL,
          body_json TEXT NOT NULL,content_sha256 TEXT NOT NULL,author_user_id TEXT NOT NULL,confirmed INTEGER NOT NULL DEFAULT 0,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,UNIQUE(review_run_id,version),
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,FOREIGN KEY(review_run_id) REFERENCES review_simulation_runs(id)
        );
        CREATE INDEX IF NOT EXISTS idx_review_runs_project ON review_simulation_runs(project_id,created_at DESC);

        CREATE TABLE IF NOT EXISTS requirements(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, external_id TEXT NOT NULL, category TEXT NOT NULL,
          requirement TEXT NOT NULL, mandatory INTEGER NOT NULL DEFAULT 0, evidence_needed_json TEXT NOT NULL,
          dependencies_json TEXT NOT NULL, source_clue TEXT, source_document TEXT, source_locator TEXT,
          status TEXT NOT NULL DEFAULT 'unverified', approved INTEGER NOT NULL DEFAULT 0, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id, external_id)
        );
        CREATE INDEX IF NOT EXISTS idx_requirements_project ON requirements(project_id,status,mandatory);

        CREATE TABLE IF NOT EXISTS interview_questions(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, requirement_external_id TEXT NOT NULL,
          question TEXT NOT NULL, answer_type TEXT NOT NULL, choices_json TEXT NOT NULL, unit TEXT, why_needed TEXT,
          evidence_requested INTEGER NOT NULL DEFAULT 0, priority INTEGER NOT NULL DEFAULT 0,
          status TEXT NOT NULL DEFAULT 'open', created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_interview_open ON interview_questions(project_id,status,priority DESC,id);
        CREATE TABLE IF NOT EXISTS interview_answers(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, question_id INTEGER NOT NULL,
          value_json TEXT NOT NULL, confidence TEXT NOT NULL, classification TEXT NOT NULL,
          notes TEXT, answered_by TEXT, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(question_id) REFERENCES interview_questions(id)
        );
        CREATE INDEX IF NOT EXISTS idx_answers_question ON interview_answers(project_id,question_id,id DESC);

        CREATE TABLE IF NOT EXISTS research_queries(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, requirement_external_id TEXT NOT NULL,
          query TEXT NOT NULL, preferred_domains_json TEXT NOT NULL, rationale TEXT, status TEXT NOT NULL DEFAULT 'queued',
          run_id TEXT,plan_query_id TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_research_queries ON research_queries(project_id,status,id);
        CREATE TABLE IF NOT EXISTS research_sources(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, query_id INTEGER, title TEXT NOT NULL,
          url TEXT NOT NULL, text TEXT NOT NULL, retrieved_at TEXT NOT NULL, content_sha256 TEXT NOT NULL, http_status INTEGER NOT NULL,
          UNIQUE(project_id,url,content_sha256), FOREIGN KEY(query_id) REFERENCES research_queries(id)
        );
        CREATE INDEX IF NOT EXISTS idx_research_sources ON research_sources(project_id,query_id);
        CREATE TABLE IF NOT EXISTS research_runs(
          id TEXT PRIMARY KEY,project_id TEXT NOT NULL,search_plan_version INTEGER NOT NULL,
          input_manifest_json TEXT NOT NULL,input_manifest_sha256 TEXT NOT NULL,search_provider TEXT NOT NULL,
          status TEXT NOT NULL CHECK(status IN ('running','complete','failed')),failure_json TEXT NOT NULL DEFAULT '[]',
          started_by_user_id TEXT NOT NULL,started_at TEXT NOT NULL,completed_at TEXT,
          manifest_artifact_version INTEGER,manifest_sha256 TEXT,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_research_runs_project ON research_runs(project_id,started_at DESC,id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_research_runs_active ON research_runs(project_id) WHERE status='running';
        CREATE TABLE IF NOT EXISTS research_query_sources(
          query_id INTEGER NOT NULL,source_id INTEGER NOT NULL,validation_status TEXT NOT NULL,
          confidence REAL NOT NULL,supporting_excerpt TEXT NOT NULL,explanation TEXT NOT NULL,
          PRIMARY KEY(query_id,source_id),FOREIGN KEY(query_id) REFERENCES research_queries(id) ON DELETE CASCADE,
          FOREIGN KEY(source_id) REFERENCES research_sources(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS research_query_requirements(
          query_id INTEGER NOT NULL,requirement_external_id TEXT NOT NULL,
          PRIMARY KEY(query_id,requirement_external_id),
          FOREIGN KEY(query_id) REFERENCES research_queries(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS evidence(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, requirement_external_id TEXT,
          source_type TEXT NOT NULL, source_ref TEXT NOT NULL, claim TEXT NOT NULL, passage TEXT NOT NULL,
          source_url TEXT, source_locator TEXT, confidence REAL NOT NULL DEFAULT 0.0, status TEXT NOT NULL DEFAULT 'candidate',
          created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_evidence_req ON evidence(project_id,requirement_external_id,status);
        CREATE TABLE IF NOT EXISTS citations(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, evidence_id INTEGER NOT NULL,
          citation_key TEXT NOT NULL, title TEXT NOT NULL, url TEXT, passage TEXT NOT NULL,
          content_sha256 TEXT NOT NULL, verified INTEGER NOT NULL DEFAULT 0, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(evidence_id) REFERENCES evidence(id)
        );
        CREATE INDEX IF NOT EXISTS idx_citations_project ON citations(project_id,verified);

        CREATE TABLE IF NOT EXISTS project_design(
          project_id TEXT PRIMARY KEY, profile_json TEXT NOT NULL, content_sha256 TEXT NOT NULL,
          updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS export_snapshots(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, snapshot_json TEXT NOT NULL,
          content_sha256 TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS clinical_studies(
          project_id TEXT PRIMARY KEY, version INTEGER NOT NULL, study_json TEXT NOT NULL, content_sha256 TEXT NOT NULL,
          updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS clinical_study_history(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, version INTEGER NOT NULL, study_json TEXT NOT NULL,
          content_sha256 TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id,version), FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_clinical_history_project ON clinical_study_history(project_id,version DESC);

        CREATE TABLE IF NOT EXISTS competitive_profiles(
          project_id TEXT PRIMARY KEY, version INTEGER NOT NULL, source_fingerprint TEXT NOT NULL,
          profile_json TEXT NOT NULL, content_sha256 TEXT NOT NULL, model TEXT NOT NULL,
          updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS competitive_profile_history(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, version INTEGER NOT NULL,
          source_fingerprint TEXT NOT NULL, profile_json TEXT NOT NULL, content_sha256 TEXT NOT NULL,
          model TEXT NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id,version), FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_competitive_profile_history ON competitive_profile_history(project_id,version DESC);

        CREATE TABLE IF NOT EXISTS competitive_runs(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, profile_version INTEGER NOT NULL,
          input_fingerprint TEXT NOT NULL, config_sha256 TEXT NOT NULL, status TEXT NOT NULL,
          provider_status_json TEXT NOT NULL DEFAULT '[]', strategy_json TEXT, strategy_sha256 TEXT, strategy_model TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP, completed_at TEXT,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_competitive_runs_project ON competitive_runs(project_id,id DESC);

        CREATE TABLE IF NOT EXISTS competitor_candidates(
          id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, project_id TEXT NOT NULL,
          candidate_key TEXT NOT NULL, name TEXT NOT NULL, rank INTEGER NOT NULL, overall_score REAL NOT NULL,
          grant_score REAL NOT NULL, publication_score REAL NOT NULL, clinical_trial_score REAL NOT NULL,
          patent_ip_score REAL NOT NULL, technology_score REAL NOT NULL, breadth_score REAL NOT NULL,
          asset_count INTEGER NOT NULL, asset_counts_json TEXT NOT NULL, dimension_coverage_json TEXT NOT NULL,
          UNIQUE(run_id,candidate_key), FOREIGN KEY(run_id) REFERENCES competitive_runs(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_competitor_candidates_rank ON competitor_candidates(project_id,run_id,rank);

        CREATE TABLE IF NOT EXISTS competitor_assets(
          id INTEGER PRIMARY KEY AUTOINCREMENT, run_id INTEGER NOT NULL, project_id TEXT NOT NULL,
          candidate_key TEXT NOT NULL, asset_key TEXT NOT NULL, provider TEXT NOT NULL, asset_type TEXT NOT NULL,
          external_id TEXT NOT NULL, title TEXT NOT NULL, summary TEXT NOT NULL, url TEXT, year INTEGER, amount REAL,
          dimension_id TEXT, metadata_json TEXT NOT NULL, relevance REAL NOT NULL, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(run_id,asset_key,candidate_key), FOREIGN KEY(run_id) REFERENCES competitive_runs(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_competitor_assets_candidate ON competitor_assets(project_id,run_id,candidate_key,relevance DESC);
        CREATE INDEX IF NOT EXISTS idx_competitor_assets_type ON competitor_assets(project_id,run_id,asset_type,relevance DESC);

        CREATE TABLE IF NOT EXISTS competitive_update_events(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, from_run_id INTEGER, to_run_id INTEGER NOT NULL,
          refresh_reason_json TEXT NOT NULL, delta_json TEXT NOT NULL, summary TEXT NOT NULL, material INTEGER NOT NULL DEFAULT 0,
          text_refresh_status TEXT NOT NULL DEFAULT 'pending', text_refresh_errors_json TEXT NOT NULL DEFAULT '[]', processed_at TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
          FOREIGN KEY(from_run_id) REFERENCES competitive_runs(id) ON DELETE SET NULL,
          FOREIGN KEY(to_run_id) REFERENCES competitive_runs(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_competitive_update_events_project ON competitive_update_events(project_id,id DESC);

        CREATE TABLE IF NOT EXISTS competitive_section_updates(
          id INTEGER PRIMARY KEY AUTOINCREMENT, event_id INTEGER NOT NULL, project_id TEXT NOT NULL, section_key TEXT NOT NULL,
          base_version_id INTEGER NOT NULL, proposed_version_id INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
          resolved_version_id INTEGER, created_at TEXT DEFAULT CURRENT_TIMESTAMP, resolved_at TEXT,
          UNIQUE(event_id,section_key),
          FOREIGN KEY(event_id) REFERENCES competitive_update_events(id) ON DELETE CASCADE,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
          FOREIGN KEY(base_version_id) REFERENCES section_versions(id) ON DELETE CASCADE,
          FOREIGN KEY(proposed_version_id) REFERENCES section_versions(id) ON DELETE CASCADE,
          FOREIGN KEY(resolved_version_id) REFERENCES section_versions(id) ON DELETE SET NULL
        );
        CREATE INDEX IF NOT EXISTS idx_competitive_section_updates_pending ON competitive_section_updates(project_id,status,section_key,event_id DESC);

        CREATE TABLE IF NOT EXISTS compliance_profiles(
          project_id TEXT PRIMARY KEY, version INTEGER NOT NULL, source_fingerprint TEXT NOT NULL,
          profile_json TEXT NOT NULL, content_sha256 TEXT NOT NULL, model TEXT NOT NULL,
          approved INTEGER NOT NULL DEFAULT 0, updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS compliance_profile_history(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, version INTEGER NOT NULL,
          source_fingerprint TEXT NOT NULL, profile_json TEXT NOT NULL, content_sha256 TEXT NOT NULL,
          model TEXT NOT NULL, approved INTEGER NOT NULL DEFAULT 0, created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id,version), FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_compliance_history ON compliance_profile_history(project_id,version DESC);
        CREATE TABLE IF NOT EXISTS compliance_rule_sources(
          project_id TEXT NOT NULL, profile_version INTEGER NOT NULL, rule_id TEXT NOT NULL,
          source_status TEXT NOT NULL CHECK(source_status IN ('located','not_located')),
          source_hint TEXT NOT NULL, source_document_id INTEGER, source_start_offset INTEGER,
          source_end_offset INTEGER, source_page INTEGER, source_excerpt TEXT NOT NULL,
          PRIMARY KEY(project_id,profile_version,rule_id),
          CHECK(
            (source_status='located' AND source_document_id IS NOT NULL AND source_start_offset IS NOT NULL
              AND source_end_offset IS NOT NULL AND source_start_offset>=0 AND source_end_offset>source_start_offset
              AND source_excerpt<>'SOURCE NOT LOCATED')
            OR
            (source_status='not_located' AND source_document_id IS NULL AND source_start_offset IS NULL
              AND source_end_offset IS NULL AND source_page IS NULL AND source_excerpt='SOURCE NOT LOCATED')
          ),
          FOREIGN KEY(project_id,profile_version) REFERENCES compliance_profile_history(project_id,version) ON DELETE CASCADE,
          FOREIGN KEY(source_document_id) REFERENCES documents(id) ON DELETE RESTRICT
        );
        CREATE TRIGGER IF NOT EXISTS compliance_rule_source_exact_insert
        BEFORE INSERT ON compliance_rule_sources
        WHEN NEW.source_status='located' AND NOT EXISTS(
          SELECT 1 FROM documents d
          WHERE d.id=NEW.source_document_id AND d.project_id=NEW.project_id
            AND CAST(substr(CAST(d.text AS BLOB),NEW.source_start_offset+1,NEW.source_end_offset-NEW.source_start_offset) AS TEXT)=NEW.source_excerpt
        )
        BEGIN SELECT RAISE(ABORT,'compliance source excerpt is not the exact document byte slice'); END;
        CREATE TRIGGER IF NOT EXISTS compliance_rule_source_exact_update
        BEFORE UPDATE ON compliance_rule_sources
        WHEN NEW.source_status='located' AND NOT EXISTS(
          SELECT 1 FROM documents d
          WHERE d.id=NEW.source_document_id AND d.project_id=NEW.project_id
            AND CAST(substr(CAST(d.text AS BLOB),NEW.source_start_offset+1,NEW.source_end_offset-NEW.source_start_offset) AS TEXT)=NEW.source_excerpt
        )
        BEGIN SELECT RAISE(ABORT,'compliance source excerpt is not the exact document byte slice'); END;
        CREATE TRIGGER IF NOT EXISTS compliance_source_document_immutable
        BEFORE UPDATE OF text ON documents
        WHEN EXISTS(SELECT 1 FROM compliance_rule_sources s WHERE s.source_document_id=OLD.id AND s.source_status='located')
        BEGIN SELECT RAISE(ABORT,'document text is immutable while exact compliance provenance references it'); END;
        CREATE TABLE IF NOT EXISTS compliance_resolutions(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, rule_id TEXT NOT NULL,
          status TEXT NOT NULL, notes TEXT NOT NULL DEFAULT '', resolved_by TEXT,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id,rule_id), FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS compliance_measurements(
          project_id TEXT PRIMARY KEY, approved_sections_fingerprint TEXT NOT NULL,
          measurements_json TEXT NOT NULL, updated_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS submission_artifacts(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, slot TEXT NOT NULL,
          filename TEXT NOT NULL, path TEXT NOT NULL, sha256 TEXT NOT NULL, extension TEXT NOT NULL,
          created_at TEXT DEFAULT CURRENT_TIMESTAMP,
          UNIQUE(project_id,slot,sha256), FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_submission_artifacts_project ON submission_artifacts(project_id,slot,id);
        "#)?;
        Self::migrate(&conn)?;
        let definition_sha256 = workflow_registry.definition_sha256()?;
        let legacy = serde_json::to_string(&workflow_registry.legacy_config()?)?;
        conn.execute(r#"INSERT OR IGNORE INTO project_workflows(project_id,definition_version,definition_sha256,config_json)
          SELECT id,?1,?2,?3 FROM projects"#,params![workflow_registry.definition_version,definition_sha256,legacy])?;
        let mut stored = conn.prepare("SELECT project_id,config_json FROM project_workflows")?;
        let rows = stored.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut updates = Vec::new();
        for row in rows {
            let (project_id, raw) = row?;
            let mut value: Value = serde_json::from_str(&raw)
                .context("stored workflow configuration is invalid JSON")?;
            let object = value
                .as_object_mut()
                .context("stored workflow configuration must be a JSON object")?;
            object.insert(
                "definition_version".into(),
                json!(workflow_registry.definition_version),
            );
            updates.push((project_id, serde_json::to_string(&value)?));
        }
        drop(stored);
        for (project_id, config_json) in updates {
            let config_sha256 = sha256_hex(config_json.as_bytes());
            conn.execute("UPDATE project_workflows SET definition_version=?1,definition_sha256=?2,config_sha256=?3,config_json=?4 WHERE project_id=?5",
              params![workflow_registry.definition_version,definition_sha256,config_sha256,config_json,project_id])?;
        }
        conn.execute("INSERT OR IGNORE INTO schema_migrations(version) VALUES(27)",[])?;
        Ok(Self {
            path: path_buf,
            workflow_registry,
        })
    }

    fn touch_project_conn(c: &Connection, project: &str) -> Result<()> {
        c.execute(
            "UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",
            [project],
        )?;
        Ok(())
    }

    pub fn create_project(
        &self,
        id: &str,
        title: &str,
        sponsor: Option<&str>,
        mechanism: Option<&str>,
        sections: &[String],
    ) -> Result<()> {
        let workflow = self.workflow_registry.legacy_config()?;
        self.create_project_with_workflow(id, title, sponsor, mechanism, sections, &workflow, None)
    }

    pub fn create_project_with_workflow(
        &self,
        id: &str,
        title: &str,
        sponsor: Option<&str>,
        mechanism: Option<&str>,
        sections: &[String],
        workflow: &WorkflowConfig,
        actor: Option<&str>,
    ) -> Result<()> {
        workflow.validate(&self.workflow_registry)?;
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        tx.execute("INSERT INTO projects(id,title,sponsor,mechanism,stage,updated_at) VALUES(?1,?2,?3,?4,'intake',CURRENT_TIMESTAMP)",params![id,title,sponsor,mechanism])?;
        for (position, title) in sections.iter().filter(|s| !s.trim().is_empty()).enumerate() {
            let key = section_key(title);
            tx.execute("INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required) VALUES(?1,?2,?3,?4,1)",params![id,key,title.trim(),position as i64])?;
        }
        let config_json = serde_json::to_string(workflow)?;
        tx.execute("INSERT INTO project_workflows(project_id,definition_version,definition_sha256,config_version,config_sha256,config_json) VALUES(?1,?2,?3,1,?4,?5)",params![id,self.workflow_registry.definition_version,self.workflow_registry.definition_sha256()?,sha256_hex(config_json.as_bytes()),config_json])?;
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'project_workflow_created',?2,?3)",params![id,actor,serde_json::to_string(&json!({"workflow":workflow}))?])?;
        tx.commit()?;
        Ok(())
    }

    pub fn workflow_config(&self, project: &str) -> Result<WorkflowConfig> {
        let c = self.conn()?;
        let raw = c
            .query_row(
                "SELECT config_json FROM project_workflows WHERE project_id=?1",
                [project],
                |r| r.get::<_, String>(0),
            )
            .context("project workflow not found")?;
        let config: WorkflowConfig =
            serde_json::from_str(&raw).context("stored project workflow is invalid")?;
        config.validate(&self.workflow_registry)?;
        Ok(config)
    }

    pub fn workflow_registry_json(&self) -> Result<Value> {
        self.workflow_registry.as_json()
    }

    pub fn default_workflow_config(&self) -> Result<WorkflowConfig> {
        self.workflow_registry.default_config()
    }

    pub fn workflow_config_record_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row("SELECT definition_version,definition_sha256,config_version,config_sha256,config_json,created_at,updated_at FROM project_workflows WHERE project_id=?1",[project],|r|
            Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,i64>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?)))
            .context("project workflow not found")?;
        Ok(
            json!({"definition_version":row.0,"definition_sha256":row.1,"config_version":row.2,"config_sha256":row.3,
          "config":serde_json::from_str::<Value>(&row.4).context("stored project workflow is invalid")?,"created_at":row.5,"updated_at":row.6}),
        )
    }

    pub fn workflow_impact_json(&self, project: &str, proposed: &WorkflowConfig) -> Result<Value> {
        proposed.validate(&self.workflow_registry)?;
        let current = self.workflow_config(project)?;
        let current_enabled: BTreeSet<&str> =
            current.enabled_modules.iter().map(String::as_str).collect();
        let proposed_enabled: BTreeSet<&str> = proposed
            .enabled_modules
            .iter()
            .map(String::as_str)
            .collect();
        let added: Vec<&str> = proposed_enabled
            .difference(&current_enabled)
            .copied()
            .collect();
        let removed: Vec<&str> = current_enabled
            .difference(&proposed_enabled)
            .copied()
            .collect();
        let current_required: BTreeSet<&str> = current
            .required_modules
            .iter()
            .map(String::as_str)
            .collect();
        let proposed_required: BTreeSet<&str> = proposed
            .required_modules
            .iter()
            .map(String::as_str)
            .collect();
        let newly_required: Vec<&str> = proposed_required
            .difference(&current_required)
            .copied()
            .collect();
        let newly_advisory: Vec<&str> = current_required
            .difference(&proposed_required)
            .copied()
            .collect();
        let mut preserved_history = Vec::new();
        for key in &removed {
            if let Some(artifact_type) = self
                .workflow_registry
                .module(key)
                .and_then(|module| module.step.artifact_type.as_deref())
            {
                let state = self.workflow_artifact_state(project, artifact_type)?;
                if state.get("version").is_some_and(|value| !value.is_null()) {
                    preserved_history.push(json!({"module":key,"artifact_type":artifact_type,"latest_version":state.get("version")}));
                }
            }
        }
        Ok(
            json!({"project_id":project,"current_config":current,"proposed_config":proposed,"added_modules":added,
          "removed_modules":removed,"newly_required_modules":newly_required,"newly_advisory_modules":newly_advisory,
          "preserved_hidden_history":preserved_history,"destructive":false}),
        )
    }

    pub fn update_workflow_config(
        &self,
        project: &str,
        proposed: &WorkflowConfig,
        expected_config_version: i64,
        actor: &str,
    ) -> Result<Value> {
        proposed.validate(&self.workflow_registry)?;
        if actor.trim().is_empty() {
            bail!("authenticated workflow actor is required");
        }
        let impact = self.workflow_impact_json(project, proposed)?;
        let raw = serde_json::to_string(proposed)?;
        let sha = sha256_hex(raw.as_bytes());
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let changed=tx.execute(r#"UPDATE project_workflows SET config_json=?1,config_sha256=?2,config_version=config_version+1,
          definition_version=?3,definition_sha256=?4,updated_at=CURRENT_TIMESTAMP WHERE project_id=?5 AND config_version=?6"#,
          params![raw,sha,self.workflow_registry.definition_version,self.workflow_registry.definition_sha256()?,project,expected_config_version])?;
        if changed != 1 {
            let actual: Option<i64> = tx
                .query_row(
                    "SELECT config_version FROM project_workflows WHERE project_id=?1",
                    [project],
                    |r| r.get(0),
                )
                .optional()?;
            bail!("workflow configuration changed since editing began: expected version {expected_config_version}, found {}",actual.map(|v|v.to_string()).unwrap_or_else(||"missing".into()));
        }
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'project_workflow_updated',?2,?3)",params![project,actor,serde_json::to_string(&impact)?])?;
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        self.workflow_config_record_json(project)
    }

    pub fn workflow_module_enabled(&self, project: &str, module: &str) -> Result<bool> {
        Ok(self.workflow_config(project)?.enabled(module))
    }

    pub fn workflow_module_required(&self,project:&str,module:&str)->Result<bool>{
        let config=self.workflow_config(project)?;
        Ok(config.required(&self.workflow_registry,module))
    }

    pub fn workflow_events_json(&self, project: &str, limit: usize) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,event_type,actor,payload_json,created_at FROM workflow_events WHERE project_id=?1 ORDER BY id DESC LIMIT ?2")?;
        let rows=st.query_map(params![project,limit.clamp(1,500) as i64],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"event_type":r.get::<_,String>(1)?,"actor":r.get::<_,Option<String>>(2)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(json!({})),"created_at":r.get::<_,String>(4)?})))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Value::Array(out))
    }

    fn workflow_artifact_state(&self, project: &str, artifact_type: &str) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row("SELECT id,version,approved,approved_by,approved_at,created_at,author,source,content_sha256 FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 ORDER BY version DESC LIMIT 1",params![project,artifact_type],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"version":r.get::<_,i64>(1)?,"approved":r.get::<_,i64>(2)?!=0,"approved_by":r.get::<_,Option<String>>(3)?,"approved_at":r.get::<_,Option<String>>(4)?,"created_at":r.get::<_,String>(5)?,"author":r.get::<_,Option<String>>(6)?,"source":r.get::<_,String>(7)?,"sha256":r.get::<_,String>(8)?}))).optional()?;
        Ok(row.unwrap_or_else(|| json!({"id":null,"version":null,"approved":false})))
    }

    pub fn workflow_artifact_json(&self, project: &str, artifact_type: &str) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row("SELECT id,version,body_json,content_sha256,source,author,approved,approved_by,approved_at,created_at FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 ORDER BY version DESC LIMIT 1",params![project,artifact_type],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"artifact_type":artifact_type,"version":r.get::<_,i64>(1)?,"body":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null),"sha256":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"author":r.get::<_,Option<String>>(5)?,"approved":r.get::<_,i64>(6)?!=0,"approved_by":r.get::<_,Option<String>>(7)?,"approved_at":r.get::<_,Option<String>>(8)?,"created_at":r.get::<_,String>(9)?}))).optional()?;
        Ok(row.unwrap_or_else(
            || json!({"artifact_type":artifact_type,"version":null,"body":null,"approved":false}),
        ))
    }

    fn latest_approved_artifact_json(c: &Connection, project: &str, artifact_type: &str) -> Result<Value> {
        let row=c.query_row("SELECT id,version,body_json,content_sha256,approved_by,approved_at FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 AND approved=1 ORDER BY version DESC LIMIT 1",params![project,artifact_type],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"artifact_type":artifact_type,"version":r.get::<_,i64>(1)?,"body":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(Value::Null),"sha256":r.get::<_,String>(3)?,"approved_by":r.get::<_,Option<String>>(4)?,"approved_at":r.get::<_,Option<String>>(5)?}))).optional()?;
        Ok(row.unwrap_or_else(||json!({"artifact_type":artifact_type,"version":null,"body":null,"approved":false})))
    }

    /// Authoritative reference catalog for structured editors. Every identifier
    /// offered here is scoped to this project and comes from an approved upstream
    /// artifact or an active project record.
    pub fn workflow_editor_context_json(&self, project: &str) -> Result<Value> {
        let c=self.conn()?;
        let solicitation=Self::latest_approved_artifact_json(&c,project,"solicitation_profile")?;
        let framework=Self::latest_approved_artifact_json(&c,project,"research_framework")?;
        let aims=Self::latest_approved_artifact_json(&c,project,"aim_set")?;
        let search_plan=Self::latest_approved_artifact_json(&c,project,"literature_search_plan")?;
        let literature=Self::latest_approved_artifact_json(&c,project,"literature_manifest")?;

        let mut members=Vec::new();
        {let mut st=c.prepare("SELECT u.id,u.display_name,u.email,pm.role FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 AND u.active=1 ORDER BY lower(u.display_name),u.id")?;for row in st.query_map([project],|r|Ok(json!({"user_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"email":r.get::<_,Option<String>>(2)?,"role":r.get::<_,String>(3)?})))?{members.push(row?);}}
        let mut evidence=Vec::new();
        {let mut st=c.prepare("SELECT id,requirement_external_id,claim,status FROM evidence WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_id":r.get::<_,Option<String>>(1)?,"claim":r.get::<_,String>(2)?,"status":r.get::<_,String>(3)?})))?{evidence.push(row?);}}
        let mut sources=Vec::new();
        {let mut st=c.prepare("SELECT id,title,url,retrieved_at,http_status FROM research_sources WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"title":r.get::<_,String>(1)?,"url":r.get::<_,String>(2)?,"retrieved_at":r.get::<_,String>(3)?,"http_status":r.get::<_,i64>(4)?})))?{sources.push(row?);}}
        let mut citations=Vec::new();
        {let mut st=c.prepare("SELECT id,evidence_id,citation_key,title,verified FROM citations WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"evidence_id":r.get::<_,i64>(1)?,"citation_key":r.get::<_,String>(2)?,"title":r.get::<_,String>(3)?,"verified":r.get::<_,i64>(4)?!=0})))?{citations.push(row?);}}
        let mut sections=Vec::new();
        {let mut st=c.prepare("SELECT section_key,title,position,required,origin FROM project_sections WHERE project_id=?1 ORDER BY position,section_key")?;for row in st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"required":r.get::<_,i64>(3)?!=0,"origin":r.get::<_,String>(4)?})))?{sections.push(row?);}}

        Ok(json!({
            "project_id":project,
            "contract":crate::workflow_artifacts::editor_contract_json(),
            "approved_artifacts":{"solicitation_profile":solicitation,"research_framework":framework,"aim_set":aims,"literature_search_plan":search_plan,"literature_manifest":literature},
            "members":members,"evidence":evidence,"sources":sources,"citations":citations,"sections":sections
        }))
    }

    fn validate_artifact_source_anchors(c:&Connection,project:&str,value:&Value)->Result<()>{
        match value{
            Value::Array(items)=>for item in items{Self::validate_artifact_source_anchors(c,project,item)?;},
            Value::Object(object)=>{
                if object.contains_key("document_id")&&object.contains_key("document_sha256")&&object.contains_key("start_offset")&&object.contains_key("end_offset")&&object.contains_key("excerpt"){
                    let document_id=object.get("document_id").and_then(Value::as_i64).context("source anchor document_id must be an integer")?;
                    let expected_sha=object.get("document_sha256").and_then(Value::as_str).context("source anchor document_sha256 must be a string")?;
                    let start=object.get("start_offset").and_then(Value::as_u64).context("source anchor start_offset must be a non-negative integer")? as usize;
                    let end=object.get("end_offset").and_then(Value::as_u64).context("source anchor end_offset must be a non-negative integer")? as usize;
                    let excerpt=object.get("excerpt").and_then(Value::as_str).context("source anchor excerpt must be a string")?;
                    let(text,actual_sha):(String,String)=c.query_row("SELECT text,sha256 FROM documents WHERE id=?1 AND project_id=?2",params![document_id,project],|row|Ok((row.get(0)?,row.get(1)?))).context("source anchor document not found in project")?;
                    if actual_sha!=expected_sha{bail!("source anchor document hash does not match the immutable project document");}
                    let bytes=text.as_bytes();
                    if start>=end||end>bytes.len()||bytes.get(start..end)!=Some(excerpt.as_bytes()){
                        bail!("source anchor excerpt is not the exact document byte slice");
                    }
                }
                for child in object.values(){Self::validate_artifact_source_anchors(c,project,child)?;}
            }
            _=>{}
        }
        Ok(())
    }

    fn approved_artifact_body_at(c:&Connection,project:&str,artifact_type:&str,version:i64)->Result<Value>{
        let raw:String=c.query_row("SELECT body_json FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 AND version=?3 AND approved=1",params![project,artifact_type,version],|row|row.get(0))
            .with_context(||format!("approved {artifact_type} version {version} is required"))?;
        serde_json::from_str(&raw).with_context(||format!("stored {artifact_type} version {version} is invalid"))
    }

    fn validate_artifact_dependencies(c:&Connection,project:&str,artifact_type:&str,body:&Value)->Result<()>{
        use crate::workflow_artifacts::{AimSet,LiteratureManifest,LiteratureSearchPlan,OpportunityFitAssessment,ProposalSnapshot,ResearchFramework,SolicitationProfile};
        match artifact_type{
            "solicitation_profile"=>{
                let(total,approved):(i64,i64)=c.query_row("SELECT COUNT(*),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1",[project],|row|Ok((row.get(0)?,row.get(1)?)))?;
                if total==0||total!=approved{bail!("all parsed solicitation requirements must be human-approved before approving the solicitation profile");}
            }
            "research_framework"=>{
                let framework:ResearchFramework=serde_json::from_value(body.clone())?;
                let profile_value=Self::approved_artifact_body_at(c,project,"solicitation_profile",framework.solicitation_profile_version)?;
                let profile:SolicitationProfile=serde_json::from_value(profile_value)?;
                let mapped_requirements:BTreeSet<&str>=framework.nodes.iter().flat_map(|node|node.requirement_ids.iter().map(String::as_str)).collect();
                let mapped_criteria:BTreeSet<&str>=framework.nodes.iter().flat_map(|node|node.review_criterion_ids.iter().map(String::as_str)).collect();
                let valid_requirements:BTreeSet<&str>=profile.requirements.iter().map(|fact|fact.id.as_str()).collect();
                let valid_criteria:BTreeSet<&str>=profile.review_criteria.iter().map(|criterion|criterion.id.as_str()).collect();
                for requirement_id in &mapped_requirements{if !valid_requirements.contains(requirement_id){bail!("research framework references unknown solicitation requirement {requirement_id}");}}
                for criterion_id in &mapped_criteria{if !valid_criteria.contains(criterion_id){bail!("research framework references unknown review criterion {criterion_id}");}}
                for node in &framework.nodes{for user_id in [&node.owner_user_id,&node.approver_user_id]{
                    let active:i64=c.query_row("SELECT COUNT(*) FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 AND pm.user_id=?2 AND u.active=1",params![project,user_id],|row|row.get(0))?;
                    if active!=1{bail!("framework node {} references a user who is not an active project member: {user_id}",node.key);}
                }}
                for requirement in profile.requirements.iter().filter(|fact|fact.mandatory){if !mapped_requirements.contains(requirement.id.as_str()){bail!("mandatory solicitation requirement {} is not mapped to the research framework",requirement.id);}}
                for criterion in &profile.review_criteria{if !mapped_criteria.contains(criterion.id.as_str()){bail!("review criterion {} is not mapped to the research framework",criterion.id);}}
            }
            "aim_set"=>{
                let aims:AimSet=serde_json::from_value(body.clone())?;
                let _=Self::approved_artifact_body_at(c,project,"research_framework",aims.framework_version)?;
                for evidence_id in aims.aims.iter().flat_map(|aim|aim.supporting_evidence_ids.iter()){
                    let exists:i64=c.query_row("SELECT COUNT(*) FROM evidence WHERE id=?1 AND project_id=?2",params![evidence_id,project],|row|row.get(0))?;
                    if exists!=1{bail!("aim set references evidence {evidence_id} outside the project");}
                }
            }
            "literature_search_plan"=>{
                let plan:LiteratureSearchPlan=serde_json::from_value(body.clone())?;
                let profile:SolicitationProfile=serde_json::from_value(Self::approved_artifact_body_at(c,project,"solicitation_profile",plan.solicitation_profile_version)?)?;
                let _=Self::approved_artifact_body_at(c,project,"research_framework",plan.framework_version)?;
                let aims:AimSet=serde_json::from_value(Self::approved_artifact_body_at(c,project,"aim_set",plan.aim_set_version)?)?;
                let valid_aims:BTreeSet<&str>=aims.aims.iter().map(|aim|aim.id.as_str()).collect();
                let valid_requirements:BTreeSet<&str>=profile.requirements.iter().map(|fact|fact.id.as_str()).collect();
                let valid_criteria:BTreeSet<&str>=profile.review_criteria.iter().map(|criterion|criterion.id.as_str()).collect();
                for query in &plan.queries{
                    for id in &query.aim_ids{if !valid_aims.contains(id.as_str()){bail!("literature query {} references unknown aim {id}",query.id);}}
                    for id in &query.requirement_ids{if !valid_requirements.contains(id.as_str()){bail!("literature query {} references unknown solicitation requirement {id}",query.id);}}
                    for id in &query.criterion_ids{if !valid_criteria.contains(id.as_str()){bail!("literature query {} references unknown review criterion {id}",query.id);}}
                }
            }
            "literature_manifest"=>{
                let manifest:LiteratureManifest=serde_json::from_value(body.clone())?;
                let profile:SolicitationProfile=serde_json::from_value(Self::approved_artifact_body_at(c,project,"solicitation_profile",manifest.solicitation_profile_version)?)?;
                let _=Self::approved_artifact_body_at(c,project,"research_framework",manifest.framework_version)?;
                let aims:AimSet=serde_json::from_value(Self::approved_artifact_body_at(c,project,"aim_set",manifest.aim_set_version)?)?;
                let valid_aims:BTreeSet<&str>=aims.aims.iter().map(|aim|aim.id.as_str()).collect();
                let valid_requirements:BTreeSet<&str>=profile.requirements.iter().map(|fact|fact.id.as_str()).collect();
                let valid_criteria:BTreeSet<&str>=profile.review_criteria.iter().map(|criterion|criterion.id.as_str()).collect();
                if let Some(search_plan_version)=manifest.search_plan_version{
                    let plan:LiteratureSearchPlan=serde_json::from_value(Self::approved_artifact_body_at(c,project,"literature_search_plan",search_plan_version)?)?;
                    if plan.queries!=manifest.queries{bail!("literature manifest queries differ from the exact approved search plan");}
                }
                for query in &manifest.queries{
                    for id in &query.aim_ids{if !valid_aims.contains(id.as_str()){bail!("literature query {} references unknown aim {id}",query.id);}}
                    for id in &query.requirement_ids{if !valid_requirements.contains(id.as_str()){bail!("literature query {} references unknown solicitation requirement {id}",query.id);}}
                    for id in &query.criterion_ids{if !valid_criteria.contains(id.as_str()){bail!("literature query {} references unknown review criterion {id}",query.id);}}
                }
                for need in &manifest.evidence_needs{for evidence_id in &need.evidence_ids{
                    let exists:i64=c.query_row("SELECT COUNT(*) FROM evidence WHERE id=?1 AND project_id=?2",params![evidence_id,project],|row|row.get(0))?;
                    if exists!=1{bail!("literature manifest references evidence {evidence_id} outside the project");}
                }}
                for source_id in &manifest.source_ids{let exists:i64=c.query_row("SELECT COUNT(*) FROM research_sources WHERE id=?1 AND project_id=?2",params![source_id,project],|row|row.get(0))?;if exists!=1{bail!("literature manifest references source {source_id} outside the project");}}
                for citation_id in &manifest.citation_ids{let exists:i64=c.query_row("SELECT COUNT(*) FROM citations WHERE id=?1 AND project_id=?2",params![citation_id,project],|row|row.get(0))?;if exists!=1{bail!("literature manifest references citation {citation_id} outside the project");}}
            }
            "proposal_snapshot"=>{
                let snapshot:ProposalSnapshot=serde_json::from_value(body.clone())?;
                let _=Self::approved_artifact_body_at(c,project,"solicitation_profile",snapshot.solicitation_profile_version)?;
                let _=Self::approved_artifact_body_at(c,project,"research_framework",snapshot.framework_version)?;
                let _=Self::approved_artifact_body_at(c,project,"aim_set",snapshot.aim_set_version)?;
                let _=Self::approved_artifact_body_at(c,project,"literature_manifest",snapshot.literature_manifest_version)?;
                let(config_version,definition_version,definition_sha):(i64,i64,Option<String>)=c.query_row("SELECT config_version,definition_version,definition_sha256 FROM project_workflows WHERE project_id=?1",[project],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
                if snapshot.workflow_config_version!=config_version||snapshot.workflow_definition_version as i64!=definition_version||Some(snapshot.workflow_definition_sha256.as_str())!=definition_sha.as_deref(){bail!("proposal snapshot workflow definition or configuration is stale");}
                for section in &snapshot.sections{
                    let body_text:String=c.query_row("SELECT body FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3 AND approved=1",params![section.version_id,project,section.section_key],|row|row.get(0)).with_context(||format!("approved proposal section version {} is missing",section.version_id))?;
                    if sha256_hex(body_text.as_bytes())!=section.content_sha256{bail!("proposal section {} content hash does not match approved version {}",section.section_key,section.version_id);}
                }
            }
            "opportunity_fit"=>{
                let assessment:OpportunityFitAssessment=serde_json::from_value(body.clone())?;
                let _=Self::approved_artifact_body_at(c,project,"solicitation_profile",assessment.solicitation_profile_version)?;
            }
            "collaboration_record"=>{
                let routing:crate::workflow_artifacts::CollaborationRouting=serde_json::from_value(body.clone())?;
                let mut users=vec![routing.project_owner_user_id];
                for route in routing.routes{users.push(route.owner_user_id);users.extend(route.approver_user_ids);}
                users.sort();users.dedup();
                for user_id in users{
                    let active:i64=c.query_row("SELECT COUNT(*) FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 AND pm.user_id=?2 AND u.active=1",params![project,user_id],|row|row.get(0))?;
                    if active!=1{bail!("approval routing references a user who is not an active project member: {user_id}");}
                }
            }
            _=>{}
        }
        Ok(())
    }

    fn synchronize_framework_sections(
        tx: &Transaction<'_>,
        project: &str,
        framework: &crate::workflow_artifacts::ResearchFramework,
    ) -> Result<Value> {
        let selected = framework
            .nodes
            .iter()
            .map(|node| node.key.as_str())
            .collect::<BTreeSet<_>>();
        let mut removed = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT section_key FROM project_sections WHERE project_id=?1 AND origin IN ('configured','framework') ORDER BY position,section_key",
            )?;
            let rows = statement.query_map([project], |row| row.get::<_, String>(0))?;
            for row in rows {
                let key = row?;
                if !selected.contains(key.as_str()) {
                    removed.push(key);
                }
            }
        }

        // Positions are unique per project. Move the current catalog out of the
        // target range before applying the newly approved framework order.
        tx.execute(
            "UPDATE project_sections SET position=position+1000000 WHERE project_id=?1",
            [project],
        )?;
        for key in &removed {
            tx.execute(
                "DELETE FROM project_sections WHERE project_id=?1 AND section_key=?2",
                params![project, key],
            )?;
        }

        let mut nodes = framework.nodes.iter().collect::<Vec<_>>();
        nodes.sort_by_key(|node| node.position);
        for (position, node) in nodes.iter().enumerate() {
            tx.execute(
                r#"INSERT INTO project_sections(project_id,section_key,title,position,required,origin)
                   VALUES(?1,?2,?3,?4,1,'framework')
                   ON CONFLICT(project_id,section_key) DO UPDATE SET
                     title=excluded.title,position=excluded.position,required=1,origin='framework'"#,
                params![project, node.key, node.title.trim(), position as i64],
            )?;
        }

        // Sponsor-required attachment/section rows may have been compiled before
        // the framework. Preserve them and place them after the narrative outline.
        let framework_count = nodes.len() as i64;
        let mut preserved = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT section_key FROM project_sections WHERE project_id=?1 AND position>=1000000 ORDER BY position,section_key",
            )?;
            let rows = statement.query_map([project], |row| row.get::<_, String>(0))?;
            for row in rows {
                preserved.push(row?);
            }
        }
        for (offset, key) in preserved.iter().enumerate() {
            tx.execute(
                "UPDATE project_sections SET position=?1 WHERE project_id=?2 AND section_key=?3",
                params![framework_count + offset as i64, project, key],
            )?;
        }

        Ok(json!({
            "framework_sections": nodes.iter().map(|node| node.key.as_str()).collect::<Vec<_>>(),
            "removed_from_active_catalog": removed,
            "preserved_non_framework_sections": preserved
        }))
    }

    fn invalidate_section_approvals(
        tx: &Transaction<'_>,
        project: &str,
        upstream_artifact_type: &str,
        upstream_version: i64,
    ) -> Result<Option<Value>> {
        let mut version_ids = Vec::new();
        {
            let mut statement = tx.prepare(
                "SELECT id FROM section_versions WHERE project_id=?1 AND approved=1 ORDER BY id",
            )?;
            let rows = statement.query_map([project], |row| row.get::<_, i64>(0))?;
            for row in rows {
                version_ids.push(row?);
            }
        }
        if version_ids.is_empty() {
            return Ok(None);
        }

        tx.execute(
            "UPDATE section_versions SET approved=0 WHERE project_id=?1 AND approved=1",
            [project],
        )?;
        Ok(Some(json!({
            "upstream_artifact_type": upstream_artifact_type,
            "upstream_version": upstream_version,
            "invalidated_section_version_ids": version_ids,
            "approval_records_preserved": true
        })))
    }

    fn current_approved_artifact_version(&self, project: &str, artifact_type: &str) -> Result<Option<i64>> {
        let c = self.conn()?;
        let row = c
            .query_row(
                "SELECT version,approved FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 ORDER BY version DESC LIMIT 1",
                params![project, artifact_type],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()?;
        Ok(row.and_then(|(version, approved)| approved.then_some(version)))
    }

    pub fn workflow_artifact_is_fresh(&self, project: &str, artifact_type: &str) -> Result<bool> {
        let artifact = self.workflow_artifact_json(project, artifact_type)?;
        if !artifact.get("approved").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(false);
        }
        let body = artifact.get("body").cloned().unwrap_or(Value::Null);
        let fresh = match artifact_type {
            "research_framework" => {
                let framework: crate::workflow_artifacts::ResearchFramework = serde_json::from_value(body)?;
                self.current_approved_artifact_version(project, "solicitation_profile")?
                    == Some(framework.solicitation_profile_version)
            }
            "aim_set" => {
                let aims: crate::workflow_artifacts::AimSet = serde_json::from_value(body)?;
                self.current_approved_artifact_version(project, "research_framework")?
                    == Some(aims.framework_version)
            }
            "literature_search_plan" => {
                let plan:crate::workflow_artifacts::LiteratureSearchPlan=serde_json::from_value(body)?;
                self.current_approved_artifact_version(project,"solicitation_profile")?==Some(plan.solicitation_profile_version)
                    && self.current_approved_artifact_version(project,"research_framework")?==Some(plan.framework_version)
                    && self.current_approved_artifact_version(project,"aim_set")?==Some(plan.aim_set_version)
            }
            "literature_manifest" => {
                let manifest: crate::workflow_artifacts::LiteratureManifest = serde_json::from_value(body)?;
                let upstream_fresh=self.current_approved_artifact_version(project, "solicitation_profile")?
                    == Some(manifest.solicitation_profile_version)
                    && self.current_approved_artifact_version(project, "research_framework")?
                        == Some(manifest.framework_version)
                    && self.current_approved_artifact_version(project, "aim_set")? == Some(manifest.aim_set_version);
                upstream_fresh && match manifest.search_plan_version{
                    Some(version)=>self.current_approved_artifact_version(project,"literature_search_plan")?==Some(version)
                        && self.workflow_artifact_is_fresh(project,"literature_search_plan")?,
                    None=>manifest.schema_version==1,
                }
            }
            "opportunity_fit" => {
                let assessment: crate::workflow_artifacts::OpportunityFitAssessment =
                    serde_json::from_value(body)?;
                self.current_approved_artifact_version(project, "solicitation_profile")?
                    == Some(assessment.solicitation_profile_version)
            }
            _ => true,
        };
        Ok(fresh)
    }

    pub fn save_workflow_artifact(
        &self,
        project: &str,
        artifact_type: &str,
        body: &Value,
        source: &str,
        author: Option<&str>,
        expected_version: Option<i64>,
    ) -> Result<Value> {
        let config = self.workflow_config(project)?;
        let core_artifact = self
            .workflow_registry
            .core_steps
            .iter()
            .any(|step| step.artifact_type.as_deref() == Some(artifact_type));
        let enabled_artifact = self
            .workflow_registry
            .optional_modules
            .iter()
            .any(|module| {
                config.enabled(&module.step.key)
                    && module.step.artifact_type.as_deref() == Some(artifact_type)
            });
        let auxiliary_core_artifact=artifact_type=="literature_search_plan";
        if !core_artifact && !enabled_artifact && !auxiliary_core_artifact {
            bail!("workflow artifact type is not enabled for this project: {artifact_type}");
        }
        crate::workflow_artifacts::validate_artifact_document(artifact_type, body, false)?;
        let raw = serde_json::to_string(body)?;
        let sha = sha256_hex(raw.as_bytes());
        let mut c = self.conn()?;
        Self::validate_artifact_source_anchors(&c,project,body)?;
        let tx = c.transaction()?;
        let current: Option<i64> = tx.query_row(
            "SELECT MAX(version) FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2",
            params![project, artifact_type],
            |r| r.get(0),
        )?;
        let current = current.unwrap_or(0);
        if let Some(expected) = expected_version {
            if expected != current {
                bail!("workflow artifact changed since editing began: expected version {expected}, found {current}");
            }
        }
        let version = current + 1;
        tx.execute("INSERT INTO workflow_artifacts(project_id,artifact_type,version,body_json,content_sha256,source,author) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![project,artifact_type,version,raw,sha,source,author])?;
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'workflow_artifact_saved',?2,?3)",params![project,author,serde_json::to_string(&json!({"artifact_type":artifact_type,"version":version,"sha256":sha}))?])?;
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        self.workflow_artifact_json(project, artifact_type)
    }

    pub fn begin_research_run(
        &self,
        project: &str,
        search_plan_version: i64,
        search_provider: &str,
        actor: &str,
        started_at: &str,
    ) -> Result<String> {
        if actor.trim().is_empty() || search_provider.trim().is_empty() {
            bail!("research run requires an authenticated actor and search provider");
        }
        let plan = self.workflow_artifact_json(project, "literature_search_plan")?;
        if plan.get("version").and_then(Value::as_i64) != Some(search_plan_version)
            || !plan.get("approved").and_then(Value::as_bool).unwrap_or(false)
            || !self.workflow_artifact_is_fresh(project, "literature_search_plan")?
        {
            bail!("the selected literature search plan is missing, unapproved, or stale");
        }
        let manifest = json!({
            "search_plan_version": search_plan_version,
            "search_plan_sha256": plan.get("sha256"),
            "search_plan": plan.get("body"),
            "workflow": self.workflow_config_record_json(project)?
        });
        let manifest_json = serde_json::to_string(&manifest)?;
        let manifest_sha = sha256_hex(manifest_json.as_bytes());
        let run_id = Uuid::new_v4().to_string();
        self.conn()?.execute(
            "INSERT INTO research_runs(id,project_id,search_plan_version,input_manifest_json,input_manifest_sha256,search_provider,status,started_by_user_id,started_at) VALUES(?1,?2,?3,?4,?5,?6,'running',?7,?8)",
            params![run_id,project,search_plan_version,manifest_json,manifest_sha,search_provider,actor,started_at],
        ).context("another literature research run is already active for this project")?;
        Ok(run_id)
    }

    pub fn fail_research_run(&self, project: &str, run_id: &str, failures: &[String], completed_at: &str) -> Result<()> {
        let failure_json = serde_json::to_string(failures)?;
        let changed = self.conn()?.execute(
            "UPDATE research_runs SET status='failed',failure_json=?1,completed_at=?2 WHERE id=?3 AND project_id=?4 AND status='running'",
            params![failure_json,completed_at,run_id,project],
        )?;
        if changed != 1 { bail!("active research run not found"); }
        Ok(())
    }

    pub fn finalize_research_run_atomic(&self, project: &str, run: &StagedResearchRun) -> Result<Value> {
        if run.queries.is_empty() { bail!("research run has no staged queries"); }
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (status, stored_plan_version):(String,i64)=tx.query_row(
            "SELECT status,search_plan_version FROM research_runs WHERE id=?1 AND project_id=?2",
            params![run.id,project],|row|Ok((row.get(0)?,row.get(1)?)),
        ).context("research run not found")?;
        if status != "running" || stored_plan_version != run.search_plan_version {
            bail!("research run is not active for the approved search plan");
        }
        let plan_raw:String=tx.query_row(
            "SELECT body_json FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='literature_search_plan' AND version=?2 AND approved=1",
            params![project,run.search_plan_version],|row|row.get(0),
        ).context("approved search plan disappeared before research commit")?;
        let plan:crate::workflow_artifacts::LiteratureSearchPlan=serde_json::from_str(&plan_raw)?;
        if plan.solicitation_profile_version!=run.solicitation_profile_version
            || plan.framework_version!=run.framework_version
            || plan.aim_set_version!=run.aim_set_version {
            bail!("staged research inputs do not match the approved search plan");
        }
        let latest_plan_version:i64=tx.query_row(
            "SELECT COALESCE(MAX(version),0) FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='literature_search_plan'",
            [project],|row|row.get(0),
        )?;
        if latest_plan_version!=run.search_plan_version{bail!("the literature search plan changed while research was running");}
        for (artifact_type,expected_version) in [
            ("solicitation_profile",run.solicitation_profile_version),
            ("research_framework",run.framework_version),
            ("aim_set",run.aim_set_version),
        ]{
            let current:Option<(i64,bool)>=tx.query_row(
                "SELECT version,approved!=0 FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 ORDER BY version DESC LIMIT 1",
                params![project,artifact_type],|row|Ok((row.get(0)?,row.get(1)?)),
            ).optional()?;
            if current!=Some((expected_version,true)){bail!("{artifact_type} changed or lost approval while research was running");}
        }
        let planned:std::collections::BTreeMap<&str,&crate::workflow_artifacts::LiteratureQueryRecord>=
            plan.queries.iter().map(|query|(query.id.as_str(),query)).collect();
        if planned.len()!=run.queries.len(){bail!("staged research query count does not match the approved search plan");}

        let mut query_manifest=Vec::new();
        let mut evidence_needs=Vec::new();
        let mut source_ids=BTreeSet::new();
        let mut citation_ids=BTreeSet::new();
        let mut contradictions=Vec::new();
        let mut seen_queries=BTreeSet::new();
        let mut sources_saved=0usize;
        for staged in &run.queries {
            let Some(approved_query)=planned.get(staged.query.id.as_str()) else{bail!("staged query {} is not in the approved search plan",staged.query.id);};
            if *approved_query!=&staged.query{bail!("staged query {} differs from the approved search plan",staged.query.id);}
            if !seen_queries.insert(staged.query.id.as_str()){bail!("duplicate staged query {}",staged.query.id);}
            if !matches!(staged.terminal_status.as_str(),"complete"|"complete_no_sources"|"failed"){
                bail!("unsupported terminal research query status: {}",staged.terminal_status);
            }
            let primary_requirement=staged.query.requirement_ids.first().context("approved research query has no requirement")?;
            tx.execute(
                "INSERT INTO research_queries(project_id,requirement_external_id,query,preferred_domains_json,rationale,status,run_id,plan_query_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![project,primary_requirement,staged.query.query,serde_json::to_string(&staged.query.preferred_domains)?,staged.query.rationale,staged.terminal_status,run.id,staged.query.id],
            )?;
            let query_id=tx.last_insert_rowid();
            for requirement_id in &staged.query.requirement_ids{
                tx.execute("INSERT INTO research_query_requirements(query_id,requirement_external_id) VALUES(?1,?2)",params![query_id,requirement_id])?;
            }
            let mut supported_evidence_ids=Vec::new();
            for assessed in &staged.sources {
                if !matches!(assessed.validation_status.as_str(),"supported"|"partially_supported"|"contradicted"|"irrelevant"){
                    bail!("unsupported source validation status: {}",assessed.validation_status);
                }
                tx.execute(
                    "INSERT OR IGNORE INTO research_sources(project_id,query_id,title,url,text,retrieved_at,content_sha256,http_status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![project,query_id,assessed.source.title,assessed.source.url,assessed.source.text,assessed.source.retrieved_at,assessed.source.sha256,assessed.source.status],
                )?;
                let source_id:i64=tx.query_row("SELECT id FROM research_sources WHERE project_id=?1 AND url=?2 AND content_sha256=?3",params![project,assessed.source.url,assessed.source.sha256],|row|row.get(0))?;
                tx.execute(
                    "INSERT INTO research_query_sources(query_id,source_id,validation_status,confidence,supporting_excerpt,explanation) VALUES(?1,?2,?3,?4,?5,?6)",
                    params![query_id,source_id,assessed.validation_status,assessed.confidence.clamp(0.0,1.0),assessed.supporting_excerpt,assessed.explanation],
                )?;
                source_ids.insert(source_id);sources_saved+=1;
                if assessed.validation_status=="irrelevant"{continue;}
                let exact=!assessed.supporting_excerpt.trim().is_empty()&&assessed.source.text.contains(&assessed.supporting_excerpt);
                let passage=if exact{assessed.supporting_excerpt.clone()}else{assessed.source.text.chars().take(1800).collect::<String>()};
                let evidence_status=if exact{assessed.validation_status.as_str()}else{"candidate"};
                tx.execute(
                    "INSERT INTO evidence(project_id,requirement_external_id,source_type,source_ref,claim,passage,source_url,confidence,status) VALUES(?1,?2,'external_research',?3,?4,?5,?6,?7,?8)",
                    params![project,primary_requirement,format!("research_source:{source_id}"),staged.query.rationale,passage,assessed.source.url,assessed.confidence.clamp(0.0,1.0),evidence_status],
                )?;
                let evidence_id=tx.last_insert_rowid();
                if matches!(evidence_status,"supported"|"partially_supported"){supported_evidence_ids.push(evidence_id);}
                if exact&&assessed.validation_status=="contradicted"{contradictions.push(format!("{}: {}",staged.query.rationale,assessed.explanation));}
                tx.execute(
                    "INSERT INTO citations(project_id,evidence_id,citation_key,title,url,passage,content_sha256,verified) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![project,evidence_id,format!("SRC-{source_id}"),assessed.source.title,assessed.source.url,passage,assessed.source.sha256,exact as i64],
                )?;
                citation_ids.insert(tx.last_insert_rowid());
            }
            let disposition=if supported_evidence_ids.is_empty(){"unresolved_risk"}else{"supported"};
            evidence_needs.push(json!({"evidence_need_id":staged.query.id,"disposition":disposition,"evidence_ids":supported_evidence_ids,"rationale":staged.query.rationale}));
            query_manifest.push(serde_json::to_value(&staged.query)?);
        }
        if seen_queries.len()!=planned.len(){bail!("not every approved search-plan query was staged");}
        let manifest=json!({
            "schema_version":2,"run_id":run.id,"search_plan_version":run.search_plan_version,
            "solicitation_profile_version":run.solicitation_profile_version,"framework_version":run.framework_version,
            "aim_set_version":run.aim_set_version,"started_at":run.started_at,"completed_at":run.completed_at,
            "search_provider":run.search_provider,"queries":query_manifest,"evidence_needs":evidence_needs,
            "source_ids":source_ids,"citation_ids":citation_ids,"contradictions":contradictions
        });
        crate::workflow_artifacts::validate_artifact_document("literature_manifest",&manifest,false)?;
        Self::validate_artifact_dependencies(&tx,project,"literature_manifest",&manifest)?;
        let raw=serde_json::to_string(&manifest)?;let sha=sha256_hex(raw.as_bytes());
        let version:i64=tx.query_row("SELECT COALESCE(MAX(version),0)+1 FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='literature_manifest'",[project],|row|row.get(0))?;
        tx.execute("INSERT INTO workflow_artifacts(project_id,artifact_type,version,body_json,content_sha256,source) VALUES(?1,'literature_manifest',?2,?3,?4,'atomic_research_pipeline')",params![project,version,raw,sha])?;
        tx.execute("UPDATE research_runs SET status='complete',failure_json=?1,completed_at=?2,manifest_artifact_version=?3,manifest_sha256=?4 WHERE id=?5 AND project_id=?6 AND status='running'",params![serde_json::to_string(&run.failures)?,run.completed_at,version,sha,run.id,project])?;
        tx.execute("INSERT INTO workflow_events(project_id,event_type,payload_json) VALUES(?1,'research_run_finalized',?2)",params![project,serde_json::to_string(&json!({"run_id":run.id,"search_plan_version":run.search_plan_version,"artifact_version":version,"artifact_sha256":sha,"query_count":run.queries.len(),"source_count":sources_saved,"isolated_failures":run.failures.len()}))?])?;
        Self::touch_project_conn(&tx,project)?;tx.commit()?;
        let mut artifact=self.workflow_artifact_json(project,"literature_manifest")?;
        artifact["sources_saved"]=json!(sources_saved);
        Ok(artifact)
    }

    pub fn approve_workflow_artifact(
        &self,
        project: &str,
        artifact_type: &str,
        version: i64,
        approver: Option<&str>,
    ) -> Result<Value> {
        let config = self.workflow_config(project)?;
        let enabled = artifact_type=="literature_search_plan" || self
            .workflow_registry
            .core_steps
            .iter()
            .any(|step| step.artifact_type.as_deref() == Some(artifact_type))
            || self
                .workflow_registry
                .optional_modules
                .iter()
                .any(|module| {
                    config.enabled(&module.step.key)
                        && module.step.artifact_type.as_deref() == Some(artifact_type)
                });
        if !enabled {
            bail!("workflow artifact type is not enabled for this project: {artifact_type}");
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let latest:i64=tx.query_row("SELECT MAX(version) FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2",params![project,artifact_type],|r|r.get::<_,Option<i64>>(0))?.context("workflow artifact does not exist")?;
        if latest != version {
            bail!("only the latest workflow artifact version can be approved; latest is {latest}");
        }
        let body_raw:String=tx.query_row("SELECT body_json FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 AND version=?3",params![project,artifact_type,version],|r|r.get(0)).context("workflow artifact version not found")?;
        let body: Value =
            serde_json::from_str(&body_raw).context("stored workflow artifact is invalid")?;
        crate::workflow_artifacts::validate_artifact_document(artifact_type, &body, true)?;
        Self::validate_artifact_source_anchors(&tx,project,&body)?;
        Self::validate_artifact_dependencies(&tx,project,artifact_type,&body)?;
        if let Some(user_id)=approver {
            let role:String=tx.query_row(
                "SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",
                params![project,user_id],|row|row.get(0),
            ).context("artifact approver is not a project member")?;
            tx.execute(
                "INSERT INTO artifact_approval_events(project_id,artifact_type,artifact_version,actor_user_id,role_at_decision,decision) VALUES(?1,?2,?3,?4,?5,'approved')",
                params![project,artifact_type,version,user_id,role],
            )?;
        }
        let approval_progress = if artifact_type != "collaboration_record" {
            Self::record_configured_artifact_approval(&tx,project,artifact_type,version,approver)?
        } else {
            None
        };
        if let Some(progress)=&approval_progress {
            if !progress.get("threshold_met").and_then(Value::as_bool).unwrap_or(false) {
                tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'workflow_artifact_approval_recorded',?2,?3)",params![project,approver,serde_json::to_string(progress)?])?;
                Self::touch_project_conn(&tx,project)?;
                tx.commit()?;
                let mut artifact=self.workflow_artifact_json(project,artifact_type)?;
                artifact["approval_progress"]=progress.clone();
                return Ok(artifact);
            }
        }
        let section_sync = if artifact_type == "research_framework" {
            let framework: crate::workflow_artifacts::ResearchFramework =
                serde_json::from_value(body.clone())?;
            Some(Self::synchronize_framework_sections(&tx, project, &framework)?)
        } else {
            None
        };
        let section_approval_invalidation = if matches!(
            artifact_type,
            "solicitation_profile" | "research_framework" | "aim_set" | "literature_search_plan" | "literature_manifest"
        ) {
            Self::invalidate_section_approvals(&tx, project, artifact_type, version)?
        } else {
            None
        };
        let n=tx.execute("UPDATE workflow_artifacts SET approved=1,approved_by=?1,approved_at=CURRENT_TIMESTAMP WHERE project_id=?2 AND artifact_type=?3 AND version=?4",params![approver,project,artifact_type,version])?;
        if n != 1 {
            bail!("workflow artifact version not found");
        }
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'workflow_artifact_approved',?2,?3)",params![project,approver,serde_json::to_string(&json!({"artifact_type":artifact_type,"version":version}))?])?;
        if let Some(section_sync) = section_sync {
            tx.execute(
                "INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'framework_sections_synchronized',?2,?3)",
                params![project, approver, serde_json::to_string(&section_sync)?],
            )?;
        }
        if let Some(invalidation) = section_approval_invalidation {
            tx.execute(
                "INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_approvals_invalidated',?2,?3)",
                params![project, approver, serde_json::to_string(&invalidation)?],
            )?;
        }
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        let mut artifact=self.workflow_artifact_json(project, artifact_type)?;
        if let Some(progress)=approval_progress{artifact["approval_progress"]=progress;}
        Ok(artifact)
    }

    pub fn return_workflow_artifact_for_revision(
        &self,
        project:&str,
        artifact_type:&str,
        version:i64,
        actor:&str,
        rationale:&str,
    )->Result<Value>{
        let rationale=rationale.trim();
        if rationale.is_empty(){bail!("a revision rationale is required");}
        if rationale.chars().count()>4000{bail!("revision rationale exceeds 4000 characters");}
        let config=self.workflow_config(project)?;
        let enabled=artifact_type=="literature_search_plan"
            || self.workflow_registry.core_steps.iter().any(|step|step.artifact_type.as_deref()==Some(artifact_type))
            || self.workflow_registry.optional_modules.iter().any(|module|config.enabled(&module.step.key)&&module.step.artifact_type.as_deref()==Some(artifact_type));
        if !enabled{bail!("workflow artifact type is not enabled for this project: {artifact_type}");}
        let mut c=self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let role:String=tx.query_row(
            "SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",
            params![project,actor],|row|row.get(0),
        ).context("revision actor is not a project member")?;
        let latest:Option<(i64,bool)>=tx.query_row(
            "SELECT version,approved!=0 FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 ORDER BY version DESC LIMIT 1",
            params![project,artifact_type],|row|Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        let Some((latest_version,approved))=latest else{bail!("workflow artifact does not exist");};
        if latest_version!=version{bail!("only the latest workflow artifact version can be returned; latest is {latest_version}");}
        let pending_approvals:i64=tx.query_row(
            "SELECT COUNT(*) FROM artifact_approval_decisions WHERE project_id=?1 AND artifact_type=?2 AND artifact_version=?3 AND decision='approved'",
            params![project,artifact_type,version],|row|row.get(0),
        )?;
        if !approved&&pending_approvals==0{bail!("the selected artifact version is neither approved nor awaiting configured approvals");}
        tx.execute(
            "UPDATE workflow_artifacts SET approved=0 WHERE project_id=?1 AND artifact_type=?2 AND version=?3",
            params![project,artifact_type,version],
        )?;
        tx.execute(
            "UPDATE artifact_approval_decisions SET decision='rejected',notes=?1,created_at=CURRENT_TIMESTAMP WHERE project_id=?2 AND artifact_type=?3 AND artifact_version=?4",
            params![rationale,project,artifact_type,version],
        )?;
        tx.execute(
            "INSERT INTO artifact_approval_events(project_id,artifact_type,artifact_version,actor_user_id,role_at_decision,decision,notes) VALUES(?1,?2,?3,?4,?5,'rejected',?6)",
            params![project,artifact_type,version,actor,role,rationale],
        )?;
        let section_invalidation=if matches!(artifact_type,"solicitation_profile"|"research_framework"|"aim_set"|"literature_search_plan"|"literature_manifest"){
            Self::invalidate_section_approvals(&tx,project,artifact_type,version)?
        }else{None};
        let event=json!({
            "artifact_type":artifact_type,"version":version,"decision":"returned_for_revision",
            "rationale":rationale,"role_at_decision":role,"prior_approval_votes":pending_approvals,
            "section_approvals_invalidated":section_invalidation,"history_preserved":true
        });
        tx.execute(
            "INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'workflow_artifact_returned_for_revision',?2,?3)",
            params![project,actor,serde_json::to_string(&event)?],
        )?;
        Self::touch_project_conn(&tx,project)?;
        tx.commit()?;
        let mut artifact=self.workflow_artifact_json(project,artifact_type)?;
        artifact["return_decision"]=event;
        Ok(artifact)
    }

    fn record_configured_artifact_approval(
        tx:&Transaction<'_>,
        project:&str,
        artifact_type:&str,
        version:i64,
        approver:Option<&str>,
    )->Result<Option<Value>>{
        let routing_raw:Option<String>=tx.query_row(
            "SELECT body_json FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='collaboration_record' AND approved=1 ORDER BY version DESC LIMIT 1",
            [project],|row|row.get(0)).optional()?;
        let Some(routing_raw)=routing_raw else{return Ok(None);};
        let routing:crate::workflow_artifacts::CollaborationRouting=serde_json::from_str(&routing_raw)?;
        let Some(route)=routing.routes.iter().find(|route|
            route.artifact_type==artifact_type || (artifact_type.starts_with("section:")&&route.artifact_type=="proposal_section")
        ) else{return Ok(None);};
        let approver=approver.context("configured approval routing requires an authenticated approver")?;
        if !route.approver_user_ids.iter().any(|user_id|user_id==approver){bail!("the authenticated user is not an approver configured for {artifact_type}");}
        let role:String=tx.query_row("SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,approver],|row|row.get(0)).context("configured approver is not a project member")?;
        tx.execute("INSERT INTO artifact_approval_decisions(project_id,artifact_type,artifact_version,approver_user_id,role_at_approval,decision) VALUES(?1,?2,?3,?4,?5,'approved') ON CONFLICT(project_id,artifact_type,artifact_version,approver_user_id) DO UPDATE SET role_at_approval=excluded.role_at_approval,decision='approved',created_at=CURRENT_TIMESTAMP",params![project,artifact_type,version,approver,role])?;
        let approvals:i64=tx.query_row("SELECT COUNT(*) FROM artifact_approval_decisions WHERE project_id=?1 AND artifact_type=?2 AND artifact_version=?3 AND decision='approved'",params![project,artifact_type,version],|row|row.get(0))?;
        Ok(Some(json!({"artifact_type":artifact_type,"artifact_version":version,"approvals":approvals,"minimum_approvals":route.minimum_approvals,"threshold_met":approvals>=route.minimum_approvals as i64})))
    }

    pub fn list_projects_json(&self,include_archived:bool) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,title,sponsor,mechanism,created_at,COALESCE(updated_at,created_at),archived_at FROM projects WHERE (?1=1 OR archived_at IS NULL) ORDER BY archived_at IS NOT NULL,COALESCE(updated_at,created_at) DESC LIMIT 250")?;
        let rows=st.query_map([include_archived as i64],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,"mechanism":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,"updated_at":r.get::<_,String>(5)?,"archived_at":r.get::<_,Option<String>>(6)?})))?;
        let mut out = Vec::new();
        for row in rows {
            let mut value=row?;
            let id=value.get("id").and_then(Value::as_str).unwrap_or_default();
            value["stage"]=json!(self.compatibility_stage(id)?);
            out.push(value);
        }
        Ok(json!(out))
    }

    pub fn upsert_identity(&self,user_id:&str,organization_id:&str,email:Option<&str>,display_name:&str)->Result<()> {
        if user_id.trim().is_empty()||organization_id.trim().is_empty()||display_name.trim().is_empty(){bail!("authenticated identity fields cannot be empty");}
        let c=self.conn()?;
        c.execute("INSERT INTO organizations(id,name) VALUES(?1,?2) ON CONFLICT(id) DO NOTHING",params![organization_id,organization_id])?;
        c.execute(r#"INSERT INTO users(id,organization_id,email,display_name) VALUES(?1,?2,?3,?4)
          ON CONFLICT(id) DO UPDATE SET organization_id=excluded.organization_id,email=excluded.email,display_name=excluded.display_name,active=1,last_seen_at=CURRENT_TIMESTAMP"#,params![user_id,organization_id,email,display_name])?;
        Ok(())
    }

    pub fn internal_bootstrap_complete(&self)->Result<bool>{
        Ok(self.conn()?.query_row("SELECT EXISTS(SELECT 1 FROM internal_auth_bootstrap WHERE singleton=1)",[],|row|row.get::<_,i64>(0))?!=0)
    }

    pub fn bootstrap_internal_admin(&self,organization_id:&str,organization_name:&str,username:&str,email:&str,display_name:&str,password_hash:&str)->Result<InternalAccountRecord>{
        let mut c=self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let complete:i64=tx.query_row("SELECT EXISTS(SELECT 1 FROM internal_auth_bootstrap WHERE singleton=1)",[],|row|row.get(0))?;
        if complete!=0{bail!("initial account setup is already complete");}
        let internal_count:i64=tx.query_row("SELECT COUNT(*) FROM users WHERE password_hash IS NOT NULL",[],|row|row.get(0))?;
        if internal_count!=0{bail!("internal accounts already exist; bootstrap cannot run");}
        let id=Uuid::new_v4().to_string();
        tx.execute("INSERT INTO organizations(id,name) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET name=excluded.name",params![organization_id,organization_name])?;
        tx.execute(r#"INSERT INTO users(id,organization_id,email,display_name,active,username,password_hash,system_role,must_change_password)
          VALUES(?1,?2,?3,?4,1,?5,?6,'system_admin',1)"#,params![id,organization_id,email,display_name,username,password_hash])?;
        tx.execute("INSERT INTO internal_auth_bootstrap(singleton,admin_user_id) VALUES(1,?1)",[&id])?;
        tx.execute("INSERT INTO account_audit_events(actor_user_id,target_user_id,event_type,detail_json) VALUES(?1,?1,'bootstrap_admin_created',?2)",params![id,json!({"username":username,"email":email}).to_string()])?;
        tx.commit()?;
        self.internal_account_by_id(&id)?.context("bootstrap administrator was not persisted")
    }

    pub fn internal_account_by_login(&self,login:&str)->Result<Option<InternalAccountRecord>>{
        let c=self.conn()?;
        c.query_row(r#"SELECT id,organization_id,username,email,display_name,password_hash,system_role,must_change_password,active,
          CASE WHEN locked_until IS NOT NULL AND datetime(locked_until)>CURRENT_TIMESTAMP THEN 1 ELSE 0 END
          FROM users WHERE password_hash IS NOT NULL AND (username=?1 COLLATE NOCASE OR email=?1 COLLATE NOCASE) LIMIT 1"#,[login],Self::read_internal_account).optional().map_err(Into::into)
    }

    pub fn internal_account_by_id(&self,user_id:&str)->Result<Option<InternalAccountRecord>>{
        let c=self.conn()?;
        c.query_row(r#"SELECT id,organization_id,username,email,display_name,password_hash,system_role,must_change_password,active,
          CASE WHEN locked_until IS NOT NULL AND datetime(locked_until)>CURRENT_TIMESTAMP THEN 1 ELSE 0 END
          FROM users WHERE id=?1 AND password_hash IS NOT NULL LIMIT 1"#,[user_id],Self::read_internal_account).optional().map_err(Into::into)
    }

    fn read_internal_account(row:&rusqlite::Row<'_>)->rusqlite::Result<InternalAccountRecord>{
        Ok(InternalAccountRecord{id:row.get(0)?,organization_id:row.get(1)?,username:row.get(2)?,email:row.get(3)?,display_name:row.get(4)?,password_hash:row.get(5)?,system_role:row.get(6)?,must_change_password:row.get::<_,i64>(7)?!=0,active:row.get::<_,i64>(8)?!=0,locked:row.get::<_,i64>(9)?!=0})
    }

    pub fn record_login_failure(&self,user_id:&str,max_failures:u32,lock_seconds:u64)->Result<()> {
        let c=self.conn()?;
        c.execute(r#"UPDATE users SET failed_login_count=failed_login_count+1,
          locked_until=CASE WHEN failed_login_count+1>=?2 THEN datetime('now','+'||?3||' seconds') ELSE locked_until END
          WHERE id=?1"#,params![user_id,max_failures,lock_seconds])?;
        c.execute("INSERT INTO account_audit_events(target_user_id,event_type,detail_json) VALUES(?1,'login_failed',?2)",params![user_id,json!({"lock_threshold":max_failures}).to_string()])?;
        Ok(())
    }

    pub fn record_login_success(&self,user_id:&str)->Result<()> {
        let c=self.conn()?;
        c.execute("UPDATE users SET failed_login_count=0,locked_until=NULL,last_seen_at=CURRENT_TIMESTAMP WHERE id=?1",[user_id])?;
        c.execute("INSERT INTO account_audit_events(actor_user_id,target_user_id,event_type) VALUES(?1,?1,'login_succeeded')",[user_id])?;
        Ok(())
    }

    pub fn create_auth_session(&self,user_id:&str,token_sha256:&str,ttl_seconds:u64)->Result<String>{
        let c=self.conn()?;
        c.execute("DELETE FROM auth_sessions WHERE revoked_at IS NOT NULL OR datetime(expires_at)<=CURRENT_TIMESTAMP",[])?;
        c.execute("INSERT INTO auth_sessions(token_sha256,user_id,expires_at) VALUES(?1,?2,datetime('now','+'||?3||' seconds'))",params![token_sha256,user_id,ttl_seconds])?;
        c.query_row("SELECT expires_at FROM auth_sessions WHERE token_sha256=?1",[token_sha256],|row|row.get(0)).map_err(Into::into)
    }

    pub fn internal_session(&self,token_sha256:&str)->Result<Option<InternalSessionRecord>>{
        let c=self.conn()?;
        let result=c.query_row(r#"SELECT u.id,u.organization_id,u.username,u.email,u.display_name,u.password_hash,u.system_role,u.must_change_password,u.active,
          CASE WHEN u.locked_until IS NOT NULL AND datetime(u.locked_until)>CURRENT_TIMESTAMP THEN 1 ELSE 0 END,s.expires_at
          FROM auth_sessions s JOIN users u ON u.id=s.user_id
          WHERE s.token_sha256=?1 AND s.revoked_at IS NULL AND datetime(s.expires_at)>CURRENT_TIMESTAMP AND u.active=1 AND u.disabled_at IS NULL"#,[token_sha256],|row|{
            let account=InternalAccountRecord{id:row.get(0)?,organization_id:row.get(1)?,username:row.get(2)?,email:row.get(3)?,display_name:row.get(4)?,password_hash:row.get(5)?,system_role:row.get(6)?,must_change_password:row.get::<_,i64>(7)?!=0,active:row.get::<_,i64>(8)?!=0,locked:row.get::<_,i64>(9)?!=0};
            Ok(InternalSessionRecord{account,expires_at:row.get(10)?})
        }).optional()?;
        if result.is_some(){c.execute("UPDATE auth_sessions SET last_seen_at=CURRENT_TIMESTAMP WHERE token_sha256=?1",[token_sha256])?;}
        Ok(result)
    }

    pub fn revoke_auth_session(&self,token_sha256:&str)->Result<()> {self.conn()?.execute("UPDATE auth_sessions SET revoked_at=CURRENT_TIMESTAMP WHERE token_sha256=?1",[token_sha256])?;Ok(())}

    pub fn revoke_other_auth_sessions(&self,user_id:&str,current_token_sha256:&str)->Result<()> {self.conn()?.execute("UPDATE auth_sessions SET revoked_at=CURRENT_TIMESTAMP WHERE user_id=?1 AND token_sha256<>?2 AND revoked_at IS NULL",params![user_id,current_token_sha256])?;Ok(())}

    pub fn change_internal_password(&self,user_id:&str,password_hash:&str)->Result<()> {
        let c=self.conn()?;
        let changed=c.execute("UPDATE users SET password_hash=?2,must_change_password=0,password_changed_at=CURRENT_TIMESTAMP,failed_login_count=0,locked_until=NULL WHERE id=?1 AND active=1",params![user_id,password_hash])?;
        if changed!=1{bail!("active account not found");}
        c.execute("INSERT INTO account_audit_events(actor_user_id,target_user_id,event_type) VALUES(?1,?1,'password_changed')",[user_id])?;
        Ok(())
    }

    pub fn create_internal_user(&self,actor_user_id:&str,organization_id:&str,username:&str,email:&str,display_name:&str,password_hash:&str)->Result<InternalAccountRecord>{
        let c=self.conn()?;
        let id=Uuid::new_v4().to_string();
        c.execute(r#"INSERT INTO users(id,organization_id,email,display_name,active,username,password_hash,system_role,must_change_password)
          VALUES(?1,?2,?3,?4,1,?5,?6,'user',1)"#,params![id,organization_id,email,display_name,username,password_hash])?;
        c.execute("INSERT INTO account_audit_events(actor_user_id,target_user_id,event_type,detail_json) VALUES(?1,?2,'user_created',?3)",params![actor_user_id,id,json!({"username":username,"email":email}).to_string()])?;
        self.internal_account_by_id(&id)?.context("created account was not persisted")
    }

    pub fn internal_users_json(&self,organization_id:&str)->Result<Value>{
        let c=self.conn()?;
        let mut statement=c.prepare(r#"SELECT id,username,email,display_name,system_role,must_change_password,active,created_at,last_seen_at,disabled_at,locked_until
          FROM users WHERE organization_id=?1 AND password_hash IS NOT NULL ORDER BY system_role='system_admin' DESC,username COLLATE NOCASE"#)?;
        let rows=statement.query_map([organization_id],|row|Ok(json!({"id":row.get::<_,String>(0)?,"username":row.get::<_,String>(1)?,"email":row.get::<_,String>(2)?,"display_name":row.get::<_,String>(3)?,"system_role":row.get::<_,String>(4)?,"must_change_password":row.get::<_,i64>(5)?!=0,"active":row.get::<_,i64>(6)?!=0,"created_at":row.get::<_,String>(7)?,"last_seen_at":row.get::<_,String>(8)?,"disabled_at":row.get::<_,Option<String>>(9)?,"locked_until":row.get::<_,Option<String>>(10)?})))?;
        let mut users=Vec::new();for row in rows{users.push(row?);}Ok(json!({"users":users}))
    }

    pub fn set_internal_user_active(&self,actor_user_id:&str,target_user_id:&str,active:bool)->Result<()> {
        if actor_user_id==target_user_id&&!active{bail!("the active system administrator cannot disable their own account");}
        let c=self.conn()?;
        let target_role:Option<String>=c.query_row("SELECT system_role FROM users WHERE id=?1 AND password_hash IS NOT NULL",[target_user_id],|row|row.get(0)).optional()?;
        if target_role.as_deref()==Some("system_admin")&&!active{bail!("the bootstrap system administrator cannot be disabled");}
        let changed=c.execute("UPDATE users SET active=?2,disabled_at=CASE WHEN ?2=0 THEN CURRENT_TIMESTAMP ELSE NULL END WHERE id=?1 AND password_hash IS NOT NULL",params![target_user_id,active as i64])?;
        if changed!=1{bail!("account not found");}
        if !active{c.execute("UPDATE auth_sessions SET revoked_at=CURRENT_TIMESTAMP WHERE user_id=?1 AND revoked_at IS NULL",[target_user_id])?;}
        c.execute("INSERT INTO account_audit_events(actor_user_id,target_user_id,event_type,detail_json) VALUES(?1,?2,?3,?4)",params![actor_user_id,target_user_id,if active{"user_enabled"}else{"user_disabled"},json!({"active":active}).to_string()])?;
        Ok(())
    }

    pub fn create_password_reset_token(&self,user_id:&str,token_sha256:&str,purpose:&str,ttl_seconds:u64,actor_user_id:Option<&str>)->Result<String>{
        let c=self.conn()?;
        c.execute("UPDATE password_reset_tokens SET used_at=CURRENT_TIMESTAMP WHERE user_id=?1 AND used_at IS NULL",[user_id])?;
        c.execute("INSERT INTO password_reset_tokens(token_sha256,user_id,purpose,expires_at) VALUES(?1,?2,?3,datetime('now','+'||?4||' seconds'))",params![token_sha256,user_id,purpose,ttl_seconds])?;
        c.execute("INSERT INTO account_audit_events(actor_user_id,target_user_id,event_type,detail_json) VALUES(?1,?2,'password_reset_issued',?3)",params![actor_user_id,user_id,json!({"purpose":purpose}).to_string()])?;
        c.query_row("SELECT expires_at FROM password_reset_tokens WHERE token_sha256=?1",[token_sha256],|row|row.get(0)).map_err(Into::into)
    }

    pub fn password_reset_user(&self,token_sha256:&str)->Result<Option<String>>{
        self.conn()?.query_row("SELECT user_id FROM password_reset_tokens WHERE token_sha256=?1 AND used_at IS NULL AND datetime(expires_at)>CURRENT_TIMESTAMP",[token_sha256],|row|row.get(0)).optional().map_err(Into::into)
    }

    pub fn consume_password_reset(&self,token_sha256:&str,password_hash:&str)->Result<String>{
        let mut c=self.conn()?;let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let user_id:Option<String>=tx.query_row("SELECT user_id FROM password_reset_tokens WHERE token_sha256=?1 AND used_at IS NULL AND datetime(expires_at)>CURRENT_TIMESTAMP",[token_sha256],|row|row.get(0)).optional()?;
        let user_id=user_id.context("password reset link is invalid, expired, or already used")?;
        let changed=tx.execute("UPDATE password_reset_tokens SET used_at=CURRENT_TIMESTAMP WHERE token_sha256=?1 AND used_at IS NULL",[token_sha256])?;
        if changed!=1{bail!("password reset link was already used");}
        tx.execute("UPDATE users SET password_hash=?2,must_change_password=0,password_changed_at=CURRENT_TIMESTAMP,failed_login_count=0,locked_until=NULL WHERE id=?1 AND active=1",params![user_id,password_hash])?;
        tx.execute("UPDATE auth_sessions SET revoked_at=CURRENT_TIMESTAMP WHERE user_id=?1 AND revoked_at IS NULL",[&user_id])?;
        tx.execute("INSERT INTO account_audit_events(target_user_id,event_type) VALUES(?1,'password_reset_completed')",[&user_id])?;
        tx.commit()?;Ok(user_id)
    }

    pub fn grant_legacy_projects_to_local_admin(&self,user_id:&str)->Result<()> {
        let c=self.conn()?;
        c.execute(r#"INSERT OR IGNORE INTO project_members(project_id,user_id,role)
          SELECT p.id,?1,'owner' FROM projects p
          WHERE NOT EXISTS(SELECT 1 FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=p.id AND u.active=1)"#,[user_id])?;
        Ok(())
    }

    pub fn add_project_member(&self,project:&str,user_id:&str,role:&str,invited_by:Option<&str>)->Result<()> {
        const ROLES:[&str;7]=["owner","pi","contributor","reviewer","approver","research_administrator","viewer"];
        if !ROLES.contains(&role){bail!("invalid project role: {role}");}
        let c=self.conn()?;
        let organization:String=c.query_row("SELECT organization_id FROM users WHERE id=?1 AND active=1",[user_id],|row|row.get(0)).context("active user not found")?;
        let project_org:Option<String>=c.query_row("SELECT u.organization_id FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 ORDER BY u.active DESC,pm.joined_at LIMIT 1",[project],|row|row.get(0)).optional()?;
        if project_org.as_deref().is_some_and(|value|value!=organization){bail!("cross-organization project membership is not allowed");}
        c.execute(r#"INSERT INTO project_members(project_id,user_id,role,invited_by_user_id) VALUES(?1,?2,?3,?4)
          ON CONFLICT(project_id,user_id) DO UPDATE SET role=excluded.role,last_seen_at=CURRENT_TIMESTAMP"#,params![project,user_id,role,invited_by])?;
        Ok(())
    }

    pub fn project_role(&self,project:&str,user_id:&str)->Result<Option<String>> {
        let mut c=self.conn()?;let tx=c.transaction()?;
        let role=tx.query_row("SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,user_id],|row|row.get(0)).optional()?;
        if role.is_some(){tx.execute("UPDATE project_members SET last_seen_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND user_id=?2",params![project,user_id])?;}
        tx.commit()?;Ok(role)
    }

    pub fn list_projects_for_user_json(&self,user_id:&str,organization_id:&str,include_archived:bool)->Result<Value> {
        let c=self.conn()?;
        let mut st=c.prepare(r#"SELECT p.id,p.title,p.sponsor,p.mechanism,p.created_at,COALESCE(p.updated_at,p.created_at),COALESCE(mine.role,'contributor'),p.archived_at
          FROM projects p
          LEFT JOIN project_members mine ON mine.project_id=p.id AND mine.user_id=?1
          WHERE EXISTS(SELECT 1 FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=p.id AND u.organization_id=?2)
            AND (?3=1 OR p.archived_at IS NULL)
          ORDER BY p.archived_at IS NOT NULL,COALESCE(p.updated_at,p.created_at) DESC LIMIT 250"#)?;
        let rows=st.query_map(params![user_id,organization_id,include_archived as i64],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,"mechanism":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,"updated_at":r.get::<_,String>(5)?,"role":r.get::<_,String>(6)?,"archived_at":r.get::<_,Option<String>>(7)?})))?;
        let mut out=Vec::new();for row in rows{let mut value=row?;let id=value.get("id").and_then(Value::as_str).unwrap_or_default();value["stage"]=json!(self.compatibility_stage(id)?);out.push(value);}Ok(json!(out))
    }

    pub fn ensure_organization_project_member(&self,project:&str,user_id:&str,organization_id:&str)->Result<Option<String>>{
        if let Some(role)=self.project_role(project,user_id)?{return Ok(Some(role));}
        let c=self.conn()?;
        let project_org:Option<String>=c.query_row("SELECT u.organization_id FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 AND u.active=1 ORDER BY CASE pm.role WHEN 'owner' THEN 0 ELSE 1 END,pm.joined_at LIMIT 1",[project],|row|row.get(0)).optional()?;
        if project_org.as_deref()!=Some(organization_id){return Ok(None);}
        drop(c);self.add_project_member(project,user_id,"contributor",None)?;Ok(Some("contributor".into()))
    }

    pub fn update_project_metadata(&self,project:&str,title:Option<&str>,archived:Option<bool>,actor:&str)->Result<Value>{
        let normalized_title=title.map(str::trim);
        if normalized_title.is_some_and(str::is_empty){bail!("project title cannot be empty");}
        if normalized_title.is_some_and(|value|value.chars().count()>240){bail!("project title cannot exceed 240 characters");}
        let mut c=self.conn()?;let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current:(String,Option<String>)=tx.query_row("SELECT title,archived_at FROM projects WHERE id=?1",[project],|row|Ok((row.get(0)?,row.get(1)?))).context("project not found")?;
        let next_title=normalized_title.unwrap_or(&current.0);
        match archived{
            Some(true)=>{tx.execute("UPDATE projects SET title=?1,archived_at=COALESCE(archived_at,CURRENT_TIMESTAMP),archived_by_user_id=CASE WHEN archived_at IS NULL THEN ?2 ELSE archived_by_user_id END,updated_at=CURRENT_TIMESTAMP WHERE id=?3",params![next_title,actor,project])?;}
            Some(false)=>{tx.execute("UPDATE projects SET title=?1,archived_at=NULL,archived_by_user_id=NULL,updated_at=CURRENT_TIMESTAMP WHERE id=?2",params![next_title,project])?;}
            None=>{tx.execute("UPDATE projects SET title=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",params![next_title,project])?;}
        }
        let event_type=match archived{Some(true)=>"project_archived",Some(false)=>"project_restored",None=>"project_metadata_updated"};
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,?2,?3,?4)",params![project,event_type,actor,json!({"previous_title":current.0,"title":next_title,"archived":archived}).to_string()])?;
        tx.commit()?;self.project_json(project)
    }

    pub fn ensure_project_channel(&self,project:&str,kind:&str,subject_key:Option<&str>,name:&str,created_by:&str)->Result<String>{
        let c=self.conn()?;
        let existing:Option<String>=c.query_row("SELECT id FROM channels WHERE project_id=?1 AND kind=?2 AND subject_key IS ?3",params![project,kind,subject_key],|row|row.get(0)).optional()?;
        if let Some(id)=existing{return Ok(id);}
        let id=uuid::Uuid::new_v4().to_string();
        c.execute("INSERT INTO channels(id,project_id,kind,subject_key,name,created_by_user_id) VALUES(?1,?2,?3,?4,?5,?6)",params![id,project,kind,subject_key,name,created_by])?;
        Ok(id)
    }

    pub fn project_json(&self, id: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut project=c.query_row("SELECT id,title,sponsor,mechanism,created_at,COALESCE(updated_at,created_at),interview_generated,archived_at,archived_by_user_id FROM projects WHERE id=?1",[id],|r|Ok(json!({
            "id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,
            "mechanism":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,
            "updated_at":r.get::<_,String>(5)?,"interview_generated":r.get::<_,i64>(6)?!=0,
            "archived_at":r.get::<_,Option<String>>(7)?,"archived_by_user_id":r.get::<_,Option<String>>(8)?
        }))).context("project not found")?;
        project["stage"]=json!(self.compatibility_stage(id)?);
        Ok(project)
    }

    pub fn compatibility_stage(&self, project: &str) -> Result<&'static str> {
        let c = self.conn()?;
        let exists:i64=c.query_row("SELECT COUNT(*) FROM projects WHERE id=?1",[project],|row|row.get(0))?;
        if exists!=1{bail!("project not found");}
        let count=|sql:&str|->Result<i64>{Ok(c.query_row(sql,[project],|row|row.get(0))?)};
        if count("SELECT COUNT(*) FROM export_snapshots WHERE project_id=?1")?>0{return Ok("export");}
        if self.all_required_sections_approved(project)?{return Ok("review");}
        if count("SELECT COUNT(*) FROM section_versions WHERE project_id=?1")?>0{return Ok("writing");}
        for (artifact,stage) in [("literature_manifest","strategy"),("aim_set","science"),("research_framework","research"),("solicitation_profile","interview")] {
            let approved:i64=c.query_row("SELECT COUNT(*) FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 AND approved=1",params![project,artifact],|row|row.get(0))?;
            if approved>0{return Ok(stage);}
        }
        if count("SELECT COUNT(*) FROM requirements WHERE project_id=?1")?>0{return Ok("requirements");}
        if count("SELECT COUNT(*) FROM documents WHERE project_id=?1")?>0{return Ok("documents");}
        Ok("intake")
    }

    pub fn add_document(
        &self,
        project: &str,
        name: &str,
        kind: &str,
        text: &str,
        sha: &str,
    ) -> Result<(i64, bool)> {
        if text.trim().is_empty() {
            bail!("document contains no readable text");
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let n=tx.execute("INSERT OR IGNORE INTO documents(project_id,name,kind,text,sha256) VALUES(?1,?2,?3,?4,?5)",params![project,name,kind,text,sha])?;
        let id = if n > 0 {
            tx.last_insert_rowid()
        } else {
            tx.query_row(
                "SELECT id FROM documents WHERE project_id=?1 AND sha256=?2",
                params![project, sha],
                |r| r.get::<_, i64>(0),
            )
            .context("document disappeared after duplicate check")?
        };
        if n > 0 {
            tx.execute("UPDATE requirements SET approved=0 WHERE project_id=?1",[project])?;
            tx.execute("UPDATE section_versions SET approved=0 WHERE project_id=?1",[project])?;
            tx.execute("UPDATE workflow_artifacts SET approved=0,approved_by=NULL,approved_at=NULL WHERE project_id=?1",[project])?;
            tx.execute("UPDATE projects SET interview_generated=0,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        }
        tx.commit()?;
        Ok((id, n > 0))
    }

    pub fn document_count(&self, project: &str) -> Result<i64> {
        Ok(self.conn()?.query_row(
            "SELECT COUNT(*) FROM documents WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )?)
    }

    pub fn replace_document_chunks(
        &self,
        project: &str,
        document_id: i64,
        chunks: &[crate::chunker::TextChunk],
    ) -> Result<()> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM document_chunks WHERE document_id=?1 AND project_id=?2",
            params![document_id, project],
        )?;
        for ch in chunks {
            tx.execute("INSERT INTO document_chunks(project_id,document_id,ordinal,start_word,end_word,text) VALUES(?1,?2,?3,?4,?5,?6)",params![project,document_id,ch.ordinal as i64,ch.start_word as i64,ch.end_word as i64,ch.text])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn document_context(&self, project: &str, max_chars: usize) -> Result<String> {
        let c = self.conn()?;
        let mut st =
            c.prepare("SELECT name,kind,text FROM documents WHERE project_id=?1 ORDER BY id")?;
        let rows = st.query_map([project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = String::new();
        for row in rows {
            let (name, kind, text) = row?;
            let chunk = format!("\n\n=== {kind}: {name} ===\n{text}");
            if out.len() + chunk.len() > max_chars {
                break;
            }
            out.push_str(&chunk);
        }
        Ok(out)
    }

    pub fn save_analysis(&self, project: &str, kind: &str, content: &str) -> Result<i64> {
        let c = self.conn()?;
        c.execute(
            "INSERT INTO analyses(project_id,kind,content) VALUES(?1,?2,?3)",
            params![project, kind, content],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn ensure_section(&self, project: &str, key: &str, title: &str) -> Result<()> {
        let c = self.conn()?;
        let next: i64 = c.query_row(
            "SELECT COALESCE(MAX(position),-1)+1 FROM project_sections WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )?;
        c.execute("INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required) VALUES(?1,?2,?3,?4,1)",params![project,key,title,next])?;
        c.execute(
            "UPDATE project_sections SET title=?1 WHERE project_id=?2 AND section_key=?3",
            params![title, project, key],
        )?;
        Ok(())
    }

    pub fn project_sections_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare(r#"
          SELECT ps.section_key,ps.title,ps.position,ps.required,ps.origin,
                 (SELECT sv.id FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key ORDER BY sv.id DESC LIMIT 1) latest_version,
                 (SELECT sv.id FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1 ORDER BY sv.id DESC LIMIT 1) approved_version
          FROM project_sections ps WHERE ps.project_id=?1 ORDER BY ps.position,ps.section_key
        "#)?;
        let rows=st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"required":r.get::<_,i64>(3)?!=0,"origin":r.get::<_,String>(4)?,"latest_version":r.get::<_,Option<i64>>(5)?,"approved_version":r.get::<_,Option<i64>>(6)?})))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }

    pub fn save_section(
        &self,
        project: &str,
        key: &str,
        title: &str,
        body: &str,
        html: Option<&str>,
        source: &str,
    ) -> Result<i64> {
        self.save_section_by(project, key, title, body, html, source, None)
    }

    pub fn save_section_by(
        &self,
        project: &str,
        key: &str,
        title: &str,
        body: &str,
        html: Option<&str>,
        source: &str,
        editor: Option<&str>,
    ) -> Result<i64> {
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest:Option<i64>=tx.query_row("SELECT id FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|row|row.get(0)).optional()?;
        let next:i64=tx.query_row("SELECT COALESCE(MAX(position),-1)+1 FROM project_sections WHERE project_id=?1",[project],|row|row.get(0))?;
        tx.execute("INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required) VALUES(?1,?2,?3,?4,1)",params![project,key,title.trim(),next])?;
        tx.execute("UPDATE project_sections SET title=?1 WHERE project_id=?2 AND section_key=?3",params![title.trim(),project,key])?;
        tx.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source,editor_name,author_user_id,base_version_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8)",params![project,key,title,body,html,source,editor,latest])?;
        let id=tx.last_insert_rowid();
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_version_created',?2,?3)",params![project,editor,json!({"section_key":key,"version_id":id,"base_version_id":latest,"source":source}).to_string()])?;
        Self::touch_project_conn(&tx,project)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn save_generated_section(
        &self,project:&str,key:&str,title:&str,body:&str,html:Option<&str>,source:&str,
        generation_run_id:&str,expected_latest:Option<i64>,actor:Option<&str>,
    )->Result<i64>{
        if body.trim().is_empty(){bail!("generated section body cannot be empty");}
        let mut c=self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let(response_sha,status):(Option<String>,String)=tx.query_row("SELECT response_sha256,status FROM generation_runs WHERE id=?1 AND project_id=?2",params![generation_run_id,project],|row|Ok((row.get(0)?,row.get(1)?))).context("generation run does not belong to this project")?;
        if status!="complete"{bail!("generation run must be complete before its output can become a section version");}
        let response_sha=response_sha.context("completed generation run is missing its response digest")?;
        if response_sha!=sha256_hex(body.as_bytes()){bail!("generated section body does not match the immutable generation response digest");}
        let prior_links:i64=tx.query_row("SELECT COUNT(*) FROM section_versions WHERE generation_run_id=?1",[generation_run_id],|row|row.get(0))?;
        if prior_links!=0{bail!("generation run is already linked to a section version");}
        let exists:i64=tx.query_row("SELECT COUNT(*) FROM project_sections WHERE project_id=?1 AND section_key=?2",params![project,key],|row|row.get(0))?;
        if exists!=1{bail!("generated output targets a section that is not present in the active project framework");}
        let latest:Option<i64>=tx.query_row("SELECT id FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|row|row.get(0)).optional()?;
        if latest!=expected_latest{bail!("section changed while generation was running: expected base version {}, found {}",expected_latest.map_or_else(||"none".into(),|value|value.to_string()),latest.map_or_else(||"none".into(),|value|value.to_string()));}
        tx.execute("UPDATE project_sections SET title=?1 WHERE project_id=?2 AND section_key=?3",params![title.trim(),project,key])?;
        tx.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source,editor_name,author_user_id,base_version_id,generation_run_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8,?9)",params![project,key,title.trim(),body,html,source,actor,latest,generation_run_id])?;
        let id=tx.last_insert_rowid();
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_version_created',?2,?3)",params![project,actor,json!({"section_key":key,"version_id":id,"base_version_id":latest,"source":source,"generation_run_id":generation_run_id,"response_sha256":response_sha}).to_string()])?;
        Self::touch_project_conn(&tx,project)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn save_section_edit(
        &self,project:&str,key:&str,title:&str,body:&str,html:Option<&str>,
        expected_latest:Option<i64>,actor:&str,
    )->Result<i64>{
        let mut c=self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest:Option<i64>=tx.query_row("SELECT id FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|row|row.get(0)).optional()?;
        if latest!=expected_latest{bail!("section changed since editing began: expected base version {}, found {}",expected_latest.map_or_else(||"none".into(),|value|value.to_string()),latest.map_or_else(||"none".into(),|value|value.to_string()));}
        let exists:i64=tx.query_row("SELECT COUNT(*) FROM project_sections WHERE project_id=?1 AND section_key=?2",params![project,key],|row|row.get(0))?;
        if exists!=1{bail!("project section does not exist");}
        tx.execute("UPDATE project_sections SET title=?1 WHERE project_id=?2 AND section_key=?3",params![title.trim(),project,key])?;
        tx.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source,editor_name,author_user_id,base_version_id) VALUES(?1,?2,?3,?4,?5,'human_edit',?6,?6,?7)",params![project,key,title,body,html,actor,latest])?;
        let id=tx.last_insert_rowid();
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_version_created',?2,?3)",params![project,actor,json!({"section_key":key,"version_id":id,"base_version_id":latest,"source":"human_edit"}).to_string()])?;
        Self::touch_project_conn(&tx,project)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn section_versions_json(&self, project: &str, key: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st = c.prepare(
            r#"
          SELECT id,created_at,source,COALESCE(author_user_id,editor_name),approved,length(body),
                 base_version_id,restored_from_version_id,
                 CASE WHEN length(body)>180 THEN substr(body,1,180)||'…' ELSE body END
          FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 100
        "#,
        )?;
        let rows=st.query_map(params![project,key],|r|Ok(json!({"version":r.get::<_,i64>(0)?,"created_at":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?,"editor":r.get::<_,Option<String>>(3)?,"approved":r.get::<_,i64>(4)?!=0,"characters":r.get::<_,i64>(5)?,"base_version_id":r.get::<_,Option<i64>>(6)?,"restored_from_version_id":r.get::<_,Option<i64>>(7)?,"preview":r.get::<_,String>(8)?})))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }

    pub fn restore_section_version(
        &self,
        project: &str,
        key: &str,
        version_id: i64,
        expected_latest: i64,
        actor: Option<&str>,
    ) -> Result<i64> {
        let mut c = self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let latest:i64=tx.query_row("SELECT id FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|r|r.get(0)).context("section has no versions")?;
        if latest != expected_latest {
            bail!("section changed since history was loaded: expected latest version {expected_latest}, found {latest}");
        }
        let (title,body,html):(String,String,Option<String>)=tx.query_row("SELECT title,body,html FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3",params![version_id,project,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).context("version does not belong to this project section")?;
        let source=format!("rollback:{version_id}");
        tx.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source,editor_name,author_user_id,base_version_id,restored_from_version_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,?8,?9)",params![project,key,title,body,html,source,actor,latest,version_id])?;
        let id=tx.last_insert_rowid();
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_version_restored',?2,?3)",params![project,actor,json!({"section_key":key,"version_id":id,"base_version_id":latest,"restored_from_version_id":version_id}).to_string()])?;
        Self::touch_project_conn(&tx,project)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn section_version_json(&self,project:&str,key:&str,version_id:i64)->Result<Value>{
        self.conn()?.query_row(r#"SELECT id,title,body,html,source,COALESCE(author_user_id,editor_name),approved,base_version_id,restored_from_version_id,generation_run_id,created_at
          FROM section_versions WHERE project_id=?1 AND section_key=?2 AND id=?3"#,params![project,key,version_id],|row|Ok(json!({"version":row.get::<_,i64>(0)?,"title":row.get::<_,String>(1)?,"body":row.get::<_,String>(2)?,"html":row.get::<_,Option<String>>(3)?,"source":row.get::<_,String>(4)?,"editor":row.get::<_,Option<String>>(5)?,"approved":row.get::<_,i64>(6)?!=0,"base_version_id":row.get::<_,Option<i64>>(7)?,"restored_from_version_id":row.get::<_,Option<i64>>(8)?,"generation_run_id":row.get::<_,Option<String>>(9)?,"created_at":row.get::<_,String>(10)?}))).context("section version does not belong to this project section")
    }

    pub fn section_compare_json(&self,project:&str,key:&str,from_version:i64,to_version:i64)->Result<Value>{
        if from_version==to_version{bail!("comparison requires two different versions");}
        let from=self.section_version_json(project,key,from_version)?;
        let to=self.section_version_json(project,key,to_version)?;
        Ok(json!({"section_key":key,"from":from,"to":to}))
    }

    pub fn section_merge_preview_json(&self,project:&str,key:&str,base_version:i64,latest_version:i64,proposed_body:&str)->Result<Value>{
        let current=self.section_version_json(project,key,latest_version)?;
        let actual_latest:i64=self.conn()?.query_row("SELECT id FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|row|row.get(0)).context("section has no versions")?;
        if actual_latest!=latest_version{bail!("section changed while merge was prepared: expected latest version {latest_version}, found {actual_latest}");}
        let base=self.section_version_json(project,key,base_version)?;
        let base_body=base.get("body").and_then(Value::as_str).context("base section body is missing")?;
        let latest_body=current.get("body").and_then(Value::as_str).context("latest section body is missing")?;
        let result=crate::versioning::three_way_merge(base_body,proposed_body,latest_body);
        Ok(json!({"section_key":key,"base_version_id":base_version,"latest_version_id":latest_version,"clean":result.clean,"merged_body":result.merged_body,"conflicts":result.conflicts}))
    }

    pub fn post_message(&self, project: &str, author_user_id: &str, body: &str) -> Result<i64> {
        let body = body.trim();
        if body.is_empty() {
            bail!("message cannot be empty");
        }
        if body.len() > 8000 {
            bail!("message is too long");
        }
        let c = self.conn()?;
        let member:i64=c.query_row("SELECT COUNT(*) FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,author_user_id],|row|row.get(0))?;
        if member!=1{bail!("project membership is required to post a message");}
        let channel_id:String=c.query_row("SELECT id FROM channels WHERE project_id=?1 AND kind='general' ORDER BY created_at LIMIT 1",[project],|row|row.get(0)).context("project general channel is missing")?;
        c.execute(
            "INSERT INTO messages(channel_id,author_user_id,body) VALUES(?1,?2,?3)",
            params![channel_id, author_user_id, body],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn collaboration_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut members = Vec::new();
        {
            let mut st=c.prepare("SELECT u.id,u.display_name,u.email,pm.role,pm.joined_at,pm.last_seen_at,pm.last_seen_at>=datetime('now','-15 seconds') FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 ORDER BY pm.last_seen_at DESC")?;
            let rows=st.query_map([project],|r|Ok(json!({"user_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"email":r.get::<_,Option<String>>(2)?,"role":r.get::<_,String>(3)?,"joined_at":r.get::<_,String>(4)?,"last_seen_at":r.get::<_,String>(5)?,"present":r.get::<_,i64>(6)?!=0})))?;
            for row in rows {
                members.push(row?);
            }
        }
        let mut messages = Vec::new();
        {
            let mut st=c.prepare("SELECT m.id,m.author_user_id,u.display_name,m.body,m.parent_message_id,m.edited_at,m.created_at FROM messages m JOIN channels c ON c.id=m.channel_id JOIN users u ON u.id=m.author_user_id WHERE c.project_id=?1 AND m.deleted_at IS NULL ORDER BY m.id DESC LIMIT 200")?;
            let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"author_user_id":r.get::<_,String>(1)?,"author":r.get::<_,String>(2)?,"body":r.get::<_,String>(3)?,"parent_message_id":r.get::<_,Option<i64>>(4)?,"edited_at":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,String>(6)?})))?;
            for row in rows {
                messages.push(row?);
            }
            messages.reverse();
        }
        let mut activity = Vec::new();
        {
            let mut st=c.prepare(r#"SELECT kind,actor,detail,created_at FROM (
          SELECT 'edit' kind,COALESCE(editor_name,'System') actor,'saved '||title||' v'||id detail,created_at FROM section_versions WHERE project_id=?1
          UNION ALL SELECT 'approval',COALESCE(a.approved_by,'Team member'),'approved '||sv.title||' v'||a.version_id,a.approved_at FROM approvals a JOIN section_versions sv ON sv.id=a.version_id WHERE a.project_id=?1
          UNION ALL SELECT 'message',u.display_name,'posted in team chat',m.created_at FROM messages m JOIN channels c ON c.id=m.channel_id JOIN users u ON u.id=m.author_user_id WHERE c.project_id=?1
        ) ORDER BY created_at DESC LIMIT 200"#)?;
            let rows=st.query_map([project],|r|Ok(json!({"kind":r.get::<_,String>(0)?,"actor":r.get::<_,String>(1)?,"detail":r.get::<_,String>(2)?,"created_at":r.get::<_,String>(3)?})))?;
            for row in rows {
                activity.push(row?);
            }
        }
        Ok(json!({"members":members,"messages":messages,"activity":activity}))
    }

    pub fn approval_routing_status_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;
        let raw:Option<String>=c.query_row("SELECT body_json FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='collaboration_record' AND approved=1 ORDER BY version DESC LIMIT 1",[project],|row|row.get(0)).optional()?;
        let Some(raw)=raw else{return Ok(json!({"configured":false,"project_owner_user_id":null,"routes":[]}));};
        let routing:crate::workflow_artifacts::CollaborationRouting=serde_json::from_str(&raw)?;
        let mut statuses=Vec::new();
        for route in &routing.routes{
            if route.artifact_type=="proposal_section"{
                let mut statement=c.prepare("SELECT ps.section_key,ps.title,sv.id,sv.approved FROM project_sections ps LEFT JOIN section_versions sv ON sv.id=(SELECT id FROM section_versions latest WHERE latest.project_id=ps.project_id AND latest.section_key=ps.section_key ORDER BY id DESC LIMIT 1) WHERE ps.project_id=?1 ORDER BY ps.position,ps.section_key")?;
                let rows=statement.query_map([project],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<i64>>(2)?,row.get::<_,Option<i64>>(3)?.unwrap_or(0)!=0)))?;
                for row in rows{
                    let(key,title,version,approved)=row?;let artifact_type=format!("section:{key}");
                    let approvals=if let Some(version)=version{c.query_row("SELECT COUNT(*) FROM artifact_approval_decisions WHERE project_id=?1 AND artifact_type=?2 AND artifact_version=?3 AND decision='approved'",params![project,artifact_type,version],|row|row.get::<_,i64>(0))?}else{0};
                    statuses.push(json!({"configured_artifact_type":route.artifact_type,"artifact_type":artifact_type,"artifact_key":key,"title":title,"current_version":version,"approved":approved,"owner_user_id":route.owner_user_id,"approver_user_ids":route.approver_user_ids,"approvals":approvals,"minimum_approvals":route.minimum_approvals,"threshold_met":approvals>=route.minimum_approvals as i64}));
                }
            }else{
                let current:Option<(i64,bool)>=c.query_row("SELECT version,approved FROM workflow_artifacts WHERE project_id=?1 AND artifact_type=?2 ORDER BY version DESC LIMIT 1",params![project,route.artifact_type],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,i64>(1)?!=0))).optional()?;
                let(version,approved)=current.map(|(version,approved)|(Some(version),approved)).unwrap_or((None,false));
                let approvals=if let Some(version)=version{c.query_row("SELECT COUNT(*) FROM artifact_approval_decisions WHERE project_id=?1 AND artifact_type=?2 AND artifact_version=?3 AND decision='approved'",params![project,route.artifact_type,version],|row|row.get::<_,i64>(0))?}else{0};
                statuses.push(json!({"configured_artifact_type":route.artifact_type,"artifact_type":route.artifact_type,"artifact_key":null,"title":route.artifact_type,"current_version":version,"approved":approved,"owner_user_id":route.owner_user_id,"approver_user_ids":route.approver_user_ids,"approvals":approvals,"minimum_approvals":route.minimum_approvals,"threshold_met":approvals>=route.minimum_approvals as i64}));
            }
        }
        Ok(json!({"configured":true,"project_owner_user_id":routing.project_owner_user_id,"routes":statuses}))
    }

    pub fn create_project_invite(&self,project:&str,email:&str,role:&str,invited_by:&str,expires_in_days:u32)->Result<Value>{
        let email=email.trim().to_ascii_lowercase();
        if !email.contains('@')||email.len()>320{bail!("a valid invite email is required");}
        const ROLES:[&str;7]=["owner","pi","contributor","reviewer","approver","research_administrator","viewer"];
        if !ROLES.contains(&role){bail!("invalid project role: {role}");}
        let days=expires_in_days.clamp(1,30);
        let token=Uuid::new_v4().to_string()+&Uuid::new_v4().simple().to_string();
        let token_sha=sha256_hex(token.as_bytes());
        let id=Uuid::new_v4().to_string();
        let modifier=format!("+{days} days");
        let c=self.conn()?;
        c.execute("INSERT INTO project_invites(id,project_id,email,role,token_sha256,invited_by_user_id,expires_at) VALUES(?1,?2,?3,?4,?5,?6,datetime('now',?7))",params![id,project,email,role,token_sha,invited_by,modifier])?;
        Ok(json!({"id":id,"project_id":project,"email":email,"role":role,"token":token,"expires_in_days":days}))
    }

    pub fn accept_project_invite(&self,token:&str,user_id:&str,user_email:Option<&str>)->Result<Value>{
        let token_sha=sha256_hex(token.trim().as_bytes());
        let mut c=self.conn()?;let tx=c.transaction()?;
        let(id,project,email,role):(String,String,String,String)=tx.query_row("SELECT id,project_id,email,role FROM project_invites WHERE token_sha256=?1 AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at>CURRENT_TIMESTAMP",[token_sha],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).context("invite is invalid, expired, accepted, or revoked")?;
        let authenticated_email=user_email.map(str::trim).filter(|value|!value.is_empty()).context("authenticated email is required to accept an invite")?;
        if !email.eq_ignore_ascii_case(authenticated_email){bail!("invite email does not match the authenticated identity");}
        let invite_org:String=tx.query_row("SELECT u.organization_id FROM project_invites i JOIN users u ON u.id=i.invited_by_user_id WHERE i.id=?1",[&id],|row|row.get(0))?;
        let user_org:String=tx.query_row("SELECT organization_id FROM users WHERE id=?1 AND active=1",[user_id],|row|row.get(0)).context("active authenticated user not found")?;
        if invite_org!=user_org{bail!("cross-organization invites are not allowed");}
        tx.execute("INSERT INTO project_members(project_id,user_id,role,invited_by_user_id) SELECT ?1,?2,?3,invited_by_user_id FROM project_invites WHERE id=?4 ON CONFLICT(project_id,user_id) DO UPDATE SET role=excluded.role,last_seen_at=CURRENT_TIMESTAMP",params![project,user_id,role,id])?;
        tx.execute("UPDATE project_invites SET accepted_by_user_id=?1,accepted_at=CURRENT_TIMESTAMP WHERE id=?2",params![user_id,id])?;
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'project_invite_accepted',?2,?3)",params![project,user_id,serde_json::to_string(&json!({"invite_id":id,"role":role}))?])?;
        tx.commit()?;Ok(json!({"project_id":project,"role":role,"accepted":true}))
    }

    pub fn project_invites_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;let mut st=c.prepare("SELECT id,email,role,invited_by_user_id,expires_at,accepted_by_user_id,accepted_at,revoked_at,created_at FROM project_invites WHERE project_id=?1 ORDER BY created_at DESC")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,String>(0)?,"email":r.get::<_,String>(1)?,"role":r.get::<_,String>(2)?,"invited_by_user_id":r.get::<_,String>(3)?,"expires_at":r.get::<_,String>(4)?,"accepted_by_user_id":r.get::<_,Option<String>>(5)?,"accepted_at":r.get::<_,Option<String>>(6)?,"revoked_at":r.get::<_,Option<String>>(7)?,"created_at":r.get::<_,String>(8)?})))?;
        let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))
    }

    pub fn revoke_project_invite(&self,project:&str,invite_id:&str,actor:&str)->Result<()> {
        let changed=self.conn()?.execute("UPDATE project_invites SET revoked_at=CURRENT_TIMESTAMP WHERE id=?1 AND project_id=?2 AND accepted_at IS NULL AND revoked_at IS NULL",params![invite_id,project])?;
        if changed!=1{bail!("active invite not found");}
        self.conn()?.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'project_invite_revoked',?2,?3)",params![project,actor,serde_json::to_string(&json!({"invite_id":invite_id}))?])?;Ok(())
    }

    pub fn post_channel_message(&self,project:&str,kind:&str,subject_key:Option<&str>,author:&str,body:&str,parent:Option<i64>,mentioned_users:&[String])->Result<Value>{
        if !matches!(kind,"general"|"framework"|"aims"|"section"){bail!("unsupported channel kind");}
        if kind=="section"&&subject_key.is_none(){bail!("section channel requires a subject key");}
        let body=body.trim();if body.is_empty()||body.len()>8000{bail!("message must contain 1-8000 characters");}
        let mut c=self.conn()?;let tx=c.transaction()?;
        let member:i64=tx.query_row("SELECT COUNT(*) FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,author],|r|r.get(0))?;if member!=1{bail!("project membership is required");}
        let channel_id:Option<String>=tx.query_row("SELECT id FROM channels WHERE project_id=?1 AND kind=?2 AND subject_key IS ?3",params![project,kind,subject_key],|r|r.get(0)).optional()?;
        let channel_id=match channel_id{Some(id)=>id,None=>{let id=Uuid::new_v4().to_string();let name=if let Some(subject)=subject_key{format!("{kind}: {subject}")}else{kind.to_owned()};tx.execute("INSERT INTO channels(id,project_id,kind,subject_key,name,created_by_user_id) VALUES(?1,?2,?3,?4,?5,?6)",params![id,project,kind,subject_key,name,author])?;id}};
        if let Some(parent_id)=parent{let valid:i64=tx.query_row("SELECT COUNT(*) FROM messages WHERE id=?1 AND channel_id=?2 AND deleted_at IS NULL",params![parent_id,channel_id],|r|r.get(0))?;if valid!=1{bail!("parent message is outside this channel");}}
        tx.execute("INSERT INTO messages(channel_id,author_user_id,body,parent_message_id) VALUES(?1,?2,?3,?4)",params![channel_id,author,body,parent])?;let message_id=tx.last_insert_rowid();
        for mentioned in mentioned_users{if mentioned==author{continue;}let valid:i64=tx.query_row("SELECT COUNT(*) FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,mentioned],|r|r.get(0))?;if valid!=1{bail!("mentioned user {mentioned} is not a project member");}tx.execute("INSERT INTO mentions(project_id,user_id,message_id) VALUES(?1,?2,?3)",params![project,mentioned,message_id])?;tx.execute("INSERT INTO notifications(user_id,project_id,kind,payload_json) VALUES(?1,?2,'mention',?3)",params![mentioned,project,serde_json::to_string(&json!({"message_id":message_id,"channel_id":channel_id,"author_user_id":author}))?])?;}
        tx.commit()?;Ok(json!({"id":message_id,"channel_id":channel_id}))
    }

    pub fn channel_messages_json(&self,project:&str,kind:&str,subject_key:Option<&str>)->Result<Value>{
        let c=self.conn()?;let mut st=c.prepare("SELECT m.id,m.author_user_id,u.display_name,m.body,m.parent_message_id,m.edited_at,m.created_at FROM messages m JOIN channels ch ON ch.id=m.channel_id JOIN users u ON u.id=m.author_user_id WHERE ch.project_id=?1 AND ch.kind=?2 AND ch.subject_key IS ?3 AND m.deleted_at IS NULL ORDER BY m.id LIMIT 500")?;
        let rows=st.query_map(params![project,kind,subject_key],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"author_user_id":r.get::<_,String>(1)?,"author":r.get::<_,String>(2)?,"body":r.get::<_,String>(3)?,"parent_message_id":r.get::<_,Option<i64>>(4)?,"edited_at":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,String>(6)?})))?;let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))
    }

    pub fn add_comment(&self,project:&str,artifact_type:&str,artifact_key:&str,version_id:i64,start:Option<i64>,end:Option<i64>,quoted:Option<&str>,author:&str,body:&str,parent:Option<i64>,mentioned_users:&[String])->Result<Value>{
        let body=body.trim();if body.is_empty()||body.len()>8000{bail!("comment must contain 1-8000 characters");}
        if start.is_some()!=end.is_some(){bail!("comment start and end offsets must be supplied together");}
        if start.zip(end).is_some_and(|(a,b)|a<0||b<=a){bail!("comment range is invalid");}
        let mut c=self.conn()?;let tx=c.transaction()?;
        let target:Option<String>=match artifact_type{"section"=>tx.query_row("SELECT body FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3",params![version_id,project,artifact_key],|r|r.get(0)).optional()?,_=>tx.query_row("SELECT body_json FROM workflow_artifacts WHERE id=?1 AND project_id=?2 AND artifact_type=?3",params![version_id,project,artifact_key],|r|r.get(0)).optional()?};
        let target=target.context("comment target version is outside this project artifact")?;
        if let Some((start,end))=start.zip(end){
            let exact=target.get(start as usize..end as usize).context("comment range is outside the target or splits a UTF-8 character")?;
            let quoted=quoted.map(str::trim).filter(|value|!value.is_empty()).context("quoted text is required for a ranged comment")?;
            if quoted!=exact{bail!("quoted text is not the exact target range");}
        }else if quoted.is_some_and(|value|!value.trim().is_empty()){bail!("quoted text requires start and end offsets");}
        if let Some(parent_id)=parent{let valid:i64=tx.query_row("SELECT COUNT(*) FROM comments WHERE id=?1 AND project_id=?2 AND artifact_type=?3 AND artifact_key=?4 AND version_id=?5",params![parent_id,project,artifact_type,artifact_key,version_id],|r|r.get(0))?;if valid!=1{bail!("parent comment is outside this exact artifact version");}}
        tx.execute("INSERT INTO comments(project_id,artifact_type,artifact_key,version_id,start_offset,end_offset,quoted_text,author_user_id,body,parent_comment_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![project,artifact_type,artifact_key,version_id,start,end,quoted,author,body,parent])?;let comment_id=tx.last_insert_rowid();
        for mentioned in mentioned_users{if mentioned==author{continue;}let valid:i64=tx.query_row("SELECT COUNT(*) FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,mentioned],|r|r.get(0))?;if valid!=1{bail!("mentioned user {mentioned} is not a project member");}tx.execute("INSERT INTO mentions(project_id,user_id,comment_id) VALUES(?1,?2,?3)",params![project,mentioned,comment_id])?;tx.execute("INSERT INTO notifications(user_id,project_id,kind,payload_json) VALUES(?1,?2,'mention',?3)",params![mentioned,project,serde_json::to_string(&json!({"comment_id":comment_id,"artifact_type":artifact_type,"artifact_key":artifact_key,"author_user_id":author}))?])?;}
        tx.commit()?;Ok(json!({"id":comment_id}))
    }

    pub fn comments_json(&self,project:&str,artifact_type:&str,artifact_key:&str,version_id:Option<i64>)->Result<Value>{
        let c=self.conn()?;let mut st=c.prepare("SELECT c.id,c.version_id,c.start_offset,c.end_offset,c.quoted_text,c.author_user_id,u.display_name,c.body,c.parent_comment_id,c.resolved_by_user_id,c.resolved_at,c.created_at FROM comments c JOIN users u ON u.id=c.author_user_id WHERE c.project_id=?1 AND c.artifact_type=?2 AND c.artifact_key=?3 AND (?4 IS NULL OR c.version_id=?4) ORDER BY c.id")?;let rows=st.query_map(params![project,artifact_type,artifact_key,version_id],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"version_id":r.get::<_,i64>(1)?,"start_offset":r.get::<_,Option<i64>>(2)?,"end_offset":r.get::<_,Option<i64>>(3)?,"quoted_text":r.get::<_,Option<String>>(4)?,"author_user_id":r.get::<_,String>(5)?,"author":r.get::<_,String>(6)?,"body":r.get::<_,String>(7)?,"parent_comment_id":r.get::<_,Option<i64>>(8)?,"resolved_by_user_id":r.get::<_,Option<String>>(9)?,"resolved_at":r.get::<_,Option<String>>(10)?,"created_at":r.get::<_,String>(11)?})))?;let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))
    }

    pub fn resolve_comment(&self,project:&str,comment_id:i64,user_id:&str)->Result<()> {let changed=self.conn()?.execute("UPDATE comments SET resolved_by_user_id=?1,resolved_at=CURRENT_TIMESTAMP WHERE id=?2 AND project_id=?3 AND resolved_at IS NULL",params![user_id,comment_id,project])?;if changed!=1{bail!("open comment not found");}Ok(())}

    pub fn create_task(&self,project:&str,title:&str,description:&str,owner:&str,source:&str,priority:&str,due_at:Option<&str>,created_by:&str,dependencies:&[String])->Result<Value>{
        let title=title.trim();if title.is_empty()||title.len()>500{bail!("task title must contain 1-500 characters");}if !matches!(priority,"low"|"normal"|"high"|"critical"){bail!("invalid task priority");}
        let id=Uuid::new_v4().to_string();let mut c=self.conn()?;
        if let Some(due)=due_at{
            let valid:i64=c.query_row("SELECT julianday(?1) IS NOT NULL",[due],|row|row.get(0))?;
            if valid!=1{bail!("task due date must be an ISO-8601 date or timestamp");}
        }
        let tx=c.transaction()?;let owner_member:i64=tx.query_row("SELECT COUNT(*) FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,owner],|r|r.get(0))?;if owner_member!=1{bail!("task owner must be a project member");}
        tx.execute("INSERT INTO tasks(id,project_id,title,description,owner_user_id,source,priority,due_at,created_by_user_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,project,title,description,owner,source,priority,due_at,created_by])?;for dependency in dependencies{let valid:i64=tx.query_row("SELECT COUNT(*) FROM tasks WHERE id=?1 AND project_id=?2",params![dependency,project],|r|r.get(0))?;if valid!=1{bail!("task dependency {dependency} is outside this project");}tx.execute("INSERT INTO task_dependencies(task_id,depends_on_task_id) VALUES(?1,?2)",params![id,dependency])?;}tx.execute("INSERT INTO notifications(user_id,project_id,kind,payload_json) VALUES(?1,?2,'task_assigned',?3)",params![owner,project,serde_json::to_string(&json!({"task_id":id,"title":title}))?])?;tx.commit()?;Ok(json!({"id":id}))
    }

    pub fn tasks_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;let mut st=c.prepare("SELECT id,title,description,owner_user_id,source,status,priority,due_at,completed_at,created_by_user_id,created_at,updated_at FROM tasks WHERE project_id=?1 ORDER BY CASE priority WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,due_at,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"description":r.get::<_,String>(2)?,"owner_user_id":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,"priority":r.get::<_,String>(6)?,"due_at":r.get::<_,Option<String>>(7)?,"completed_at":r.get::<_,Option<String>>(8)?,"created_by_user_id":r.get::<_,String>(9)?,"created_at":r.get::<_,String>(10)?,"updated_at":r.get::<_,String>(11)?})))?;
        let mut out=Vec::new();
        for row in rows{
            let mut task=row?;let task_id=task.get("id").and_then(Value::as_str).unwrap_or_default();
            let mut dependencies=Vec::new();let mut dependency_statement=c.prepare("SELECT depends_on_task_id FROM task_dependencies WHERE task_id=?1 ORDER BY depends_on_task_id")?;
            for dependency in dependency_statement.query_map([task_id],|row|row.get::<_,String>(0))?{dependencies.push(Value::String(dependency?));}
            task["dependencies"]=Value::Array(dependencies);out.push(task);
        }
        Ok(Value::Array(out))
    }

    pub fn update_task_status(&self,project:&str,task_id:&str,status:&str,actor:&str,actor_role:&str)->Result<()> {
        if !matches!(status,"open"|"in_progress"|"blocked"|"complete"|"cancelled"){bail!("invalid task status");}
        let c=self.conn()?;let owner:Option<String>=c.query_row("SELECT owner_user_id FROM tasks WHERE id=?1 AND project_id=?2",params![task_id,project],|row|row.get(0)).optional()?;
        let owner=owner.context("task not found")?;
        if owner!=actor&&!matches!(actor_role,"owner"|"pi"|"research_administrator"){bail!("only the task owner or project leadership can change task status");}
        c.execute("UPDATE tasks SET status=?1,completed_at=CASE WHEN ?1='complete' THEN CURRENT_TIMESTAMP ELSE NULL END,updated_at=CURRENT_TIMESTAMP WHERE id=?2 AND project_id=?3",params![status,task_id,project])?;Ok(())
    }

    pub fn notifications_json(&self,user_id:&str,project:Option<&str>)->Result<Value>{let c=self.conn()?;let mut st=c.prepare("SELECT id,project_id,kind,payload_json,read_at,created_at FROM notifications WHERE user_id=?1 AND (?2 IS NULL OR project_id=?2) ORDER BY id DESC LIMIT 500")?;let rows=st.query_map(params![user_id,project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"project_id":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"payload":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(json!({})),"read_at":r.get::<_,Option<String>>(4)?,"created_at":r.get::<_,String>(5)?})))?;let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))}

    pub fn mark_notification_read(&self,user_id:&str,notification_id:i64)->Result<()> {let changed=self.conn()?.execute("UPDATE notifications SET read_at=COALESCE(read_at,CURRENT_TIMESTAMP) WHERE id=?1 AND user_id=?2",params![notification_id,user_id])?;if changed!=1{bail!("notification not found");}Ok(())}

    pub fn section_state_json(&self, project: &str, key: &str) -> Result<Value> {
        let c = self.conn()?;
        let meta=c.query_row("SELECT title,position,required FROM project_sections WHERE project_id=?1 AND section_key=?2",params![project,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?!=0))).optional()?;
        let Some((title, position, required)) = meta else {
            return Ok(json!({"section_key":key,"exists":false}));
        };
        let latest=c.query_row("SELECT id,body,html,source,approved,created_at FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|r|Ok(json!({"version":r.get::<_,i64>(0)?,"body":r.get::<_,String>(1)?,"html":r.get::<_,Option<String>>(2)?,"source":r.get::<_,String>(3)?,"approved":r.get::<_,i64>(4)?!=0,"created_at":r.get::<_,String>(5)?}))).optional()?;
        let approved=c.query_row("SELECT id,body,source,created_at FROM section_versions WHERE project_id=?1 AND section_key=?2 AND approved=1 ORDER BY id DESC LIMIT 1",params![project,key],|r|Ok(json!({"version":r.get::<_,i64>(0)?,"body":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?,"created_at":r.get::<_,String>(3)?}))).optional()?;
        let competitive_update=c.query_row(r#"
          SELECT csu.id,csu.event_id,csu.base_version_id,csu.proposed_version_id,csu.status,
                 e.from_run_id,e.to_run_id,e.summary,e.delta_json,e.refresh_reason_json,e.created_at,base.body,proposed.body
          FROM competitive_section_updates csu
          JOIN competitive_update_events e ON e.id=csu.event_id
          JOIN section_versions base ON base.id=csu.base_version_id
          JOIN section_versions proposed ON proposed.id=csu.proposed_version_id
          WHERE csu.project_id=?1 AND csu.section_key=?2 AND csu.status='pending'
          ORDER BY csu.event_id DESC LIMIT 1
        "#,params![project,key],|r|{
            let delta_raw=r.get::<_,String>(8)?;
            let reason_raw=r.get::<_,String>(9)?;
            Ok(json!({"id":r.get::<_,i64>(0)?,"event_id":r.get::<_,i64>(1)?,"base_version":r.get::<_,i64>(2)?,"proposed_version":r.get::<_,i64>(3)?,"status":r.get::<_,String>(4)?,"from_run_id":r.get::<_,Option<i64>>(5)?,"to_run_id":r.get::<_,i64>(6)?,"summary":r.get::<_,String>(7)?,"delta":serde_json::from_str::<Value>(&delta_raw).unwrap_or(json!({})),"refresh_reason":serde_json::from_str::<Value>(&reason_raw).unwrap_or(json!([])),"created_at":r.get::<_,String>(10)?,"base_body":r.get::<_,String>(11)?,"proposed_body":r.get::<_,String>(12)?}))
        }).optional()?;
        Ok(
            json!({"section_key":key,"exists":true,"title":title,"position":position,"required":required,"latest":latest,"approved":approved,"competitive_update":competitive_update}),
        )
    }

    pub fn latest_sections_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare(r#"
          SELECT ps.section_key,ps.title,ps.position,sv.id,sv.body,sv.source,sv.approved
          FROM project_sections ps
          JOIN section_versions sv ON sv.id=(SELECT id FROM section_versions x WHERE x.project_id=ps.project_id AND x.section_key=ps.section_key ORDER BY id DESC LIMIT 1)
          WHERE ps.project_id=?1 ORDER BY ps.position,ps.section_key
        "#)?;
        let rows=st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"version":r.get::<_,i64>(3)?,"body":r.get::<_,String>(4)?,"source":r.get::<_,String>(5)?,"approved":r.get::<_,i64>(6)?!=0})))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }

    pub fn approve_section_version(
        &self,
        project: &str,
        key: &str,
        version_id: i64,
    ) -> Result<i64> {
        let result=self.approve_section_version_by(project,key,version_id,None)?;
        result.get("approved_version").and_then(Value::as_i64).context("section approval did not complete")
    }
    pub fn approve_section_version_by(
        &self,
        project: &str,
        key: &str,
        version_id: i64,
        actor: Option<&str>,
    ) -> Result<Value> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let exists:i64=tx.query_row("SELECT COUNT(*) FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3",params![version_id,project,key],|r|r.get(0))?;
        if exists != 1 {
            bail!("section version {version_id} does not belong to project/section");
        }
        let approval_type=format!("section:{key}");
        let role:Option<String>=if let Some(user)=actor{tx.query_row("SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,user],|row|row.get(0)).optional()?}else{None};
        if let (Some(user),Some(role))=(actor,role.as_deref()){
            tx.execute(
                "INSERT INTO artifact_approval_events(project_id,artifact_type,artifact_version,actor_user_id,role_at_decision,decision) VALUES(?1,?2,?3,?4,?5,'approved')",
                params![project,approval_type,version_id,user,role],
            )?;
        }
        let approval_progress=Self::record_configured_artifact_approval(&tx,project,&approval_type,version_id,actor)?;
        if let Some(progress)=&approval_progress{
            if !progress.get("threshold_met").and_then(Value::as_bool).unwrap_or(false){
                tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_approval_recorded',?2,?3)",params![project,actor,serde_json::to_string(progress)?])?;
                Self::touch_project_conn(&tx,project)?;tx.commit()?;
                return Ok(json!({"section_key":key,"version":version_id,"approved":false,"approval_progress":progress}));
            }
        }
        tx.execute(
            "UPDATE section_versions SET approved=0 WHERE project_id=?1 AND section_key=?2",
            params![project, key],
        )?;
        tx.execute(
            "UPDATE section_versions SET approved=1 WHERE id=?1",
            [version_id],
        )?;
        tx.execute("INSERT INTO approvals(project_id,section_key,version_id,approved_by,approver_user_id,role_at_approval,decision) VALUES(?1,?2,?3,?4,?4,?5,'approved')",params![project,key,version_id,actor,role])?;
        // Any explicit post-update human approval resolves pending competitive text proposals for this section.
        // The human may approve the proposed version, an edited derivative, or deliberately re-approve the prior text.
        tx.execute("UPDATE competitive_section_updates SET status='resolved_by_human',resolved_version_id=?1,resolved_at=CURRENT_TIMESTAMP WHERE project_id=?2 AND section_key=?3 AND status='pending'",params![version_id,project,key])?;
        tx.execute(
            "UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",
            [project],
        )?;
        tx.commit()?;
        Ok(json!({"section_key":key,"approved_version":version_id,"approved":true,"approval_progress":approval_progress}))
    }

    pub fn return_section_for_revision(
        &self,
        project:&str,
        key:&str,
        version_id:i64,
        actor:&str,
        rationale:&str,
    )->Result<Value>{
        let rationale=rationale.trim();
        if rationale.is_empty(){bail!("a revision rationale is required");}
        if rationale.chars().count()>4000{bail!("revision rationale exceeds 4000 characters");}
        let mut c=self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let role:String=tx.query_row(
            "SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",
            params![project,actor],|row|row.get(0),
        ).context("revision actor is not a project member")?;
        let latest:Option<(i64,bool)>=tx.query_row(
            "SELECT id,approved!=0 FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",
            params![project,key],|row|Ok((row.get(0)?,row.get(1)?)),
        ).optional()?;
        let Some((latest_version,approved))=latest else{bail!("proposal section has no saved version");};
        if latest_version!=version_id{bail!("only the latest section version can be returned; latest is {latest_version}");}
        let approval_type=format!("section:{key}");
        let pending_approvals:i64=tx.query_row(
            "SELECT COUNT(*) FROM artifact_approval_decisions WHERE project_id=?1 AND artifact_type=?2 AND artifact_version=?3 AND decision='approved'",
            params![project,approval_type,version_id],|row|row.get(0),
        )?;
        if !approved&&pending_approvals==0{bail!("the selected section version is neither approved nor awaiting configured approvals");}
        tx.execute(
            "UPDATE section_versions SET approved=0 WHERE project_id=?1 AND section_key=?2 AND id=?3",
            params![project,key,version_id],
        )?;
        tx.execute(
            "UPDATE artifact_approval_decisions SET decision='rejected',notes=?1,created_at=CURRENT_TIMESTAMP WHERE project_id=?2 AND artifact_type=?3 AND artifact_version=?4",
            params![rationale,project,approval_type,version_id],
        )?;
        tx.execute(
            "INSERT INTO artifact_approval_events(project_id,artifact_type,artifact_version,actor_user_id,role_at_decision,decision,notes) VALUES(?1,?2,?3,?4,?5,'rejected',?6)",
            params![project,approval_type,version_id,actor,role,rationale],
        )?;
        tx.execute(
            "INSERT INTO approvals(project_id,section_key,version_id,approved_by,approver_user_id,role_at_approval,decision,notes) VALUES(?1,?2,?3,?4,?4,?5,'rejected',?6)",
            params![project,key,version_id,actor,role,rationale],
        )?;
        let event=json!({
            "section_key":key,"version":version_id,"decision":"returned_for_revision",
            "rationale":rationale,"role_at_decision":role,"prior_approval_votes":pending_approvals,
            "history_preserved":true
        });
        tx.execute(
            "INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'section_returned_for_revision',?2,?3)",
            params![project,actor,serde_json::to_string(&event)?],
        )?;
        Self::touch_project_conn(&tx,project)?;
        tx.commit()?;
        Ok(event)
    }

    pub fn approved_sections_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare(r#"
          SELECT ps.section_key,ps.title,sv.body,sv.html,sv.id,ps.position
          FROM project_sections ps
          JOIN section_versions sv ON sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1
          WHERE ps.project_id=?1
          ORDER BY ps.position ASC, ps.section_key ASC
        "#)?;
        let rows=st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"body":r.get::<_,String>(2)?,"html":r.get::<_,Option<String>>(3)?,"version":r.get::<_,i64>(4)?,"position":r.get::<_,i64>(5)?})))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }

    pub fn all_required_sections_approved(&self, project: &str) -> Result<bool> {
        let c = self.conn()?;
        let total: i64 = c.query_row(
            "SELECT COUNT(*) FROM project_sections WHERE project_id=?1 AND required=1",
            [project],
            |r| r.get(0),
        )?;
        let missing:i64=c.query_row(r#"
          SELECT COUNT(*) FROM project_sections ps
          WHERE ps.project_id=?1 AND ps.required=1 AND NOT EXISTS(
            SELECT 1 FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1)
        "#,[project],|r|r.get(0))?;
        Ok(total > 0 && missing == 0)
    }

    pub fn replace_requirements(&self, project: &str, reqs: &[RequirementDraft]) -> Result<()> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        // Delete downstream objects in foreign-key-safe order before replacing the
        // authoritative requirement set.  Research sources reference queries and
        // citations reference evidence, so parents must be removed last.
        tx.execute("DELETE FROM citations WHERE project_id=?1", [project])?;
        tx.execute("DELETE FROM evidence WHERE project_id=?1", [project])?;
        tx.execute(
            "DELETE FROM research_sources WHERE project_id=?1",
            [project],
        )?;
        tx.execute(
            "DELETE FROM research_queries WHERE project_id=?1",
            [project],
        )?;
        tx.execute(
            "DELETE FROM interview_answers WHERE project_id=?1",
            [project],
        )?;
        tx.execute(
            "DELETE FROM interview_questions WHERE project_id=?1",
            [project],
        )?;
        tx.execute("DELETE FROM requirements WHERE project_id=?1", [project])?;
        tx.execute(
            "UPDATE section_versions SET approved=0 WHERE project_id=?1",
            [project],
        )?;
        for r in reqs {
            tx.execute("INSERT INTO requirements(project_id,external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,source_clue,source_document,source_locator) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![project,r.external_id,r.category,r.requirement,r.mandatory as i32,serde_json::to_string(&r.evidence_needed)?,serde_json::to_string(&r.dependencies)?,r.source_clue,r.source_document,r.source_locator])?;
        }
        tx.execute("UPDATE projects SET interview_generated=0,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?;
        Ok(())
    }

    pub fn requirements_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,source_clue,source_document,source_locator,status,approved FROM requirements WHERE project_id=?1 ORDER BY mandatory DESC,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,String>(0)?,"category":r.get::<_,String>(1)?,"requirement":r.get::<_,String>(2)?,"mandatory":r.get::<_,i64>(3)?!=0,"evidence_needed":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"dependencies":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(json!([])),"source_clue":r.get::<_,Option<String>>(6)?,"source_document":r.get::<_,Option<String>>(7)?,"source_locator":r.get::<_,Option<String>>(8)?,"status":r.get::<_,String>(9)?,"approved":r.get::<_,i64>(10)?!=0})))?;
        let mut out = vec![];
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }
    pub fn requirements_context(&self, project: &str) -> Result<String> {
        Ok(serde_json::to_string_pretty(
            &self.requirements_json(project)?,
        )?)
    }
    pub fn requirements_all_approved(&self, project: &str) -> Result<bool> {
        let c = self.conn()?;
        let (total, approved): (i64, i64) = c.query_row(
            "SELECT COUNT(*),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1",
            [project],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(total > 0 && total == approved)
    }
    pub fn approve_requirements(&self, project: &str) -> Result<usize> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let total: i64 = tx.query_row(
            "SELECT COUNT(*) FROM requirements WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )?;
        if total == 0 {
            bail!("no parsed requirements exist to approve");
        }
        let n = tx.execute(
            "UPDATE requirements SET approved=1 WHERE project_id=?1",
            [project],
        )?;
        tx.execute("UPDATE projects SET interview_generated=0,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?;
        Ok(n)
    }

    pub fn replace_open_interview_questions(
        &self,
        project: &str,
        questions: &[InterviewQuestionDraft],
    ) -> Result<()> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        tx.execute(
            "DELETE FROM interview_questions WHERE project_id=?1 AND status='open'",
            [project],
        )?;
        for q in questions {
            tx.execute("INSERT INTO interview_questions(project_id,requirement_external_id,question,answer_type,choices_json,unit,why_needed,evidence_requested,priority) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![project,q.requirement_id,q.question,q.answer_type,serde_json::to_string(&q.choices)?,q.unit,q.why_needed,q.evidence_requested as i32,q.priority])?;
        }
        tx.execute("UPDATE projects SET interview_generated=1,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?;
        Ok(())
    }
    pub fn interview_generated(&self, project: &str) -> Result<bool> {
        Ok(self.conn()?.query_row(
            "SELECT interview_generated FROM projects WHERE id=?1",
            [project],
            |r| r.get::<_, i64>(0),
        )? != 0)
    }
    pub fn interview_open_count(&self, project: &str) -> Result<i64> {
        Ok(self.conn()?.query_row(
            "SELECT COUNT(*) FROM interview_questions WHERE project_id=?1 AND status='open'",
            [project],
            |r| r.get(0),
        )?)
    }
    pub fn interview_questions_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,requirement_external_id,question,answer_type,choices_json,unit,why_needed,evidence_requested,priority,status FROM interview_questions WHERE project_id=?1 ORDER BY CASE status WHEN 'open' THEN 0 ELSE 1 END,priority DESC,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_id":r.get::<_,String>(1)?,"question":r.get::<_,String>(2)?,"answer_type":r.get::<_,String>(3)?,"choices":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"unit":r.get::<_,Option<String>>(5)?,"why_needed":r.get::<_,Option<String>>(6)?,"evidence_requested":r.get::<_,i64>(7)?!=0,"priority":r.get::<_,i64>(8)?,"status":r.get::<_,String>(9)?})))?;
        let mut out = vec![];
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }
    pub fn save_interview_answer(
        &self,
        project: &str,
        question_id: i64,
        value: &Value,
        confidence: &str,
        classification: &str,
        notes: Option<&str>,
        answered_by: Option<&str>,
    ) -> Result<i64> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let status: String = tx
            .query_row(
                "SELECT status FROM interview_questions WHERE id=?1 AND project_id=?2",
                params![question_id, project],
                |r| r.get(0),
            )
            .context("interview question not found for project")?;
        if status != "open" {
            bail!("interview question {question_id} is not open");
        }
        tx.execute("INSERT INTO interview_answers(project_id,question_id,value_json,confidence,classification,notes,answered_by) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![project,question_id,serde_json::to_string(value)?,confidence,classification,notes,answered_by])?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE interview_questions SET status='answered' WHERE id=?1 AND project_id=?2",
            params![question_id, project],
        )?;
        tx.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?;
        Ok(id)
    }
    pub fn interview_context(&self, project: &str) -> Result<String> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT q.requirement_external_id,q.question,a.value_json,a.confidence,a.classification,a.notes FROM interview_answers a JOIN interview_questions q ON q.id=a.question_id WHERE a.project_id=?1 ORDER BY a.id")?;
        let rows=st.query_map([project],|r|Ok(json!({"requirement_id":r.get::<_,String>(0)?,"question":r.get::<_,String>(1)?,"answer":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(json!(null)),"confidence":r.get::<_,String>(3)?,"classification":r.get::<_,String>(4)?,"notes":r.get::<_,Option<String>>(5)?})))?;
        let mut out = vec![];
        for row in rows {
            out.push(row?);
        }
        Ok(serde_json::to_string_pretty(&out)?)
    }

    pub fn insert_research_query(
        &self,
        project: &str,
        requirement_id: &str,
        query: &str,
        domains: &[String],
        rationale: &str,
    ) -> Result<i64> {
        let c = self.conn()?;
        c.execute("INSERT INTO research_queries(project_id,requirement_external_id,query,preferred_domains_json,rationale) VALUES(?1,?2,?3,?4,?5)",params![project,requirement_id,query,serde_json::to_string(domains)?,rationale])?;
        Ok(c.last_insert_rowid())
    }
    pub fn mark_research_query(&self, id: i64, status: &str) -> Result<()> {
        self.conn()?.execute(
            "UPDATE research_queries SET status=?1 WHERE id=?2",
            params![status, id],
        )?;
        Ok(())
    }
    pub fn add_research_source(
        &self,
        project: &str,
        query_id: i64,
        src: &FetchedSource,
    ) -> Result<Option<i64>> {
        let c = self.conn()?;
        let n=c.execute("INSERT OR IGNORE INTO research_sources(project_id,query_id,title,url,text,retrieved_at,content_sha256,http_status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project,query_id,src.title,src.url,src.text,src.retrieved_at,src.sha256,src.status])?;
        if n > 0 {
            return Ok(Some(c.last_insert_rowid()));
        }
        Ok(c.query_row(
            "SELECT id FROM research_sources WHERE project_id=?1 AND url=?2 AND content_sha256=?3",
            params![project, src.url, src.sha256],
            |r| r.get::<_, i64>(0),
        )
        .optional()?)
    }
    pub fn add_evidence(
        &self,
        project: &str,
        requirement_id: Option<&str>,
        source_type: &str,
        source_ref: &str,
        claim: &str,
        passage: &str,
        url: Option<&str>,
        locator: Option<&str>,
        confidence: f64,
        status: &str,
    ) -> Result<i64> {
        let c = self.conn()?;
        c.execute("INSERT INTO evidence(project_id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![project,requirement_id,source_type,source_ref,claim,passage,url,locator,confidence,status])?;
        Ok(c.last_insert_rowid())
    }
    pub fn add_citation(
        &self,
        project: &str,
        evidence_id: i64,
        key: &str,
        title: &str,
        url: Option<&str>,
        passage: &str,
        sha: &str,
        verified: bool,
    ) -> Result<i64> {
        let c = self.conn()?;
        c.execute("INSERT INTO citations(project_id,evidence_id,citation_key,title,url,passage,content_sha256,verified) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project,evidence_id,key,title,url,passage,sha,verified as i32])?;
        Ok(c.last_insert_rowid())
    }
    pub fn evidence_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status FROM evidence WHERE project_id=?1 ORDER BY id DESC")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_id":r.get::<_,Option<String>>(1)?,"source_type":r.get::<_,String>(2)?,"source_ref":r.get::<_,String>(3)?,"claim":r.get::<_,String>(4)?,"passage":r.get::<_,String>(5)?,"url":r.get::<_,Option<String>>(6)?,"locator":r.get::<_,Option<String>>(7)?,"confidence":r.get::<_,f64>(8)?,"status":r.get::<_,String>(9)?})))?;
        let mut out = vec![];
        for row in rows {
            out.push(row?);
        }
        Ok(json!(out))
    }
    pub fn evidence_context(&self, project: &str, max_chars: usize) -> Result<String> {
        let mut s = serde_json::to_string_pretty(&self.evidence_json(project)?)?;
        if s.len() > max_chars {
            s.truncate(max_chars);
        }
        Ok(s)
    }
    pub fn requirement_ids(&self, project: &str) -> Result<Vec<String>> {
        let c = self.conn()?;
        let mut st =
            c.prepare("SELECT external_id FROM requirements WHERE project_id=?1 ORDER BY id")?;
        let rows = st.query_map([project], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn save_clinical_study(&self, project: &str, study: &ClinicalStudy) -> Result<Value> {
        crate::clinical::validate_study(study)?;
        let bytes = serde_json::to_vec(study)?;
        let sha = sha256_hex(&bytes);
        let study_json = String::from_utf8(bytes)?;
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM projects WHERE id=?1",
            [project],
            |r| r.get(0),
        )?;
        if exists != 1 {
            bail!("project not found");
        }
        let version: i64 = tx
            .query_row(
                "SELECT COALESCE(version,0)+1 FROM clinical_studies WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(1);
        tx.execute("INSERT INTO clinical_study_history(project_id,version,study_json,content_sha256) VALUES(?1,?2,?3,?4)",params![project,version,study_json,sha])?;
        tx.execute(r#"INSERT INTO clinical_studies(project_id,version,study_json,content_sha256,updated_at)
            VALUES(?1,?2,?3,?4,CURRENT_TIMESTAMP)
            ON CONFLICT(project_id) DO UPDATE SET version=excluded.version,study_json=excluded.study_json,content_sha256=excluded.content_sha256,updated_at=CURRENT_TIMESTAMP"#,
            params![project,version,study_json,sha])?;
        tx.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?;
        Ok(json!({"version":version,"sha256":sha,"study":study}))
    }

    pub fn clinical_study_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row("SELECT version,study_json,content_sha256,updated_at FROM clinical_studies WHERE project_id=?1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional()?;
        if let Some((version, raw, sha, updated_at)) = row {
            let study: Value =
                serde_json::from_str(&raw).context("stored clinical study JSON is invalid")?;
            Ok(
                json!({"exists":true,"version":version,"sha256":sha,"updated_at":updated_at,"study":study}),
            )
        } else {
            Ok(json!({"exists":false,"version":null,"sha256":null,"updated_at":null,"study":null}))
        }
    }

    pub fn clinical_study_typed(&self, project: &str) -> Result<Option<ClinicalStudy>> {
        let c = self.conn()?;
        let raw = c
            .query_row(
                "SELECT study_json FROM clinical_studies WHERE project_id=?1",
                [project],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        match raw {
            Some(x) => Ok(Some(
                serde_json::from_str(&x).context("stored clinical study JSON is invalid")?,
            )),
            None => Ok(None),
        }
    }

    pub fn clinical_context(&self, project: &str) -> Result<String> {
        let Some(study) = self.clinical_study_typed(project)? else {
            return Ok("CLINICAL STUDY MODEL: not configured".into());
        };
        let assessment = crate::clinical::assess(&study, &self.approved_sections_json(project)?);
        Ok(format!(
            "AUTHORITATIVE CLINICAL STUDY MODEL:\n{}\n\nDETERMINISTIC CLINICAL ASSESSMENT:\n{}",
            serde_json::to_string_pretty(&study)?,
            serde_json::to_string_pretty(&assessment)?
        ))
    }

    pub fn clinical_assessment_json(&self, project: &str) -> Result<Value> {
        let Some(study) = self.clinical_study_typed(project)? else {
            return Ok(
                json!({"exists":false,"errors":[{"code":"missing_clinical_study","message":"Clinical study model has not been configured"}],"warnings":[],"cross_section_consistency":{"count":0,"conflicts":[]}}),
            );
        };
        let mut assessment =
            crate::clinical::assess(&study, &self.approved_sections_json(project)?);
        if let Some(obj) = assessment.as_object_mut() {
            obj.insert("exists".into(), Value::Bool(true));
        }
        Ok(assessment)
    }

    pub fn save_design_profile(&self, project: &str, profile: &Value) -> Result<Value> {
        // Serialize once so the same exact bytes are hashed and persisted for snapshot reproducibility.
        let bytes = serde_json::to_vec(profile)?;
        let sha = sha256_hex(&bytes);
        let json = String::from_utf8(bytes)?;
        let c = self.conn()?;
        let exists: i64 = c.query_row(
            "SELECT COUNT(*) FROM projects WHERE id=?1",
            [project],
            |r| r.get(0),
        )?;
        if exists != 1 {
            bail!("project not found");
        }
        c.execute(r#"INSERT INTO project_design(project_id,profile_json,content_sha256,updated_at)
          VALUES(?1,?2,?3,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET profile_json=excluded.profile_json,content_sha256=excluded.content_sha256,updated_at=CURRENT_TIMESTAMP"#,
          params![project,json,sha])?;
        Self::touch_project_conn(&c, project)?;
        Ok(json!({"profile":profile,"sha256":sha}))
    }

    pub fn design_profile_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row("SELECT profile_json,content_sha256,updated_at FROM project_design WHERE project_id=?1",[project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
        if let Some((profile, sha, updated_at)) = row {
            let parsed = serde_json::from_str::<Value>(&profile)
                .context("stored design profile is invalid JSON")?;
            Ok(json!({"profile":parsed,"sha256":sha,"updated_at":updated_at}))
        } else {
            Ok(json!({"profile":null,"sha256":null,"updated_at":null}))
        }
    }

    pub fn workflow_status_json(&self, project: &str) -> Result<Value> {
        let config = self.workflow_config(project)?;
        let c = self.conn()?;
        let documents: i64 = c.query_row(
            "SELECT COUNT(*) FROM documents WHERE project_id=?1",
            [project],
            |r| r.get(0),
        )?;
        let requirements = self.requirements_all_approved(project)?;
        let sections = self.all_required_sections_approved(project)?;
        let configured_sections = self
            .project_sections_json(project)?
            .as_array()
            .map(|x| !x.is_empty())
            .unwrap_or(false);
        let proposal_draft_ready:i64=c.query_row(r#"SELECT COUNT(*) FROM project_sections ps WHERE ps.project_id=?1 AND ps.required=1
          AND EXISTS(SELECT 1 FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key)"#,[project],|r|r.get(0))?;
        let required_section_count: i64 = c.query_row(
            "SELECT COUNT(*) FROM project_sections WHERE project_id=?1 AND required=1",
            [project],
            |r| r.get(0),
        )?;
        drop(c);

        let evaluate = |definition: &WorkflowStepDefinition| -> Result<(bool, bool)> {
            let artifact = if let Some(kind) = definition.artifact_type.as_deref() {
                Some(self.workflow_artifact_state(project, kind)?)
            } else {
                None
            };
            let artifact_approved = artifact
                .as_ref()
                .and_then(|v| v.get("approved"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let artifact_fresh = if artifact_approved {
                if let Some(kind) = definition.artifact_type.as_deref() {
                    self.workflow_artifact_is_fresh(project, kind)?
                } else {
                    true
                }
            } else {
                false
            };
            let artifact_started = artifact
                .as_ref()
                .and_then(|v| v.get("version"))
                .is_some_and(|v| !v.is_null());
            let result = match definition.completion_evaluator.as_str() {
                "solicitation_approved" => (
                    artifact_approved && artifact_fresh && requirements,
                    artifact_started || documents > 0,
                ),
                "artifact_approved" => (artifact_approved && artifact_fresh, artifact_started),
                "required_sections_approved" => (sections, configured_sections),
                "investigator_interview_complete" => {
                    let generated=self.interview_generated(project)?;
                    (generated&&self.interview_open_count(project)?==0,generated)
                }
                "clinical_design_ready" => {
                    let clinical=self.clinical_assessment_json(project)?;
                    let exists=clinical.get("exists").and_then(Value::as_bool).unwrap_or(false);
                    let complete=exists&&clinical.get("errors").and_then(Value::as_array).is_some_and(|errors|errors.is_empty());
                    (complete,exists)
                }
                "competitive_intelligence_ready" => {
                    let state=self.competitive_latest_json(project)?;
                    let exists=state.get("exists").and_then(Value::as_bool).unwrap_or(false);
                    let complete=exists&&self.competitive_ready(project)?&&self.competitive_pending_update_count(project)?==0&&self.competitive_text_refresh_pending_count(project)?==0;
                    (complete,exists)
                }
                "sponsor_compliance_ready" => {
                    let profile=self.compliance_profile_json(project)?;
                    let exists=profile.get("exists").and_then(Value::as_bool).unwrap_or(false);
                    let complete=exists&&self.compliance_assessment_json(project)?.get("ready").and_then(Value::as_bool).unwrap_or(false);
                    (complete,exists)
                }
                "collaboration_routing_complete" => (artifact_approved, artifact_started),
                "view_only" => (true, true),
                other => bail!("unsupported completion evaluator at runtime: {other}"),
            };
            Ok(result)
        };

        let mut steps = Vec::new();
        let mut blockers = Vec::new();
        let mut completed_keys = BTreeSet::new();
        for definition in &self.workflow_registry.core_steps {
            let (complete, started) = evaluate(definition)?;
            let prerequisites_complete = definition
                .prerequisites
                .iter()
                .all(|key| completed_keys.contains(key));
            let effective_complete = complete && prerequisites_complete;
            let status = if effective_complete {
                "complete"
            } else if !prerequisites_complete {
                "blocked"
            } else if started {
                "awaiting_review"
            } else {
                "available"
            };
            if effective_complete {
                completed_keys.insert(definition.key.clone());
            }
            steps.push(json!({"key":definition.key,"title":definition.title,"description":definition.description,"output":definition.output,"ui_surface":definition.ui_surface,"required":true,"enabled":true,"status":status}));
            if !effective_complete {
                blockers.push(json!({"code":format!("core_{}_incomplete",definition.key),"step":definition.key,"message":format!("{} is incomplete",definition.title),"remediation":format!("Open the {} step and approve its current output.",definition.title)}));
            }
        }

        let proposal_draft_complete =
            required_section_count > 0 && proposal_draft_ready == required_section_count;
        for module_key in &config.enabled_modules {
            let module = self.workflow_registry.module(module_key).with_context(|| {
                format!("enabled workflow module definition not found: {module_key}")
            })?;
            let (complete, started) = evaluate(&module.step)?;
            let prerequisites_complete = module.step.prerequisites.iter().all(|key| {
                if key == "proposal_draft" {
                    proposal_draft_complete
                } else {
                    completed_keys.contains(key)
                }
            });
            let required = config.required(&self.workflow_registry, module_key);
            let effective_complete = complete && prerequisites_complete;
            let status = if effective_complete {
                "complete"
            } else if !prerequisites_complete {
                "blocked"
            } else if started {
                "awaiting_review"
            } else {
                "available"
            };
            steps.push(json!({"key":module.step.key,"title":module.step.title,"description":module.step.description,"output":module.step.output,"placement":module.step.placement,"ui_surface":module.step.ui_surface,"runtime_implication":module.runtime_implication,"required":required,"enabled":true,"status":status,"optional":true}));
            if required && !effective_complete {
                blockers.push(json!({"code":format!("module_{}_incomplete",module.step.key),"module":module.step.key,"message":format!("Required module '{}' is incomplete",module.step.title),"remediation":format!("Complete '{}' or change it from required to advisory through an audited workflow update.",module.step.title)}));
            }
        }
        let ready = blockers.is_empty();
        Ok(
            json!({"ready":ready,"definition_version":self.workflow_registry.definition_version,"definition_sha256":self.workflow_registry.definition_sha256()?,"config":config,"steps":steps,"blockers":blockers,"events":self.workflow_events_json(project,25)?}),
        )
    }

    pub fn workflow_json(&self, project: &str) -> Result<Value> {
        let status = self.workflow_status_json(project)?;
        Ok(
            json!({"definitions":self.workflow_registry.as_json()?,"config":self.workflow_config(project)?,"status":status}),
        )
    }

    pub fn project_health_json(&self,project:&str)->Result<Value>{
        let workflow=self.workflow_status_json(project)?;
        let config=self.workflow_config(project)?;
        let tasks=self.tasks_json(project)?;
        let routing=self.approval_routing_status_json(project)?;
        let c=self.conn()?;
        let now_julian:f64=c.query_row("SELECT julianday('now')",[],|row|row.get(0))?;
        let mut issues=Vec::new();

        for step in workflow.get("steps").and_then(Value::as_array).into_iter().flatten(){
            let status=step.get("status").and_then(Value::as_str).unwrap_or_default();
            if status=="blocked"{
                issues.push(json!({
                    "severity":"high","kind":"blocked_gate","code":format!("blocked_gate_{}",step.get("key").and_then(Value::as_str).unwrap_or("unknown")),
                    "title":format!("{} is blocked",step.get("title").and_then(Value::as_str).unwrap_or("Workflow step")),
                    "detail":"One or more prerequisite workflow steps are incomplete.","step_key":step.get("key"),
                    "owner_user_id":null,"due_at":null,"remediation":format!("Complete the prerequisites, then reopen {}.",step.get("title").and_then(Value::as_str).unwrap_or("this step"))
                }));
            }else if status=="awaiting_review"{
                issues.push(json!({
                    "severity":"medium","kind":"pending_review","code":format!("pending_review_{}",step.get("key").and_then(Value::as_str).unwrap_or("unknown")),
                    "title":format!("{} is awaiting review",step.get("title").and_then(Value::as_str).unwrap_or("Workflow step")),
                    "detail":"Current work exists but the step has not reached its configured completion gate.","step_key":step.get("key"),
                    "owner_user_id":null,"due_at":null,"remediation":format!("Review and approve the current output for {}.",step.get("title").and_then(Value::as_str).unwrap_or("this step"))
                }));
            }
        }

        let artifact_definitions=self.workflow_registry.core_steps.iter()
            .chain(self.workflow_registry.optional_modules.iter().filter(|module|config.enabled(&module.step.key)).map(|module|&module.step))
            .filter(|step|matches!(step.completion_evaluator.as_str(),"artifact_approved"|"solicitation_approved"|"collaboration_routing_complete"));
        for definition in artifact_definitions{
            let Some(artifact_type)=definition.artifact_type.as_deref() else{continue;};
            let artifact=self.workflow_artifact_json(project,artifact_type)?;
            if artifact.get("version").and_then(Value::as_i64).is_some()
                && artifact.get("approved").and_then(Value::as_bool).unwrap_or(false)
                && !self.workflow_artifact_is_fresh(project,artifact_type)?{
                issues.push(json!({
                    "severity":"high","kind":"stale_artifact","code":format!("stale_artifact_{artifact_type}"),
                    "title":format!("{} is stale",definition.title),
                    "detail":"An approved upstream version changed after this artifact was approved.","step_key":definition.key,
                    "owner_user_id":null,"due_at":null,"remediation":"Regenerate or correct the artifact against the current approved inputs, then approve the new exact version."
                }));
            }
        }

        for task in tasks.as_array().into_iter().flatten(){
            let status=task.get("status").and_then(Value::as_str).unwrap_or_default();
            if matches!(status,"complete"|"cancelled"){continue;}
            let priority=task.get("priority").and_then(Value::as_str).unwrap_or("normal");
            let title=task.get("title").and_then(Value::as_str).unwrap_or("Project task");
            let owner=task.get("owner_user_id").cloned().unwrap_or(Value::Null);
            let due=task.get("due_at").and_then(Value::as_str);
            if status=="blocked"{
                issues.push(json!({"severity":if priority=="critical"{"critical"}else{"high"},"kind":"blocked_task","code":format!("blocked_task_{}",task.get("id").and_then(Value::as_str).unwrap_or("unknown")),"title":format!("Blocked task: {title}"),"detail":task.get("description"),"step_key":null,"owner_user_id":owner,"due_at":due,"remediation":"Resolve the blocking dependency or reassign the task."}));
            }
            if let Some(due)=due{
                let due_julian:Option<f64>=c.query_row("SELECT julianday(?1)",[due],|row|row.get(0))?;
                match due_julian{
                    Some(value) if value<now_julian=>issues.push(json!({"severity":if priority=="critical"{"critical"}else{"high"},"kind":"overdue_task","code":format!("overdue_task_{}",task.get("id").and_then(Value::as_str).unwrap_or("unknown")),"title":format!("Overdue task: {title}"),"detail":task.get("description"),"step_key":null,"owner_user_id":owner,"due_at":due,"remediation":"Complete, reassign, or set a defensible new due date."})),
                    None=>issues.push(json!({"severity":"medium","kind":"invalid_task_date","code":format!("invalid_task_date_{}",task.get("id").and_then(Value::as_str).unwrap_or("unknown")),"title":format!("Task has an invalid due date: {title}"),"detail":format!("The stored value '{due}' is not an ISO-8601 date or timestamp."),"step_key":null,"owner_user_id":owner,"due_at":due,"remediation":"Replace the due date with an ISO-8601 date or timestamp."})),
                    _=>{}
                }
            }
        }

        for route in routing.get("routes").and_then(Value::as_array).into_iter().flatten(){
            if route.get("current_version").and_then(Value::as_i64).is_some()&&!route.get("approved").and_then(Value::as_bool).unwrap_or(false){
                let approvals=route.get("approvals").and_then(Value::as_i64).unwrap_or(0);
                let required=route.get("minimum_approvals").and_then(Value::as_i64).unwrap_or(1);
                issues.push(json!({"severity":"medium","kind":"pending_approval","code":format!("pending_approval_{}",route.get("artifact_type").and_then(Value::as_str).unwrap_or("unknown")),"title":format!("Approval pending: {}",route.get("title").and_then(Value::as_str).unwrap_or("workflow artifact")),"detail":format!("{approvals} of {required} configured approvals are recorded."),"step_key":null,"owner_user_id":route.get("owner_user_id"),"due_at":null,"remediation":"A configured approver must review the exact current version."}));
            }
        }

        let open_comments:i64=c.query_row("SELECT COUNT(*) FROM comments WHERE project_id=?1 AND resolved_at IS NULL",[project],|row|row.get(0))?;
        if open_comments>0{
            issues.push(json!({"severity":"medium","kind":"open_comments","code":"open_version_comments","title":format!("{open_comments} unresolved version comment(s)"),"detail":"Comments remain open on immutable artifact or section versions.","step_key":null,"owner_user_id":null,"due_at":null,"remediation":"Address or explicitly resolve each comment in its version thread."}));
        }

        let literature=self.workflow_artifact_json(project,"literature_manifest")?;
        if let Some(body)=literature.get("body").filter(|value|value.is_object()){
            let unresolved=body.get("evidence_needs").and_then(Value::as_array).map(|items|items.iter().filter(|item|item.get("disposition").and_then(Value::as_str)==Some("unresolved_risk")).count()).unwrap_or(0);
            let contradictions=body.get("contradictions").and_then(Value::as_array).map_or(0,Vec::len);
            if unresolved>0{issues.push(json!({"severity":"high","kind":"evidence_risk","code":"unresolved_evidence_risks","title":format!("{unresolved} unresolved evidence risk(s)"),"detail":"The literature manifest records evidence needs that are not supported or waived.","step_key":"literature","owner_user_id":null,"due_at":null,"remediation":"Add grounded evidence, record a justified waiver, or retain the item as an explicit proposal risk."}));}
            if contradictions>0{issues.push(json!({"severity":"medium","kind":"evidence_contradiction","code":"literature_contradictions","title":format!("{contradictions} evidence contradiction(s) require review"),"detail":"The literature manifest contains conflicting findings that must remain visible in drafting.","step_key":"literature","owner_user_id":null,"due_at":null,"remediation":"Resolve the interpretation or document how the proposal handles the contradiction."}));}
        }

        if let Some(deadline)=config.target_deadline.as_deref().filter(|value|!value.trim().is_empty()){
            let deadline_julian:Option<f64>=c.query_row("SELECT julianday(?1)",[deadline],|row|row.get(0))?;
            match deadline_julian{
                Some(value) if !workflow.get("ready").and_then(Value::as_bool).unwrap_or(false)&&value<now_julian=>issues.push(json!({"severity":"critical","kind":"submission_deadline","code":"submission_deadline_passed","title":"Target submission deadline has passed","detail":format!("The configured target deadline was {deadline}."),"step_key":null,"owner_user_id":null,"due_at":deadline,"remediation":"Project leadership must set a valid revised target or close the project."})),
                Some(value) if !workflow.get("ready").and_then(Value::as_bool).unwrap_or(false)&&value-now_julian<=7.0=>issues.push(json!({"severity":"high","kind":"submission_deadline","code":"submission_deadline_near","title":"Submission deadline is within seven days","detail":format!("The configured target deadline is {deadline} and required gates remain open."),"step_key":null,"owner_user_id":null,"due_at":deadline,"remediation":"Prioritize blocking gates, overdue tasks, and pending approvals."})),
                None=>issues.push(json!({"severity":"medium","kind":"invalid_submission_deadline","code":"invalid_submission_deadline","title":"Configured submission deadline is invalid","detail":format!("The stored value '{deadline}' is not an ISO-8601 date or timestamp."),"step_key":null,"owner_user_id":null,"due_at":deadline,"remediation":"Update the project workflow with a valid ISO-8601 deadline."})),
                _=>{}
            }
        }

        let count=|severity:&str|issues.iter().filter(|item|item.get("severity").and_then(Value::as_str)==Some(severity)).count();
        let critical=count("critical");let high=count("high");let medium=count("medium");
        let state=if workflow.get("ready").and_then(Value::as_bool).unwrap_or(false)&&critical==0&&high==0{"ready"}else if critical>0{"critical"}else if high>0||medium>0{"at_risk"}else{"on_track"};
        Ok(json!({"state":state,"ready":workflow.get("ready"),"summary":{"critical":critical,"high":high,"medium":medium,"total":issues.len()},"issues":issues,"workflow":workflow,"generated_at_unix":time::OffsetDateTime::now_utc().unix_timestamp()}))
    }

    pub fn proposed_reviewer_roles_json(&self, project: &str) -> Result<Value> {
        let profile = self.workflow_artifact_json(project, "solicitation_profile")?;
        if !profile.get("approved").and_then(Value::as_bool).unwrap_or(false) {
            bail!("approve the current solicitation profile before deriving reviewer roles");
        }
        let body = profile.get("body").context("approved solicitation profile has no body")?;
        let criteria = body
            .get("review_criteria")
            .and_then(Value::as_array)
            .context("approved solicitation profile has no review_criteria array")?;
        if criteria.is_empty() {
            bail!("approved solicitation profile must contain at least one review criterion");
        }
        let grant_type = self.workflow_config(project)?.grant_type.unwrap_or_default();
        let criterion_text = criteria
            .iter()
            .map(|criterion| {
                format!(
                    "{} {}",
                    criterion.get("title").and_then(Value::as_str).unwrap_or_default(),
                    criterion.get("description").and_then(Value::as_str).unwrap_or_default()
                )
                .to_ascii_lowercase()
            })
            .collect::<Vec<_>>();
        let mut roles = Vec::new();
        for definition in &self.workflow_registry.reviewer_archetypes {
            let mut matched_criteria = criteria
                .iter()
                .zip(&criterion_text)
                .filter(|(_, text)| {
                    definition.criterion_terms.iter().any(|term| {
                        let normalized = term.trim().to_ascii_lowercase();
                        !normalized.is_empty() && text.contains(&normalized)
                    })
                })
                .filter_map(|(criterion, _)| criterion.get("id").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let grant_type_match = definition.grant_types.iter().any(|item| item == &grant_type);
            if definition.always_include || grant_type_match || !matched_criteria.is_empty() {
                if matched_criteria.is_empty() {
                    matched_criteria=criteria.iter().filter_map(|criterion|criterion.get("id").and_then(Value::as_str)).map(str::to_owned).collect();
                }
                roles.push(json!({
                    "key":definition.key,
                    "title":definition.title,
                    "description":definition.description,
                    "criterion_ids":matched_criteria,
                    "derived_from_grant_type":grant_type_match,
                    "always_include":definition.always_include
                }));
            }
        }
        Ok(json!({
            "solicitation_profile_version":profile.get("version"),
            "grant_type":grant_type,
            "roles":roles,
            "synthetic_review_notice":crate::workflow_artifacts::SYNTHETIC_REVIEW_NOTICE
        }))
    }

    pub fn create_review_panel_plan(&self,project:&str,mode:&str,actor:&str)->Result<Value>{
        let proposal=self.proposed_reviewer_roles_json(project)?;
        let profile_version=proposal.get("solicitation_profile_version").and_then(Value::as_i64).context("solicitation profile version missing")?;
        let body=json!({"schema_version":1,"solicitation_profile_version":profile_version,"registry_definition_version":self.workflow_registry.definition_version,"mode":mode,"roles":proposal.get("roles").cloned().unwrap_or(json!([])),"synthetic_review_notice":proposal.get("synthetic_review_notice")});
        let plan:crate::workflow_artifacts::ReviewerPanelPlan=serde_json::from_value(body.clone())?;
        crate::workflow_artifacts::validate_panel_plan(&plan)?;
        let id=Uuid::new_v4().to_string();let c=self.conn()?;
        c.execute("INSERT INTO review_panel_plans(id,project_id,solicitation_profile_version,registry_definition_version,mode,roles_json,synthetic_review_notice,status,created_by_user_id) VALUES(?1,?2,?3,?4,?5,?6,?7,'draft',?8)",params![id,project,profile_version,self.workflow_registry.definition_version,mode,serde_json::to_string(&plan.roles)?,plan.synthetic_review_notice,actor])?;
        self.review_panel_plan_json(project,&id)
    }

    pub fn approve_review_panel_plan(&self,project:&str,plan_id:&str,actor:&str)->Result<Value>{
        let profile=self.workflow_artifact_json(project,"solicitation_profile")?;let current=profile.get("version").and_then(Value::as_i64).context("approved solicitation profile is missing")?;if !profile.get("approved").and_then(Value::as_bool).unwrap_or(false){bail!("solicitation profile is not approved");}
        let c=self.conn()?;let changed=c.execute("UPDATE review_panel_plans SET status='approved',approved_by_user_id=?1,approved_at=CURRENT_TIMESTAMP WHERE id=?2 AND project_id=?3 AND status='draft' AND solicitation_profile_version=?4",params![actor,plan_id,project,current])?;if changed!=1{bail!("draft panel plan is missing or stale relative to the approved solicitation");}self.review_panel_plan_json(project,plan_id)
    }

    pub fn review_panel_plan_json(&self,project:&str,plan_id:&str)->Result<Value>{
        let c=self.conn()?;c.query_row("SELECT id,solicitation_profile_version,registry_definition_version,mode,roles_json,synthetic_review_notice,status,created_by_user_id,approved_by_user_id,created_at,approved_at FROM review_panel_plans WHERE id=?1 AND project_id=?2",params![plan_id,project],|r|Ok(json!({"schema_version":1,"id":r.get::<_,String>(0)?,"solicitation_profile_version":r.get::<_,i64>(1)?,"registry_definition_version":r.get::<_,i64>(2)?,"mode":r.get::<_,String>(3)?,"roles":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"synthetic_review_notice":r.get::<_,String>(5)?,"status":r.get::<_,String>(6)?,"created_by_user_id":r.get::<_,String>(7)?,"approved_by_user_id":r.get::<_,Option<String>>(8)?,"created_at":r.get::<_,String>(9)?,"approved_at":r.get::<_,Option<String>>(10)?}))).context("review panel plan not found")
    }

    pub fn create_review_snapshot(&self,project:&str,actor:&str)->Result<Value>{
        if !self.all_required_sections_approved(project)?{bail!("all required proposal sections must be approved before review simulation");}
        for artifact_type in ["solicitation_profile","research_framework","aim_set","literature_manifest"]{if !self.workflow_artifact_is_fresh(project,artifact_type)?{bail!("approved and fresh {artifact_type} is required before review simulation");}}
        let mut sections=self.approved_sections_json(project)?;
        if let Some(items)=sections.as_array_mut(){for item in items{let key=item.get("section_key").and_then(Value::as_str).unwrap_or_default();let version=item.get("version").and_then(Value::as_i64).unwrap_or_default();item["anchor_id"]=json!(format!("section:{key}:v{version}"));item["content_sha256"]=json!(sha256_hex(item.get("body").and_then(Value::as_str).unwrap_or_default().as_bytes()));}}
        let snapshot=json!({"schema_version":1,"project":self.project_json(project)?,"workflow_configuration":self.workflow_config_record_json(project)?,"solicitation_profile":self.workflow_artifact_json(project,"solicitation_profile")?,"research_framework":self.workflow_artifact_json(project,"research_framework")?,"aim_set":self.workflow_artifact_json(project,"aim_set")?,"literature_manifest":self.workflow_artifact_json(project,"literature_manifest")?,"sections":sections});
        let raw=serde_json::to_string(&snapshot)?;let sha=sha256_hex(raw.as_bytes());let id=Uuid::new_v4().to_string();self.conn()?.execute("INSERT INTO proposal_review_snapshots(id,project_id,snapshot_json,content_sha256,created_by_user_id) VALUES(?1,?2,?3,?4,?5)",params![id,project,raw,sha,actor])?;Ok(json!({"id":id,"sha256":sha,"snapshot":snapshot}))
    }

    pub fn begin_review_run(&self,project:&str,snapshot_id:&str,plan_id:&str,actor:&str)->Result<Value>{
        let c=self.conn()?;let(status,profile_version):(String,i64)=c.query_row("SELECT status,solicitation_profile_version FROM review_panel_plans WHERE id=?1 AND project_id=?2",params![plan_id,project],|r|Ok((r.get(0)?,r.get(1)?))).context("panel plan not found")?;if status!="approved"{bail!("panel plan must be approved before simulation");}
        let snapshot_exists:i64=c.query_row("SELECT COUNT(*) FROM proposal_review_snapshots WHERE id=?1 AND project_id=?2",params![snapshot_id,project],|r|r.get(0))?;if snapshot_exists!=1{bail!("review snapshot does not belong to project");}
        let run_id=Uuid::new_v4().to_string();let rubric_version_id=format!("solicitation-profile-v{profile_version}");c.execute("INSERT INTO review_simulation_runs(id,project_id,snapshot_id,panel_plan_id,rubric_version_id,status,created_by_user_id) VALUES(?1,?2,?3,?4,?5,'running',?6)",params![run_id,project,snapshot_id,plan_id,rubric_version_id,actor])?;Ok(json!({"id":run_id,"rubric_version_id":rubric_version_id}))
    }

    pub fn review_execution_inputs(&self,project:&str,run_id:&str)->Result<Value>{
        let c=self.conn()?;let(snapshot_raw,plan_id,rubric,status):(String,String,String,String)=c.query_row("SELECT s.snapshot_json,r.panel_plan_id,r.rubric_version_id,r.status FROM review_simulation_runs r JOIN proposal_review_snapshots s ON s.id=r.snapshot_id WHERE r.id=?1 AND r.project_id=?2",params![run_id,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).context("review run not found")?;if status!="running"{bail!("review run is not executable");}Ok(json!({"snapshot":serde_json::from_str::<Value>(&snapshot_raw)?,"panel_plan":self.review_panel_plan_json(project,&plan_id)?,"rubric_version_id":rubric}))
    }

    pub fn finish_review_run(&self,project:&str,run_id:&str,result:&crate::workflow_artifacts::ReviewSimulationResult)->Result<Value>{
        crate::workflow_artifacts::validate_review_result(result,false)?;let raw=serde_json::to_string(result)?;let sha=sha256_hex(raw.as_bytes());let mut c=self.conn()?;let tx=c.transaction()?;
        let(run_snapshot,run_plan,status):(String,String,String)=tx.query_row("SELECT snapshot_id,panel_plan_id,status FROM review_simulation_runs WHERE id=?1 AND project_id=?2",params![run_id,project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).context("review run not found")?;if status!="running"||run_snapshot!=result.snapshot_id||run_plan!=result.panel_plan_id{bail!("review result does not match its immutable run inputs");}
        let current:i64=tx.query_row("SELECT COALESCE(MAX(version),0) FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='review_simulation'",[project],|r|r.get(0))?;let artifact_version=current+1;
        tx.execute("UPDATE review_simulation_runs SET status='complete',result_json=?1,result_sha256=?2,completed_at=CURRENT_TIMESTAMP WHERE id=?3",params![raw,sha,run_id])?;
        tx.execute("INSERT INTO workflow_artifacts(project_id,artifact_type,version,body_json,content_sha256,source) VALUES(?1,'review_simulation',?2,?3,?4,'review_pipeline')",params![project,artifact_version,raw,sha])?;
        if let Some(causal)=&result.causal_analysis{let body=serde_json::to_string(causal)?;let causal_sha=sha256_hex(body.as_bytes());tx.execute("INSERT INTO causal_models(project_id,review_run_id,version,body_json,content_sha256,author_user_id) VALUES(?1,?2,1,?3,?4,'model-inferred')",params![project,run_id,body,causal_sha])?;}
        tx.execute("INSERT INTO workflow_events(project_id,event_type,payload_json) VALUES(?1,'review_simulation_completed',?2)",params![project,serde_json::to_string(&json!({"run_id":run_id,"artifact_version":artifact_version,"sha256":sha}))?])?;Self::touch_project_conn(&tx,project)?;tx.commit()?;self.review_run_json(project,run_id)
    }

    pub fn fail_review_run(&self,project:&str,run_id:&str,error:&str)->Result<()> {self.conn()?.execute("UPDATE review_simulation_runs SET status='failed',error=?1,completed_at=CURRENT_TIMESTAMP WHERE id=?2 AND project_id=?3 AND status='running'",params![error.chars().take(4000).collect::<String>(),run_id,project])?;Ok(())}
    pub fn review_run_json(&self,project:&str,run_id:&str)->Result<Value>{let c=self.conn()?;c.query_row("SELECT id,snapshot_id,panel_plan_id,rubric_version_id,status,result_json,result_sha256,error,created_by_user_id,created_at,completed_at FROM review_simulation_runs WHERE id=?1 AND project_id=?2",params![run_id,project],|r|{let raw=r.get::<_,Option<String>>(5)?;Ok(json!({"id":r.get::<_,String>(0)?,"snapshot_id":r.get::<_,String>(1)?,"panel_plan_id":r.get::<_,String>(2)?,"rubric_version_id":r.get::<_,String>(3)?,"status":r.get::<_,String>(4)?,"result":raw.and_then(|value|serde_json::from_str::<Value>(&value).ok()),"result_sha256":r.get::<_,Option<String>>(6)?,"error":r.get::<_,Option<String>>(7)?,"created_by_user_id":r.get::<_,String>(8)?,"created_at":r.get::<_,String>(9)?,"completed_at":r.get::<_,Option<String>>(10)?}))}).context("review run not found")}

    pub fn approve_review_run(&self,project:&str,run_id:&str,approver:&str)->Result<Value>{
        let c=self.conn()?;
        let (status,result_sha):(String,Option<String>)=c.query_row(
            "SELECT status,result_sha256 FROM review_simulation_runs WHERE id=?1 AND project_id=?2",
            params![run_id,project],
            |row|Ok((row.get(0)?,row.get(1)?)),
        ).context("review run not found")?;
        if status!="complete"{bail!("only a completed review simulation can be approved");}
        let result_sha=result_sha.context("completed review simulation is missing its immutable result hash")?;
        let version:i64=c.query_row(
            "SELECT version FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='review_simulation' AND content_sha256=?2 ORDER BY version DESC LIMIT 1",
            params![project,result_sha],
            |row|row.get(0),
        ).context("review simulation workflow artifact is missing")?;
        drop(c);
        let mut artifact=self.approve_workflow_artifact(project,"review_simulation",version,Some(approver))?;
        artifact["review_run_id"]=json!(run_id);
        Ok(artifact)
    }

    pub fn save_causal_model_version(&self,project:&str,run_id:&str,body:&Value,author:&str,confirmed:bool)->Result<Value>{
        let causal:crate::workflow_artifacts::CausalAnalysisResult=serde_json::from_value(body.clone())?;let wrapper=crate::workflow_artifacts::ReviewSimulationResult{schema_version:1,snapshot_id:"validation".into(),rubric_version_id:"validation".into(),panel_plan_id:"validation".into(),reviews:Vec::new(),causal_analysis:Some(causal),panel_summary:json!({}),revision_tasks:Vec::new(),synthetic_review_notice:"validation".into()};crate::workflow_artifacts::validate_review_result(&wrapper,false)?;
        let raw=serde_json::to_string(body)?;let sha=sha256_hex(raw.as_bytes());let c=self.conn()?;let version:i64=c.query_row("SELECT COALESCE(MAX(version),0)+1 FROM causal_models WHERE review_run_id=?1",[run_id],|r|r.get(0))?;c.execute("INSERT INTO causal_models(project_id,review_run_id,version,body_json,content_sha256,author_user_id,confirmed) SELECT ?1,?2,?3,?4,?5,?6,?7 WHERE EXISTS(SELECT 1 FROM review_simulation_runs WHERE id=?2 AND project_id=?1 AND status='complete')",params![project,run_id,version,raw,sha,author,confirmed as i64])?;Ok(json!({"review_run_id":run_id,"version":version,"body":body,"sha256":sha,"confirmed":confirmed,"author_user_id":author}))
    }

    pub fn causal_models_json(&self,project:&str,run_id:&str)->Result<Value>{let c=self.conn()?;let mut st=c.prepare("SELECT version,body_json,content_sha256,author_user_id,confirmed,created_at FROM causal_models WHERE project_id=?1 AND review_run_id=?2 ORDER BY version DESC")?;let rows=st.query_map(params![project,run_id],|r|Ok(json!({"version":r.get::<_,i64>(0)?,"body":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(json!({})),"sha256":r.get::<_,String>(2)?,"author_user_id":r.get::<_,String>(3)?,"confirmed":r.get::<_,i64>(4)?!=0,"created_at":r.get::<_,String>(5)?})))?;let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))}

    pub fn generation_runs_json(&self, project: &str, limit: usize) -> Result<Value> {
        let c = self.conn()?;
        let mut statement = c.prepare(
            r#"SELECT id,task_kind,routing_mode,provider,model,prompt_sha256,response_sha256,
                      input_manifest_sha256,output_contract_name,output_contract_version,output_schema_sha256,
                      high_value,status,error,started_at,completed_at
               FROM generation_runs WHERE project_id=?1 ORDER BY started_at DESC,id DESC LIMIT ?2"#,
        )?;
        let rows = statement.query_map(params![project, limit.clamp(1, 500) as i64], |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "task_kind": row.get::<_, String>(1)?,
                "routing_mode": row.get::<_, String>(2)?,
                "provider": row.get::<_, String>(3)?,
                "model": row.get::<_, String>(4)?,
                "prompt_sha256": row.get::<_, String>(5)?,
                "response_sha256": row.get::<_, Option<String>>(6)?,
                "input_manifest_sha256": row.get::<_, Option<String>>(7)?,
                "output_contract_name":row.get::<_,Option<String>>(8)?,
                "output_contract_version":row.get::<_,Option<i64>>(9)?,
                "output_schema_sha256":row.get::<_,Option<String>>(10)?,
                "high_value": row.get::<_, i64>(11)? != 0,
                "status": row.get::<_, String>(12)?,
                "error": row.get::<_, Option<String>>(13)?,
                "started_at": row.get::<_, String>(14)?,
                "completed_at": row.get::<_, Option<String>>(15)?
            }))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(Value::Array(result))
    }

    pub fn generation_run_json(&self,project:&str,run_id:&str)->Result<Value>{
        self.conn()?.query_row(r#"SELECT id,task_kind,routing_mode,provider,model,prompt_sha256,response_sha256,
          input_manifest_json,input_manifest_sha256,output_contract_name,output_contract_version,output_schema_json,output_schema_sha256,
          high_value,status,error,started_at,completed_at
          FROM generation_runs WHERE id=?1 AND project_id=?2"#,params![run_id,project],|row|{
            let manifest=row.get::<_,Option<String>>(7)?.and_then(|raw|serde_json::from_str::<Value>(&raw).ok());
            let schema=row.get::<_,Option<String>>(11)?.and_then(|raw|serde_json::from_str::<Value>(&raw).ok());
            Ok(json!({"id":row.get::<_,String>(0)?,"task_kind":row.get::<_,String>(1)?,"routing_mode":row.get::<_,String>(2)?,"provider":row.get::<_,String>(3)?,"model":row.get::<_,String>(4)?,"prompt_sha256":row.get::<_,String>(5)?,"response_sha256":row.get::<_,Option<String>>(6)?,"input_manifest":manifest,"input_manifest_sha256":row.get::<_,Option<String>>(8)?,"output_contract_name":row.get::<_,Option<String>>(9)?,"output_contract_version":row.get::<_,Option<i64>>(10)?,"output_schema":schema,"output_schema_sha256":row.get::<_,Option<String>>(12)?,"high_value":row.get::<_,i64>(13)?!=0,"status":row.get::<_,String>(14)?,"error":row.get::<_,Option<String>>(15)?,"started_at":row.get::<_,String>(16)?,"completed_at":row.get::<_,Option<String>>(17)?}))
          }).context("generation run does not belong to this project")
    }

    fn generation_input_manifest_conn(c:&Connection,project:&str)->Result<Value>{
        let workflow=c.query_row("SELECT definition_version,definition_sha256,config_version,config_sha256 FROM project_workflows WHERE project_id=?1",[project],|row|Ok(json!({"definition_version":row.get::<_,i64>(0)?,"definition_sha256":row.get::<_,Option<String>>(1)?,"config_version":row.get::<_,i64>(2)?,"config_sha256":row.get::<_,Option<String>>(3)?}))).context("project workflow is missing")?;
        let mut artifacts=Vec::new();
        {let mut statement=c.prepare(r#"SELECT artifact_type,id,version,content_sha256 FROM workflow_artifacts a WHERE project_id=?1 AND approved=1 AND version=(SELECT MAX(version) FROM workflow_artifacts b WHERE b.project_id=a.project_id AND b.artifact_type=a.artifact_type AND b.approved=1) ORDER BY artifact_type"#)?;for row in statement.query_map([project],|row|Ok(json!({"artifact_type":row.get::<_,String>(0)?,"id":row.get::<_,i64>(1)?,"version":row.get::<_,i64>(2)?,"sha256":row.get::<_,String>(3)?})))?{artifacts.push(row?);}}
        let mut documents=Vec::new();
        {let mut statement=c.prepare("SELECT id,kind,sha256 FROM documents WHERE project_id=?1 ORDER BY id")?;for row in statement.query_map([project],|row|Ok(json!({"id":row.get::<_,i64>(0)?,"kind":row.get::<_,String>(1)?,"sha256":row.get::<_,String>(2)?})))?{documents.push(row?);}}
        let mut requirements=Vec::new();
        {let mut statement=c.prepare("SELECT id,external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,status,approved FROM requirements WHERE project_id=?1 ORDER BY id")?;for row in statement.query_map([project],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,i64>(4)?,row.get::<_,String>(5)?,row.get::<_,String>(6)?,row.get::<_,String>(7)?,row.get::<_,i64>(8)?)))?{let(id,external_id,category,text,mandatory,evidence,dependencies,status,approved)=row?;let record=json!({"external_id":external_id,"category":category,"requirement":text,"mandatory":mandatory!=0,"evidence_needed":serde_json::from_str::<Value>(&evidence)?,"dependencies":serde_json::from_str::<Value>(&dependencies)?,"status":status,"approved":approved!=0});requirements.push(json!({"id":id,"sha256":sha256_hex(&serde_json::to_vec(&record)?)}));}}
        let mut evidence=Vec::new();
        {let mut statement=c.prepare("SELECT id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status FROM evidence WHERE project_id=?1 ORDER BY id")?;for row in statement.query_map([project],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?,row.get::<_,String>(5)?,row.get::<_,Option<String>>(6)?,row.get::<_,Option<String>>(7)?,row.get::<_,f64>(8)?,row.get::<_,String>(9)?)))?{let(id,requirement_id,source_type,source_ref,claim,passage,url,locator,confidence,status)=row?;let record=json!({"requirement_id":requirement_id,"source_type":source_type,"source_ref":source_ref,"claim":claim,"passage":passage,"url":url,"locator":locator,"confidence":confidence,"status":status});evidence.push(json!({"id":id,"sha256":sha256_hex(&serde_json::to_vec(&record)?)}));}}
        let mut citations=Vec::new();
        {let mut statement=c.prepare("SELECT id,evidence_id,content_sha256,verified FROM citations WHERE project_id=?1 ORDER BY id")?;for row in statement.query_map([project],|row|Ok(json!({"id":row.get::<_,i64>(0)?,"evidence_id":row.get::<_,i64>(1)?,"sha256":row.get::<_,String>(2)?,"verified":row.get::<_,i64>(3)?!=0})))?{citations.push(row?);}}
        let mut approved_sections=Vec::new();
        {let mut statement=c.prepare(r#"SELECT sv.id,sv.section_key,sv.body FROM section_versions sv WHERE sv.project_id=?1 AND sv.approved=1 AND sv.id=(SELECT MAX(x.id) FROM section_versions x WHERE x.project_id=sv.project_id AND x.section_key=sv.section_key AND x.approved=1) ORDER BY sv.section_key"#)?;for row in statement.query_map([project],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))?{let(id,key,body)=row?;approved_sections.push(json!({"version_id":id,"section_key":key,"sha256":sha256_hex(body.as_bytes())}));}}
        let clinical=c.query_row("SELECT version,content_sha256 FROM clinical_studies WHERE project_id=?1",[project],|row|Ok(json!({"version":row.get::<_,i64>(0)?,"sha256":row.get::<_,String>(1)?}))).optional()?;
        let competitive=c.query_row("SELECT version,content_sha256 FROM competitive_profiles WHERE project_id=?1",[project],|row|Ok(json!({"profile_version":row.get::<_,i64>(0)?,"profile_sha256":row.get::<_,String>(1)?}))).optional()?;
        let compliance=c.query_row("SELECT version,content_sha256,approved FROM compliance_profiles WHERE project_id=?1",[project],|row|Ok(json!({"version":row.get::<_,i64>(0)?,"sha256":row.get::<_,String>(1)?,"approved":row.get::<_,i64>(2)?!=0}))).optional()?;
        Ok(json!({"schema_version":1,"workflow":workflow,"approved_workflow_artifacts":artifacts,"documents":documents,"requirements":requirements,"evidence":evidence,"citations":citations,"approved_sections":approved_sections,"clinical_study":clinical,"competitive_profile":competitive,"compliance_profile":compliance}))
    }

    pub fn claim_idempotency(
        &self,
        user_id: &str,
        key: &str,
        method: &str,
        path: &str,
        request_sha256: &str,
    ) -> Result<IdempotencyClaim> {
        if key.len() < 8 || key.len() > 200 || key.chars().any(char::is_whitespace) {
            bail!("Idempotency-Key must be 8-200 non-whitespace characters");
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let existing = tx
            .query_row(
                "SELECT method,path,request_sha256,state,status_code,content_type,response_body FROM idempotency_keys WHERE user_id=?1 AND key=?2",
                params![user_id, key],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                )),
            )
            .optional()?;
        let claim = match existing {
            Some((stored_method, stored_path, stored_request_sha256, _, _, _, _))
                if stored_method != method
                    || stored_path != path
                    || stored_request_sha256.as_deref() != Some(request_sha256) => IdempotencyClaim::Conflict,
            Some((_, _, _, state, status, content_type, body)) if state == "complete" => {
                IdempotencyClaim::Replay {
                    status_code: status.unwrap_or(500).clamp(100, 599) as u16,
                    content_type: content_type.unwrap_or_else(|| "application/json".into()),
                    body: body.unwrap_or_default(),
                }
            }
            Some(_) => IdempotencyClaim::InProgress,
            None => {
                tx.execute(
                    "INSERT INTO idempotency_keys(user_id,key,method,path,request_sha256,state) VALUES(?1,?2,?3,?4,?5,'in_progress')",
                    params![user_id, key, method, path, request_sha256],
                )?;
                IdempotencyClaim::New
            }
        };
        tx.commit()?;
        Ok(claim)
    }

    pub fn complete_idempotency(
        &self,
        user_id: &str,
        key: &str,
        status_code: u16,
        content_type: &str,
        body: &[u8],
    ) -> Result<()> {
        let changed = self.conn()?.execute(
            "UPDATE idempotency_keys SET state='complete',status_code=?1,content_type=?2,response_body=?3,completed_at=CURRENT_TIMESTAMP WHERE user_id=?4 AND key=?5 AND state='in_progress'",
            params![status_code as i64, content_type, body, user_id, key],
        )?;
        if changed != 1 {
            bail!("idempotency claim is missing or already complete");
        }
        Ok(())
    }

    pub fn workflow_artifact_manifest_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare(r#"SELECT a.artifact_type,a.id,a.version,a.content_sha256,a.source,a.approved_by,a.approved_at
          FROM workflow_artifacts a WHERE a.project_id=?1 AND a.approved=1 AND a.version=(
            SELECT MAX(b.version) FROM workflow_artifacts b WHERE b.project_id=a.project_id AND b.artifact_type=a.artifact_type AND b.approved=1)
          ORDER BY a.artifact_type"#)?;
        let rows=st.query_map([project],|r|Ok(json!({"artifact_type":r.get::<_,String>(0)?,"id":r.get::<_,i64>(1)?,"version":r.get::<_,i64>(2)?,"sha256":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"approved_by":r.get::<_,Option<String>>(5)?,"approved_at":r.get::<_,Option<String>>(6)?})))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Value::Array(out))
    }

    pub fn readiness_json(&self, project: &str) -> Result<Value> {
        let workflow = self.workflow_status_json(project)?;
        let config = self.workflow_config(project)?;
        let requirements = self.requirements_all_approved(project)?;
        let sections = self.all_required_sections_approved(project)?;
        let ready = workflow
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut result=json!({"ready":ready,"stage":self.compatibility_stage(project)?,"workflow":workflow,"workflow_config":config,
          "requirements_approved":requirements,"required_sections_approved":sections});
        let object=result.as_object_mut().context("readiness construction failed")?;
        let enabled_evaluator=|name:&str|self.workflow_registry.optional_modules.iter()
            .any(|module|module.step.completion_evaluator==name&&config.enabled(&module.step.key));
        if enabled_evaluator("investigator_interview_complete"){
            object.insert("investigator_interview".into(),json!({"generated":self.interview_generated(project)?,"open_questions":self.interview_open_count(project)?}));
        }
        if enabled_evaluator("clinical_design_ready"){
            object.insert("clinical_design".into(),self.clinical_assessment_json(project)?);
        }
        if enabled_evaluator("competitive_intelligence_ready"){
            object.insert("competitive_intelligence".into(),self.competitive_latest_json(project)?);
            object.insert("competitive_updates_pending".into(),json!(self.competitive_pending_update_count(project)?));
            object.insert("competitive_refresh_processing_pending".into(),json!(self.competitive_text_refresh_pending_count(project)?));
        }
        if enabled_evaluator("sponsor_compliance_ready"){
            object.insert("sponsor_compliance".into(),self.compliance_assessment_json(project)?);
        }
        Ok(result)
    }

    pub fn create_export_snapshot(&self, project: &str) -> Result<Value> {
        let readiness = self.readiness_json(project)?;
        if !readiness
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!(
                "project is not ready for export: {}",
                serde_json::to_string(&readiness)?
            );
        }
        let sections = self.approved_sections_json(project)?;
        let project_meta = self.project_json(project)?;
        let workflow_config = self.workflow_config(project)?;
        let workflow_record = self.workflow_config_record_json(project)?;
        let workflow_artifacts = self.workflow_artifact_manifest_json(project)?;
        let mut snapshot = json!({"project":project_meta,"workflow_definition_version":self.workflow_registry.definition_version,
          "workflow_definition_sha256":self.workflow_registry.definition_sha256()?,"workflow_configuration":workflow_record,
          "workflow_artifacts":workflow_artifacts,"sections":sections});
        let object=snapshot.as_object_mut().context("export snapshot construction failed")?;
        if workflow_config.enabled("clinical_design"){
            object.insert("design_profile".into(),self.design_profile_json(project)?);
            object.insert("clinical_study".into(),self.clinical_study_json(project)?);
        }
        if workflow_config.enabled("competitive_intelligence"){
            object.insert("competitive_intelligence".into(),self.competitive_latest_json(project)?);
            object.insert("competitive_updates".into(),self.competitive_updates_json(project,25)?);
        }
        if workflow_config.enabled("sponsor_compliance"){
            object.insert("sponsor_compliance_profile".into(),self.compliance_profile_json(project)?);
            object.insert("sponsor_compliance_assessment".into(),self.compliance_assessment_json(project)?);
            object.insert("submission_artifacts".into(),self.submission_artifacts_json(project)?);
        }
        let bytes = serde_json::to_vec(&snapshot)?;
        let sha = sha256_hex(&bytes);
        let c = self.conn()?;
        c.execute("INSERT INTO export_snapshots(project_id,snapshot_json,content_sha256) VALUES(?1,?2,?3)",params![project,String::from_utf8(bytes)?,sha])?;
        let snapshot_id = c.last_insert_rowid();
        c.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        Ok(json!({"snapshot_id":snapshot_id,"sha256":sha,"snapshot":snapshot}))
    }

    pub fn portable_project_package(&self, project:&str)->Result<Value>{
        let c=self.conn()?;
        let project_meta=self.project_json(project)?;
        let workflow=self.workflow_config_record_json(project)?;
        let mut documents=Vec::new();
        {let mut st=c.prepare("SELECT id,name,kind,text,sha256,created_at FROM documents WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"name":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?,"sha256":r.get::<_,String>(4)?,"created_at":r.get::<_,String>(5)?})))?{documents.push(row?);}}
        let mut document_chunks=Vec::new();
        {let mut st=c.prepare("SELECT dc.id,dc.document_id,dc.ordinal,dc.start_word,dc.end_word,dc.text FROM document_chunks dc JOIN documents d ON d.id=dc.document_id WHERE d.project_id=?1 ORDER BY dc.id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"document_id":r.get::<_,i64>(1)?,"ordinal":r.get::<_,i64>(2)?,"start_word":r.get::<_,i64>(3)?,"end_word":r.get::<_,i64>(4)?,"text":r.get::<_,String>(5)?})))?{document_chunks.push(row?);}}
        let mut requirements=Vec::new();
        {let mut st=c.prepare("SELECT external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,source_clue,source_document,source_locator,status,approved,created_at FROM requirements WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"external_id":r.get::<_,String>(0)?,"category":r.get::<_,String>(1)?,"requirement":r.get::<_,String>(2)?,"mandatory":r.get::<_,i64>(3)?!=0,"evidence_needed":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"dependencies":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(json!([])),"source_clue":r.get::<_,Option<String>>(6)?,"source_document":r.get::<_,Option<String>>(7)?,"source_locator":r.get::<_,Option<String>>(8)?,"status":r.get::<_,String>(9)?,"approved":r.get::<_,i64>(10)?!=0,"created_at":r.get::<_,String>(11)?})))?{requirements.push(row?);}}
        let mut interview_questions=Vec::new();
        {let mut st=c.prepare("SELECT id,requirement_external_id,question,answer_type,choices_json,unit,why_needed,evidence_requested,priority,status,created_at FROM interview_questions WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_external_id":r.get::<_,String>(1)?,"question":r.get::<_,String>(2)?,"answer_type":r.get::<_,String>(3)?,"choices":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"unit":r.get::<_,Option<String>>(5)?,"why_needed":r.get::<_,Option<String>>(6)?,"evidence_requested":r.get::<_,i64>(7)?!=0,"priority":r.get::<_,i64>(8)?,"status":r.get::<_,String>(9)?,"created_at":r.get::<_,String>(10)?})))?{interview_questions.push(row?);}}
        let mut interview_answers=Vec::new();
        {let mut st=c.prepare("SELECT a.question_id,a.value_json,a.confidence,a.classification,a.notes,a.answered_by,a.created_at FROM interview_answers a JOIN interview_questions q ON q.id=a.question_id WHERE q.project_id=?1 ORDER BY a.id")?;for row in st.query_map([project],|r|Ok(json!({"question_id":r.get::<_,i64>(0)?,"value":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(Value::Null),"confidence":r.get::<_,String>(2)?,"classification":r.get::<_,String>(3)?,"notes":r.get::<_,Option<String>>(4)?,"answered_by":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,String>(6)?})))?{interview_answers.push(row?);}}
        let mut research_queries=Vec::new();
        {let mut st=c.prepare("SELECT id,requirement_external_id,query,preferred_domains_json,rationale,status,created_at FROM research_queries WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_external_id":r.get::<_,String>(1)?,"query":r.get::<_,String>(2)?,"preferred_domains":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(json!([])),"rationale":r.get::<_,Option<String>>(4)?,"status":r.get::<_,String>(5)?,"created_at":r.get::<_,String>(6)?})))?{research_queries.push(row?);}}
        let mut research_sources=Vec::new();
        {let mut st=c.prepare("SELECT id,query_id,title,url,text,retrieved_at,content_sha256,http_status FROM research_sources WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"query_id":r.get::<_,Option<i64>>(1)?,"title":r.get::<_,String>(2)?,"url":r.get::<_,String>(3)?,"text":r.get::<_,String>(4)?,"retrieved_at":r.get::<_,String>(5)?,"content_sha256":r.get::<_,String>(6)?,"http_status":r.get::<_,i64>(7)?})))?{research_sources.push(row?);}}
        let mut sections=Vec::new();
        {let mut st=c.prepare("SELECT section_key,title,position,required,origin,created_at FROM project_sections WHERE project_id=?1 ORDER BY position,section_key")?;for row in st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"required":r.get::<_,i64>(3)?!=0,"origin":r.get::<_,String>(4)?,"created_at":r.get::<_,String>(5)?})))?{sections.push(row?);}}
        let mut generation_runs=Vec::new();
        {let mut st=c.prepare("SELECT id,task_kind,routing_mode,provider,model,prompt_sha256,response_sha256,input_manifest_json,input_manifest_sha256,high_value,status,error,started_at,completed_at,output_contract_name,output_contract_version,output_schema_json,output_schema_sha256 FROM generation_runs WHERE project_id=?1 ORDER BY started_at,id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,String>(0)?,"task_kind":r.get::<_,String>(1)?,"routing_mode":r.get::<_,String>(2)?,"provider":r.get::<_,String>(3)?,"model":r.get::<_,String>(4)?,"prompt_sha256":r.get::<_,String>(5)?,"response_sha256":r.get::<_,Option<String>>(6)?,"input_manifest_json":r.get::<_,Option<String>>(7)?,"input_manifest_sha256":r.get::<_,Option<String>>(8)?,"high_value":r.get::<_,i64>(9)?!=0,"status":r.get::<_,String>(10)?,"error":r.get::<_,Option<String>>(11)?,"started_at":r.get::<_,String>(12)?,"completed_at":r.get::<_,Option<String>>(13)?,"output_contract_name":r.get::<_,Option<String>>(14)?,"output_contract_version":r.get::<_,Option<i64>>(15)?,"output_schema_json":r.get::<_,Option<String>>(16)?,"output_schema_sha256":r.get::<_,Option<String>>(17)?})))?{generation_runs.push(row?);}}
        if generation_runs.iter().any(|run|run.get("status").and_then(Value::as_str)==Some("running")){bail!("project has an active model generation; wait for it to finish before creating a portable package");}
        let mut section_versions=Vec::new();
        {let mut st=c.prepare("SELECT id,section_key,title,body,html,source,editor_name,author_user_id,approved,base_version_id,restored_from_version_id,generation_run_id,created_at FROM section_versions WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"section_key":r.get::<_,String>(1)?,"title":r.get::<_,String>(2)?,"body":r.get::<_,String>(3)?,"html":r.get::<_,Option<String>>(4)?,"source":r.get::<_,String>(5)?,"editor_name":r.get::<_,Option<String>>(6)?,"author_user_id":r.get::<_,Option<String>>(7)?,"approved":r.get::<_,i64>(8)?!=0,"base_version_id":r.get::<_,Option<i64>>(9)?,"restored_from_version_id":r.get::<_,Option<i64>>(10)?,"generation_run_id":r.get::<_,Option<String>>(11)?,"created_at":r.get::<_,String>(12)?})))?{section_versions.push(row?);}}
        let mut approvals=Vec::new();
        {let mut st=c.prepare("SELECT a.section_key,a.version_id,a.approved_by,a.approver_user_id,a.role_at_approval,a.decision,a.notes,a.approved_at FROM approvals a JOIN section_versions sv ON sv.id=a.version_id WHERE a.project_id=?1 ORDER BY a.id")?;for row in st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"version_id":r.get::<_,i64>(1)?,"approved_by":r.get::<_,Option<String>>(2)?,"approver_user_id":r.get::<_,Option<String>>(3)?,"role_at_approval":r.get::<_,Option<String>>(4)?,"decision":r.get::<_,String>(5)?,"notes":r.get::<_,Option<String>>(6)?,"approved_at":r.get::<_,String>(7)?})))?{approvals.push(row?);}}
        let mut artifacts=Vec::new();
        {let mut st=c.prepare("SELECT artifact_type,version,body_json,content_sha256,source,author,approved,approved_by,approved_at,created_at FROM workflow_artifacts WHERE project_id=?1 ORDER BY artifact_type,version")?;for row in st.query_map([project],|r|Ok(json!({"artifact_type":r.get::<_,String>(0)?,"version":r.get::<_,i64>(1)?,"body":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(json!({})),"content_sha256":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"author":r.get::<_,Option<String>>(5)?,"approved":r.get::<_,i64>(6)?!=0,"approved_by":r.get::<_,Option<String>>(7)?,"approved_at":r.get::<_,Option<String>>(8)?,"created_at":r.get::<_,String>(9)?})))?{artifacts.push(row?);}}
        let mut evidence=Vec::new();
        {let mut st=c.prepare("SELECT id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status,created_at FROM evidence WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_external_id":r.get::<_,Option<String>>(1)?,"source_type":r.get::<_,String>(2)?,"source_ref":r.get::<_,String>(3)?,"claim":r.get::<_,String>(4)?,"passage":r.get::<_,String>(5)?,"source_url":r.get::<_,Option<String>>(6)?,"source_locator":r.get::<_,Option<String>>(7)?,"confidence":r.get::<_,f64>(8)?,"status":r.get::<_,String>(9)?,"created_at":r.get::<_,String>(10)?})))?{evidence.push(row?);}}
        let mut citations=Vec::new();
        {let mut st=c.prepare("SELECT c.evidence_id,c.citation_key,c.title,c.url,c.passage,c.content_sha256,c.verified,c.created_at FROM citations c JOIN evidence e ON e.id=c.evidence_id WHERE e.project_id=?1 ORDER BY c.id")?;for row in st.query_map([project],|r|Ok(json!({"evidence_id":r.get::<_,i64>(0)?,"citation_key":r.get::<_,String>(1)?,"title":r.get::<_,String>(2)?,"url":r.get::<_,Option<String>>(3)?,"passage":r.get::<_,String>(4)?,"content_sha256":r.get::<_,String>(5)?,"verified":r.get::<_,i64>(6)?!=0,"created_at":r.get::<_,String>(7)?})))?{citations.push(row?);}}
        let design=c.query_row("SELECT profile_json,content_sha256,updated_at FROM project_design WHERE project_id=?1",[project],|r|Ok(json!({"profile":serde_json::from_str::<Value>(&r.get::<_,String>(0)?).unwrap_or(json!({})),"content_sha256":r.get::<_,String>(1)?,"updated_at":r.get::<_,String>(2)?}))).optional()?;
        let clinical_study=self.clinical_study_json(project)?;
        let competitive_intelligence=self.competitive_latest_json(project)?;
        let compliance_profile=self.compliance_profile_json(project)?;
        let mut compliance_sources=Vec::new();
        {let mut st=c.prepare("SELECT profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt FROM compliance_rule_sources WHERE project_id=?1 ORDER BY profile_version,rule_id")?;for row in st.query_map([project],|r|Ok(json!({"profile_version":r.get::<_,i64>(0)?,"rule_id":r.get::<_,String>(1)?,"source_status":r.get::<_,String>(2)?,"source_hint":r.get::<_,String>(3)?,"source_document_id":r.get::<_,Option<i64>>(4)?,"source_start_offset":r.get::<_,Option<i64>>(5)?,"source_end_offset":r.get::<_,Option<i64>>(6)?,"source_page":r.get::<_,Option<i64>>(7)?,"source_excerpt":r.get::<_,String>(8)?})))?{compliance_sources.push(row?);}}
        let mut compliance_resolutions=Vec::new();
        {let mut st=c.prepare("SELECT rule_id,status,notes,resolved_by,created_at FROM compliance_resolutions WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"rule_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"notes":r.get::<_,String>(2)?,"resolved_by":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?})))?{compliance_resolutions.push(row?);}}
        let mut export_snapshots=Vec::new();
        {let mut st=c.prepare("SELECT snapshot_json,content_sha256,created_at FROM export_snapshots WHERE project_id=?1 ORDER BY id")?;for row in st.query_map([project],|r|Ok(json!({"snapshot":serde_json::from_str::<Value>(&r.get::<_,String>(0)?).unwrap_or(json!({})),"content_sha256":r.get::<_,String>(1)?,"created_at":r.get::<_,String>(2)?})))?{export_snapshots.push(row?);}}
        let payload=json!({"project":project_meta,"workflow":workflow,"documents":documents,"document_chunks":document_chunks,"requirements":requirements,"interview_questions":interview_questions,"interview_answers":interview_answers,"research_queries":research_queries,"research_sources":research_sources,"sections":sections,"generation_runs":generation_runs,"section_versions":section_versions,"approvals":approvals,"workflow_artifacts":artifacts,"evidence":evidence,"citations":citations,"design":design,"clinical_study":clinical_study,"competitive_intelligence":competitive_intelligence,"compliance_profile":compliance_profile,"compliance_sources":compliance_sources,"compliance_resolutions":compliance_resolutions,"export_snapshots":export_snapshots});
        let payload_sha256=sha256_hex(&serde_json::to_vec(&payload)?);
        Ok(json!({"format":"grantspace-portable-project","schema_version":2,"workflow_definition_version":self.workflow_registry.definition_version,"workflow_definition_sha256":self.workflow_registry.definition_sha256()?,"source_project_id":project,"payload_sha256":payload_sha256,"payload":payload}))
    }

    pub fn validate_portable_project_package(&self,package:&Value)->Result<Value>{
        let schema_version=package.get("schema_version").and_then(Value::as_u64).context("portable project schema version is missing")?;
        if package.get("format").and_then(Value::as_str)!=Some("grantspace-portable-project")||!matches!(schema_version,1|2){bail!("unsupported portable project package format or schema version");}
        let expected_definition=self.workflow_registry.definition_sha256()?;
        if package.get("workflow_definition_sha256").and_then(Value::as_str)!=Some(expected_definition.as_str()){bail!("portable project workflow definition does not match this deployment; migrate it with the matching release first");}
        let payload=package.get("payload").and_then(Value::as_object).context("portable project payload is missing")?;
        let expected_hash=package.get("payload_sha256").and_then(Value::as_str).context("portable project payload hash is missing")?;
        if sha256_hex(&serde_json::to_vec(&Value::Object(payload.clone()))?)!=expected_hash{bail!("portable project payload hash does not match its content");}
        let project=payload.get("project").and_then(Value::as_object).context("portable project metadata is missing")?;
        if project.get("title").and_then(Value::as_str).is_none_or(|value|value.trim().is_empty()){bail!("portable project title is required");}
        let config_value=payload.get("workflow").and_then(Value::as_object).and_then(|workflow|workflow.get("config")).context("portable project workflow configuration is missing")?;
        let config:WorkflowConfig=serde_json::from_value(config_value.clone()).context("portable project workflow configuration is invalid")?;
        config.validate(&self.workflow_registry)?;
        let documents=payload.get("documents").and_then(Value::as_array).context("portable project documents must be an array")?;
        let mut document_ids=std::collections::BTreeMap::new();
        for document in documents{let id=document.get("id").and_then(Value::as_i64).context("portable document ID is required")?;if document_ids.insert(id,document).is_some(){bail!("portable project contains a duplicate document ID");}let text=document.get("text").and_then(Value::as_str).context("portable document text is required")?;if document.get("sha256").and_then(Value::as_str)!=Some(sha256_hex(text.as_bytes()).as_str()){bail!("portable document hash does not match its exact text");}}
        let versions=payload.get("section_versions").and_then(Value::as_array).context("portable section_versions must be an array")?;
        let version_ids=versions.iter().map(|value|value.get("id").and_then(Value::as_i64).context("portable section version ID is required")).collect::<Result<BTreeSet<_>>>()?;
        if version_ids.len()!=versions.len(){bail!("portable project contains duplicate section version IDs");}
        for version in versions{for field in ["base_version_id","restored_from_version_id"]{if let Some(id)=version.get(field).and_then(Value::as_i64){if !version_ids.contains(&id){bail!("portable section version references a missing {field}");}}}}
        let empty_generation_runs=Vec::new();
        let generation_runs=if schema_version>=2{payload.get("generation_runs").and_then(Value::as_array).context("portable generation_runs must be an array")?}else{payload.get("generation_runs").and_then(Value::as_array).unwrap_or(&empty_generation_runs)};
        let mut generation_run_ids=BTreeSet::new();
        for run in generation_runs{
            let id=portable_str(run,"id")?;
            if !generation_run_ids.insert(id){bail!("portable project contains a duplicate generation run ID");}
            validate_sha256_field(run,"prompt_sha256",false)?;
            validate_sha256_field(run,"response_sha256",true)?;
            let status=portable_str(run,"status")?;
            if !matches!(status,"complete"|"failed"){bail!("portable generation runs must be complete or failed");}
            if status=="complete"&&run.get("response_sha256").and_then(Value::as_str).is_none(){bail!("portable completed generation run is missing its response digest");}
            match (run.get("input_manifest_json").and_then(Value::as_str),run.get("input_manifest_sha256").and_then(Value::as_str)){
                (Some(raw),Some(expected))=>{serde_json::from_str::<Value>(raw).context("portable generation input manifest is invalid JSON")?;if sha256_hex(raw.as_bytes())!=expected{bail!("portable generation input manifest hash does not match its exact JSON");}},
                (None,None)=>{},
                _=>bail!("portable generation input manifest and digest must either both be present or both be absent"),
            }
            match (run.get("output_contract_name").and_then(Value::as_str),run.get("output_contract_version").and_then(Value::as_i64),run.get("output_schema_json").and_then(Value::as_str),run.get("output_schema_sha256").and_then(Value::as_str)){
                (Some(name),Some(version),Some(raw),Some(expected))=>{if name.is_empty()||version<=0{bail!("portable generation output contract identity is invalid");}let schema:Value=serde_json::from_str(raw).context("portable generation output schema is invalid JSON")?;jsonschema::validator_for(&schema).context("portable generation output schema is invalid")?;if sha256_hex(raw.as_bytes())!=expected{bail!("portable generation output schema hash does not match its exact JSON");}},
                (None,None,None,None)=>{},
                _=>bail!("portable generation output contract metadata must be complete"),
            }
        }
        if schema_version>=2{
            let mut linked_generation_runs=BTreeSet::new();
            for version in versions{if let Some(run_id)=version.get("generation_run_id").and_then(Value::as_str){if !generation_run_ids.contains(run_id){bail!("portable section version references a missing generation run");}if !linked_generation_runs.insert(run_id){bail!("portable generation run is linked to more than one section version");}}}
        }
        let artifacts=payload.get("workflow_artifacts").and_then(Value::as_array).context("portable workflow_artifacts must be an array")?;
        for artifact in artifacts{let kind=artifact.get("artifact_type").and_then(Value::as_str).context("portable artifact type is required")?;let body=artifact.get("body").context("portable artifact body is required")?;if artifact.get("content_sha256").and_then(Value::as_str)!=Some(sha256_hex(&serde_json::to_vec(body)?).as_str()){bail!("portable {kind} artifact hash does not match its body");}crate::workflow_artifacts::validate_artifact_document(kind,body,artifact.get("approved").and_then(Value::as_bool).unwrap_or(false))?;validate_portable_source_anchors(body,&document_ids)?;}
        Ok(json!({"valid":true,"schema_version":schema_version,"title":project.get("title"),"source_project_id":package.get("source_project_id"),"counts":{"documents":documents.len(),"sections":payload.get("sections").and_then(Value::as_array).map_or(0,Vec::len),"generation_runs":generation_runs.len(),"section_versions":versions.len(),"workflow_artifacts":artifacts.len(),"evidence":payload.get("evidence").and_then(Value::as_array).map_or(0,Vec::len),"citations":payload.get("citations").and_then(Value::as_array).map_or(0,Vec::len),"export_snapshots":payload.get("export_snapshots").and_then(Value::as_array).map_or(0,Vec::len)}}))
    }

    pub fn import_portable_project_package(&self,package:&Value,actor:&str)->Result<Value>{
        let validation=self.validate_portable_project_package(package)?;
        let payload=package.get("payload").and_then(Value::as_object).context("portable project payload is missing")?;
        let project_meta=payload.get("project").and_then(Value::as_object).context("portable project metadata is missing")?;
        let title=portable_str_object(project_meta,"title")?;
        let sponsor=project_meta.get("sponsor").and_then(Value::as_str);
        let mechanism=project_meta.get("mechanism").and_then(Value::as_str);
        let config:WorkflowConfig=serde_json::from_value(payload.get("workflow").and_then(Value::as_object).and_then(|workflow|workflow.get("config")).context("portable workflow config is missing")?.clone())?;
        let project_id=Uuid::new_v4().to_string();
        let mut c=self.conn()?;
        let tx=c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO projects(id,title,sponsor,mechanism,stage,interview_generated,updated_at) VALUES(?1,?2,?3,?4,'intake',?5,CURRENT_TIMESTAMP)",params![project_id,title,sponsor,mechanism,!portable_array_object(payload,"interview_questions")?.is_empty() as i64])?;
        let config_raw=serde_json::to_string(&config)?;
        tx.execute("INSERT INTO project_workflows(project_id,definition_version,definition_sha256,config_version,config_sha256,config_json) VALUES(?1,?2,?3,1,?4,?5)",params![project_id,self.workflow_registry.definition_version,self.workflow_registry.definition_sha256()?,sha256_hex(config_raw.as_bytes()),config_raw])?;

        let mut document_map=std::collections::BTreeMap::new();
        for document in portable_array_object(payload,"documents")?{
            tx.execute("INSERT INTO documents(project_id,name,kind,text,sha256,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![project_id,portable_str(document,"name")?,portable_str(document,"kind")?,portable_str(document,"text")?,portable_str(document,"sha256")?,portable_str(document,"created_at")?])?;
            document_map.insert(portable_i64(document,"id")?,tx.last_insert_rowid());
        }
        for chunk in portable_array_object(payload,"document_chunks")?{
            let document_id=*document_map.get(&portable_i64(chunk,"document_id")?).context("portable document chunk references a missing document")?;
            tx.execute("INSERT INTO document_chunks(project_id,document_id,ordinal,start_word,end_word,text) VALUES(?1,?2,?3,?4,?5,?6)",params![project_id,document_id,portable_i64(chunk,"ordinal")?,portable_i64(chunk,"start_word")?,portable_i64(chunk,"end_word")?,portable_str(chunk,"text")?])?;
        }
        for requirement in portable_array_object(payload,"requirements")?{
            tx.execute("INSERT INTO requirements(project_id,external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,source_clue,source_document,source_locator,status,approved,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",params![project_id,portable_str(requirement,"external_id")?,portable_str(requirement,"category")?,portable_str(requirement,"requirement")?,portable_bool(requirement,"mandatory")? as i64,serde_json::to_string(requirement.get("evidence_needed").unwrap_or(&json!([])))?,serde_json::to_string(requirement.get("dependencies").unwrap_or(&json!([])))?,requirement.get("source_clue").and_then(Value::as_str),requirement.get("source_document").and_then(Value::as_str),requirement.get("source_locator").and_then(Value::as_str),portable_str(requirement,"status")?,portable_bool(requirement,"approved")? as i64,portable_str(requirement,"created_at")?])?;
        }

        let mut question_map=std::collections::BTreeMap::new();
        for question in portable_array_object(payload,"interview_questions")?{
            tx.execute("INSERT INTO interview_questions(project_id,requirement_external_id,question,answer_type,choices_json,unit,why_needed,evidence_requested,priority,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project_id,portable_str(question,"requirement_external_id")?,portable_str(question,"question")?,portable_str(question,"answer_type")?,serde_json::to_string(question.get("choices").unwrap_or(&json!([])))?,question.get("unit").and_then(Value::as_str),question.get("why_needed").and_then(Value::as_str),portable_bool(question,"evidence_requested")? as i64,portable_i64(question,"priority")?,portable_str(question,"status")?,portable_str(question,"created_at")?])?;
            question_map.insert(portable_i64(question,"id")?,tx.last_insert_rowid());
        }
        for answer in portable_array_object(payload,"interview_answers")?{
            let question_id=*question_map.get(&portable_i64(answer,"question_id")?).context("portable interview answer references a missing question")?;
            tx.execute("INSERT INTO interview_answers(project_id,question_id,value_json,confidence,classification,notes,answered_by,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project_id,question_id,serde_json::to_string(answer.get("value").unwrap_or(&Value::Null))?,portable_str(answer,"confidence")?,portable_str(answer,"classification")?,answer.get("notes").and_then(Value::as_str),answer.get("answered_by").and_then(Value::as_str),portable_str(answer,"created_at")?])?;
        }

        let mut query_map=std::collections::BTreeMap::new();
        for query in portable_array_object(payload,"research_queries")?{
            tx.execute("INSERT INTO research_queries(project_id,requirement_external_id,query,preferred_domains_json,rationale,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![project_id,portable_str(query,"requirement_external_id")?,portable_str(query,"query")?,serde_json::to_string(query.get("preferred_domains").unwrap_or(&json!([])))?,query.get("rationale").and_then(Value::as_str),portable_str(query,"status")?,portable_str(query,"created_at")?])?;
            query_map.insert(portable_i64(query,"id")?,tx.last_insert_rowid());
        }
        for source in portable_array_object(payload,"research_sources")?{
            let query_id=source.get("query_id").and_then(Value::as_i64).map(|old|query_map.get(&old).copied().context("portable research source references a missing query")).transpose()?;
            let text=portable_str(source,"text")?;
            if portable_str(source,"content_sha256")?!=sha256_hex(text.as_bytes()){bail!("portable research source hash does not match its text");}
            tx.execute("INSERT INTO research_sources(project_id,query_id,title,url,text,retrieved_at,content_sha256,http_status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project_id,query_id,portable_str(source,"title")?,portable_str(source,"url")?,text,portable_str(source,"retrieved_at")?,portable_str(source,"content_sha256")?,portable_i64(source,"http_status")?])?;
        }

        let empty_generation_runs=Vec::new();
        let generation_records=payload.get("generation_runs").and_then(Value::as_array).unwrap_or(&empty_generation_runs);
        let mut generation_run_map=std::collections::BTreeMap::new();
        for run in generation_records{
            let old_id=portable_str(run,"id")?;
            let new_id=Uuid::new_v4().to_string();
            tx.execute("INSERT INTO generation_runs(id,project_id,task_kind,routing_mode,provider,model,prompt_sha256,response_sha256,input_manifest_json,input_manifest_sha256,high_value,status,error,started_at,completed_at,output_contract_name,output_contract_version,output_schema_json,output_schema_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",params![new_id,project_id,portable_str(run,"task_kind")?,portable_str(run,"routing_mode")?,portable_str(run,"provider")?,portable_str(run,"model")?,portable_str(run,"prompt_sha256")?,run.get("response_sha256").and_then(Value::as_str),run.get("input_manifest_json").and_then(Value::as_str),run.get("input_manifest_sha256").and_then(Value::as_str),portable_bool(run,"high_value")? as i64,portable_str(run,"status")?,run.get("error").and_then(Value::as_str),portable_str(run,"started_at")?,run.get("completed_at").and_then(Value::as_str),run.get("output_contract_name").and_then(Value::as_str),run.get("output_contract_version").and_then(Value::as_i64),run.get("output_schema_json").and_then(Value::as_str),run.get("output_schema_sha256").and_then(Value::as_str)])?;
            generation_run_map.insert(old_id.to_owned(),new_id);
        }

        for section in portable_array_object(payload,"sections")?{
            tx.execute("INSERT INTO project_sections(project_id,section_key,title,position,required,origin,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![project_id,portable_str(section,"section_key")?,portable_str(section,"title")?,portable_i64(section,"position")?,portable_bool(section,"required")? as i64,portable_str(section,"origin")?,portable_str(section,"created_at")?])?;
        }
        let mut version_map=std::collections::BTreeMap::new();
        for version in portable_array_object(payload,"section_versions")?{
            let generation_run_id=version.get("generation_run_id").and_then(Value::as_str).and_then(|old_id|generation_run_map.get(old_id)).map(String::as_str);
            tx.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source,editor_name,author_user_id,approved,generation_run_id,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project_id,portable_str(version,"section_key")?,portable_str(version,"title")?,portable_str(version,"body")?,version.get("html").and_then(Value::as_str),portable_str(version,"source")?,version.get("editor_name").and_then(Value::as_str),version.get("author_user_id").and_then(Value::as_str),portable_bool(version,"approved")? as i64,generation_run_id,portable_str(version,"created_at")?])?;
            version_map.insert(portable_i64(version,"id")?,tx.last_insert_rowid());
        }
        for version in portable_array_object(payload,"section_versions")?{
            let new_id=*version_map.get(&portable_i64(version,"id")?).context("portable section version map is incomplete")?;
            let base=version.get("base_version_id").and_then(Value::as_i64).map(|old|version_map.get(&old).copied().context("portable base version is missing")).transpose()?;
            let restored=version.get("restored_from_version_id").and_then(Value::as_i64).map(|old|version_map.get(&old).copied().context("portable restored version is missing")).transpose()?;
            tx.execute("UPDATE section_versions SET base_version_id=?1,restored_from_version_id=?2 WHERE id=?3",params![base,restored,new_id])?;
        }
        for approval in portable_array_object(payload,"approvals")?{
            let version_id=*version_map.get(&portable_i64(approval,"version_id")?).context("portable approval references a missing section version")?;
            tx.execute("INSERT INTO approvals(project_id,section_key,version_id,approved_by,approver_user_id,role_at_approval,decision,notes,approved_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![project_id,portable_str(approval,"section_key")?,version_id,approval.get("approved_by").and_then(Value::as_str),approval.get("approver_user_id").and_then(Value::as_str),approval.get("role_at_approval").and_then(Value::as_str),portable_str(approval,"decision")?,approval.get("notes").and_then(Value::as_str),portable_str(approval,"approved_at")?])?;
        }

        for artifact in portable_array_object(payload,"workflow_artifacts")?{
            let mut body=artifact.get("body").context("portable workflow artifact body is missing")?.clone();
            remap_portable_document_ids(&mut body,&document_map);
            let raw=serde_json::to_string(&body)?;let sha=sha256_hex(raw.as_bytes());
            tx.execute("INSERT INTO workflow_artifacts(project_id,artifact_type,version,body_json,content_sha256,source,author,approved,approved_by,approved_at,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project_id,portable_str(artifact,"artifact_type")?,portable_i64(artifact,"version")?,raw,sha,portable_str(artifact,"source")?,artifact.get("author").and_then(Value::as_str),portable_bool(artifact,"approved")? as i64,artifact.get("approved_by").and_then(Value::as_str),artifact.get("approved_at").and_then(Value::as_str),portable_str(artifact,"created_at")?])?;
        }

        let mut evidence_map=std::collections::BTreeMap::new();
        for item in portable_array_object(payload,"evidence")?{
            tx.execute("INSERT INTO evidence(project_id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project_id,item.get("requirement_external_id").and_then(Value::as_str),portable_str(item,"source_type")?,portable_str(item,"source_ref")?,portable_str(item,"claim")?,portable_str(item,"passage")?,item.get("source_url").and_then(Value::as_str),item.get("source_locator").and_then(Value::as_str),item.get("confidence").and_then(Value::as_f64).context("portable evidence confidence is required")?,portable_str(item,"status")?,portable_str(item,"created_at")?])?;
            evidence_map.insert(portable_i64(item,"id")?,tx.last_insert_rowid());
        }
        for citation in portable_array_object(payload,"citations")?{
            let evidence_id=*evidence_map.get(&portable_i64(citation,"evidence_id")?).context("portable citation references missing evidence")?;
            let passage=portable_str(citation,"passage")?;
            if portable_str(citation,"content_sha256")?!=sha256_hex(passage.as_bytes()){bail!("portable citation hash does not match its exact passage");}
            tx.execute("INSERT INTO citations(project_id,evidence_id,citation_key,title,url,passage,content_sha256,verified,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![project_id,evidence_id,portable_str(citation,"citation_key")?,portable_str(citation,"title")?,citation.get("url").and_then(Value::as_str),passage,portable_str(citation,"content_sha256")?,portable_bool(citation,"verified")? as i64,portable_str(citation,"created_at")?])?;
        }
        if let Some(design)=payload.get("design").filter(|value|!value.is_null()){
            let profile=design.get("profile").context("portable design profile is missing")?;let raw=serde_json::to_string(profile)?;if design.get("content_sha256").and_then(Value::as_str)!=Some(sha256_hex(raw.as_bytes()).as_str()){bail!("portable design profile hash does not match its content");}
            tx.execute("INSERT INTO project_design(project_id,profile_json,content_sha256,updated_at) VALUES(?1,?2,?3,?4)",params![project_id,raw,portable_str(design,"content_sha256")?,portable_str(design,"updated_at")?])?;
        }
        if let Some(clinical)=payload.get("clinical_study").filter(|value|value.get("exists").and_then(Value::as_bool).unwrap_or(false)){
            let study=clinical.get("study").context("portable clinical study body is missing")?;
            let typed:ClinicalStudy=serde_json::from_value(study.clone()).context("portable clinical study is invalid")?;crate::clinical::validate_study(&typed)?;
            let raw=serde_json::to_string(study)?;let sha=sha256_hex(raw.as_bytes());if clinical.get("sha256").and_then(Value::as_str)!=Some(sha.as_str()){bail!("portable clinical study hash does not match its content");}
            let version=clinical.get("version").and_then(Value::as_i64).context("portable clinical study version is required")?;
            tx.execute("INSERT INTO clinical_study_history(project_id,version,study_json,content_sha256,created_at) VALUES(?1,?2,?3,?4,?5)",params![project_id,version,raw,sha,portable_str(clinical,"updated_at")?])?;
            tx.execute("INSERT INTO clinical_studies(project_id,version,study_json,content_sha256,updated_at) VALUES(?1,?2,?3,?4,?5)",params![project_id,version,raw,sha,portable_str(clinical,"updated_at")?])?;
        }
        if let Some(compliance)=payload.get("compliance_profile").filter(|value|value.get("exists").and_then(Value::as_bool).unwrap_or(false)){
            let profile=compliance.get("profile").context("portable compliance profile body is missing")?;
            let typed:ComplianceProfile=serde_json::from_value(profile.clone()).context("portable compliance profile is invalid")?;crate::compliance::validate_profile(&typed)?;
            let raw=serde_json::to_string(profile)?;let sha=sha256_hex(raw.as_bytes());if compliance.get("sha256").and_then(Value::as_str)!=Some(sha.as_str()){bail!("portable compliance profile hash does not match its content");}
            let version=compliance.get("version").and_then(Value::as_i64).context("portable compliance profile version is required")?;
            let fingerprint=portable_str(compliance,"source_fingerprint")?;let model=portable_str(compliance,"model")?;let approved=compliance.get("approved").and_then(Value::as_bool).unwrap_or(false) as i64;let updated=portable_str(compliance,"updated_at")?;
            tx.execute("INSERT INTO compliance_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project_id,version,fingerprint,raw,sha,model,approved,updated])?;
            tx.execute("INSERT INTO compliance_profiles(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project_id,version,fingerprint,raw,sha,model,approved,updated])?;
            for source in portable_array_object(payload,"compliance_sources")?{
                let document_id=source.get("source_document_id").and_then(Value::as_i64).map(|old|document_map.get(&old).copied().context("portable compliance source references a missing document")).transpose()?;
                tx.execute("INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![project_id,portable_i64(source,"profile_version")?,portable_str(source,"rule_id")?,portable_str(source,"source_status")?,portable_str(source,"source_hint")?,document_id,source.get("source_start_offset").and_then(Value::as_i64),source.get("source_end_offset").and_then(Value::as_i64),source.get("source_page").and_then(Value::as_i64),portable_str(source,"source_excerpt")?])?;
            }
            for resolution in portable_array_object(payload,"compliance_resolutions")?{
                tx.execute("INSERT INTO compliance_resolutions(project_id,rule_id,status,notes,resolved_by,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![project_id,portable_str(resolution,"rule_id")?,portable_str(resolution,"status")?,portable_str(resolution,"notes")?,resolution.get("resolved_by").and_then(Value::as_str),portable_str(resolution,"created_at")?])?;
            }
        }
        if let Some(competitive)=payload.get("competitive_intelligence").filter(|value|value.get("exists").and_then(Value::as_bool).unwrap_or(false)){
            let strategy=competitive.get("strategy").cloned().unwrap_or(Value::Null);let strategy_raw=if strategy.is_null(){None}else{Some(serde_json::to_string(&strategy)?)};
            tx.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,strategy_json,strategy_sha256,strategy_model,created_at,completed_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![project_id,competitive.get("profile_version").and_then(Value::as_i64).unwrap_or(1),portable_str(competitive,"input_fingerprint")?,portable_str(competitive,"config_sha256")?,portable_str(competitive,"status")?,serde_json::to_string(competitive.get("provider_status").unwrap_or(&json!([])))?,strategy_raw,competitive.get("strategy_sha256").and_then(Value::as_str),competitive.get("strategy_model").and_then(Value::as_str),portable_str(competitive,"created_at")?,competitive.get("completed_at").and_then(Value::as_str)])?;
            let run_id=tx.last_insert_rowid();
            for candidate in competitive.get("candidates").and_then(Value::as_array).context("portable competitive candidates must be an array")?{
                tx.execute("INSERT INTO competitor_candidates(run_id,project_id,candidate_key,name,rank,overall_score,grant_score,publication_score,clinical_trial_score,patent_ip_score,technology_score,breadth_score,asset_count,asset_counts_json,dimension_coverage_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![run_id,project_id,portable_str(candidate,"candidate_key")?,portable_str(candidate,"name")?,portable_i64(candidate,"rank")?,candidate.get("overall_score").and_then(Value::as_f64).context("portable competitive overall_score is required")?,candidate.get("grant_score").and_then(Value::as_f64).context("portable competitive grant_score is required")?,candidate.get("publication_score").and_then(Value::as_f64).context("portable competitive publication_score is required")?,candidate.get("clinical_trial_score").and_then(Value::as_f64).context("portable competitive clinical_trial_score is required")?,candidate.get("patent_ip_score").and_then(Value::as_f64).context("portable competitive patent_ip_score is required")?,candidate.get("technology_score").and_then(Value::as_f64).context("portable competitive technology_score is required")?,candidate.get("breadth_score").and_then(Value::as_f64).context("portable competitive breadth_score is required")?,portable_i64(candidate,"asset_count")?,serde_json::to_string(candidate.get("asset_counts").unwrap_or(&json!({})))?,serde_json::to_string(candidate.get("dimension_coverage").unwrap_or(&json!([])))?])?;
            }
            for asset in competitive.get("assets").and_then(Value::as_array).context("portable competitive assets must be an array")?{
                tx.execute("INSERT INTO competitor_assets(run_id,project_id,candidate_key,asset_key,provider,asset_type,external_id,title,summary,url,year,amount,dimension_id,metadata_json,relevance) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",params![run_id,project_id,portable_str(asset,"candidate_key")?,portable_str(asset,"asset_key")?,portable_str(asset,"provider")?,portable_str(asset,"asset_type")?,portable_str(asset,"external_id")?,portable_str(asset,"title")?,portable_str(asset,"summary")?,asset.get("url").and_then(Value::as_str),asset.get("year").and_then(Value::as_i64),asset.get("amount").and_then(Value::as_f64),asset.get("dimension_id").and_then(Value::as_str),serde_json::to_string(asset.get("metadata").unwrap_or(&json!({})))?,asset.get("relevance").and_then(Value::as_f64).context("portable competitive relevance is required")?])?;
            }
        }
        for snapshot in portable_array_object(payload,"export_snapshots")?{
            let body=snapshot.get("snapshot").context("portable export snapshot body is missing")?;let raw=serde_json::to_string(body)?;if snapshot.get("content_sha256").and_then(Value::as_str)!=Some(sha256_hex(raw.as_bytes()).as_str()){bail!("portable export snapshot hash does not match its content");}
            tx.execute("INSERT INTO export_snapshots(project_id,snapshot_json,content_sha256,created_at) VALUES(?1,?2,?3,?4)",params![project_id,raw,portable_str(snapshot,"content_sha256")?,portable_str(snapshot,"created_at")?])?;
        }
        tx.execute("INSERT INTO project_members(project_id,user_id,role,invited_by_user_id) VALUES(?1,?2,'owner',?2)",params![project_id,actor])?;
        let channel_id=Uuid::new_v4().to_string();tx.execute("INSERT INTO channels(id,project_id,kind,subject_key,name,created_by_user_id) VALUES(?1,?2,'general',NULL,'General',?3)",params![channel_id,project_id,actor])?;
        if config.enabled("team_collaboration"){
            let mut routed=vec!["solicitation_profile","research_framework","aim_set","literature_manifest","proposal_section","proposal_snapshot"];
            if config.enabled("review_simulator"){routed.push("review_simulation");}
            let routes=routed.into_iter().map(|artifact_type|json!({"artifact_type":artifact_type,"owner_user_id":actor,"approver_user_ids":[actor],"minimum_approvals":1})).collect::<Vec<_>>();
            let body=json!({"schema_version":1,"project_owner_user_id":actor,"routes":routes});
            crate::workflow_artifacts::validate_artifact_document("collaboration_record",&body,true)?;
            let version:i64=tx.query_row("SELECT COALESCE(MAX(version),0)+1 FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='collaboration_record'",[&project_id],|row|row.get(0))?;
            let raw=serde_json::to_string(&body)?;let sha=sha256_hex(raw.as_bytes());
            tx.execute("INSERT INTO workflow_artifacts(project_id,artifact_type,version,body_json,content_sha256,source,author,approved,approved_by,approved_at) VALUES(?1,'collaboration_record',?2,?3,?4,'portable_import_owner_reconciliation',?5,1,?5,CURRENT_TIMESTAMP)",params![project_id,version,raw,sha,actor])?;
        }
        tx.execute("INSERT INTO workflow_events(project_id,event_type,actor,payload_json) VALUES(?1,'portable_project_imported',?2,?3)",params![project_id,actor,serde_json::to_string(&json!({"source_project_id":package.get("source_project_id"),"payload_sha256":package.get("payload_sha256"),"validation":validation}))?])?;
        tx.commit()?;
        Ok(json!({"id":project_id,"title":title,"validation":validation}))
    }

    pub fn competitive_input_fingerprint(&self, project: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        let c = self.conn()?;
        let mut h = Sha256::new();
        h.update(project.as_bytes());
        let meta: (String, Option<String>, Option<String>) = c.query_row(
            "SELECT title,sponsor,mechanism FROM projects WHERE id=?1",
            [project],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        h.update(meta.0.as_bytes());
        h.update(meta.1.unwrap_or_default().as_bytes());
        h.update(meta.2.unwrap_or_default().as_bytes());
        let aggregates=[
            ("documents","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM documents WHERE project_id=?1"),
            ("requirements","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1"),
            ("interview_answers","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM interview_answers WHERE project_id=?1"),
            ("evidence","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM evidence WHERE project_id=?1"),
            ("clinical_study","SELECT COUNT(*),COALESCE(MAX(version),0),COALESCE(SUM(version),0) FROM clinical_studies WHERE project_id=?1")
        ];
        for (name, sql) in aggregates {
            let (a, b, cstate): (i64, i64, i64) =
                c.query_row(sql, [project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            h.update(name.as_bytes());
            h.update(a.to_le_bytes());
            h.update(b.to_le_bytes());
            h.update(cstate.to_le_bytes());
        }
        Ok(hex::encode(h.finalize()))
    }

    pub fn save_competitive_profile(
        &self,
        project: &str,
        profile: &CompetitiveProfile,
        source_fingerprint: &str,
        model: &str,
    ) -> Result<Value> {
        let bytes = serde_json::to_vec(profile)?;
        let sha = sha256_hex(&bytes);
        let raw = String::from_utf8(bytes)?;
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let version: i64 = tx
            .query_row(
                "SELECT COALESCE(version,0)+1 FROM competitive_profiles WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(1);
        tx.execute("INSERT INTO competitive_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model) VALUES(?1,?2,?3,?4,?5,?6)",params![project,version,source_fingerprint,raw,sha,model])?;
        tx.execute(r#"INSERT INTO competitive_profiles(project_id,version,source_fingerprint,profile_json,content_sha256,model,updated_at)
          VALUES(?1,?2,?3,?4,?5,?6,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET version=excluded.version,source_fingerprint=excluded.source_fingerprint,profile_json=excluded.profile_json,content_sha256=excluded.content_sha256,model=excluded.model,updated_at=CURRENT_TIMESTAMP"#,params![project,version,source_fingerprint,raw,sha,model])?;
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        Ok(
            json!({"version":version,"sha256":sha,"source_fingerprint":source_fingerprint,"model":model,"profile":profile}),
        )
    }

    pub fn competitive_profile_typed(&self, project: &str) -> Result<Option<CompetitiveProfile>> {
        let c = self.conn()?;
        let raw = c
            .query_row(
                "SELECT profile_json FROM competitive_profiles WHERE project_id=?1",
                [project],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        match raw {
            Some(x) => Ok(Some(
                serde_json::from_str(&x).context("stored competitive profile is invalid JSON")?,
            )),
            None => Ok(None),
        }
    }

    pub fn competitive_profile_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row("SELECT version,source_fingerprint,profile_json,content_sha256,model,updated_at FROM competitive_profiles WHERE project_id=?1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).optional()?;
        let current = self.competitive_input_fingerprint(project)?;
        if let Some((version, source_fp, raw, sha, model, updated_at)) = row {
            let profile: Value = serde_json::from_str(&raw)?;
            Ok(
                json!({"exists":true,"fresh":source_fp==current,"version":version,"source_fingerprint":source_fp,"current_fingerprint":current,"sha256":sha,"model":model,"updated_at":updated_at,"profile":profile}),
            )
        } else {
            Ok(
                json!({"exists":false,"fresh":false,"version":null,"profile":null,"current_fingerprint":current}),
            )
        }
    }

    pub fn begin_competitive_run(
        &self,
        project: &str,
        profile_version: i64,
        input_fingerprint: &str,
        config_sha256: &str,
    ) -> Result<i64> {
        let c = self.conn()?;
        c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status) VALUES(?1,?2,?3,?4,'running')",params![project,profile_version,input_fingerprint,config_sha256])?;
        Ok(c.last_insert_rowid())
    }

    pub fn fail_competitive_run(&self, run_id: i64, detail: &str) -> Result<()> {
        let c = self.conn()?;
        c.execute("UPDATE competitive_runs SET status='failed',provider_status_json=?1,completed_at=CURRENT_TIMESTAMP WHERE id=?2",params![serde_json::to_string(&json!([{"provider":"run","ok":false,"records":0,"detail":detail}]))?,run_id])?;
        Ok(())
    }

    pub fn finish_competitive_run(
        &self,
        project: &str,
        run_id: i64,
        out: &CompetitiveRunOutput,
    ) -> Result<Value> {
        let strategy_bytes = serde_json::to_vec(&out.strategy)?;
        let strategy_sha = sha256_hex(&strategy_bytes);
        let strategy_raw = String::from_utf8(strategy_bytes)?;
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let owner: String = tx.query_row(
            "SELECT project_id FROM competitive_runs WHERE id=?1",
            [run_id],
            |r| r.get(0),
        )?;
        if owner != project {
            bail!("competitive run does not belong to project");
        }
        tx.execute(
            "DELETE FROM competitor_candidates WHERE run_id=?1",
            [run_id],
        )?;
        tx.execute("DELETE FROM competitor_assets WHERE run_id=?1", [run_id])?;
        for (rank, cand) in out.candidates.iter().enumerate() {
            tx.execute(r#"INSERT INTO competitor_candidates(run_id,project_id,candidate_key,name,rank,overall_score,grant_score,publication_score,clinical_trial_score,patent_ip_score,technology_score,breadth_score,asset_count,asset_counts_json,dimension_coverage_json)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,params![run_id,project,cand.candidate_key,cand.name,(rank+1) as i64,cand.overall_score,cand.grant_score,cand.publication_score,cand.clinical_trial_score,cand.patent_ip_score,cand.technology_score,cand.breadth_score,cand.asset_count as i64,serde_json::to_string(&cand.asset_counts)?,serde_json::to_string(&cand.dimension_coverage)?])?;
        }
        for a in &out.assets {
            tx.execute(r#"INSERT INTO competitor_assets(run_id,project_id,candidate_key,asset_key,provider,asset_type,external_id,title,summary,url,year,amount,dimension_id,metadata_json,relevance)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,params![run_id,project,a.candidate_key,a.asset_key,a.provider,a.asset_type,a.external_id,a.title,a.summary,a.url,a.year,a.amount,a.dimension_id,serde_json::to_string(&a.metadata)?,a.relevance])?;
        }
        tx.execute("UPDATE competitive_runs SET status='complete',provider_status_json=?1,strategy_json=?2,strategy_sha256=?3,strategy_model=?4,completed_at=CURRENT_TIMESTAMP WHERE id=?5",params![serde_json::to_string(&out.provider_status)?,strategy_raw,strategy_sha,out.strategy_model,run_id])?;
        tx.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?;
        self.competitive_latest_json(project)
    }

    pub fn competitive_latest_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let current = self.competitive_input_fingerprint(project)?;
        let row=c.query_row("SELECT id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,strategy_json,strategy_sha256,strategy_model,created_at,completed_at FROM competitive_runs WHERE project_id=?1 ORDER BY id DESC LIMIT 1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,String>(9)?,r.get::<_,Option<String>>(10)?))).optional()?;
        let Some((
            run_id,
            profile_version,
            input_fp,
            config_sha,
            status,
            provider_raw,
            strategy_raw,
            strategy_sha,
            strategy_model,
            created,
            completed,
        )) = row
        else {
            return Ok(
                json!({"exists":false,"fresh":false,"current_fingerprint":current,"candidates":[],"assets":[],"strategy":null}),
            );
        };
        let mut st=c.prepare("SELECT candidate_key,name,rank,overall_score,grant_score,publication_score,clinical_trial_score,patent_ip_score,technology_score,breadth_score,asset_count,asset_counts_json,dimension_coverage_json FROM competitor_candidates WHERE run_id=?1 ORDER BY rank")?;
        let rows=st.query_map([run_id],|r|Ok(json!({"candidate_key":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"rank":r.get::<_,i64>(2)?,"overall_score":r.get::<_,f64>(3)?,"grant_score":r.get::<_,f64>(4)?,"publication_score":r.get::<_,f64>(5)?,"clinical_trial_score":r.get::<_,f64>(6)?,"patent_ip_score":r.get::<_,f64>(7)?,"technology_score":r.get::<_,f64>(8)?,"breadth_score":r.get::<_,f64>(9)?,"asset_count":r.get::<_,i64>(10)?,"asset_counts":serde_json::from_str::<Value>(&r.get::<_,String>(11)?).unwrap_or(json!({})),"dimension_coverage":serde_json::from_str::<Value>(&r.get::<_,String>(12)?).unwrap_or(json!([]))})))?;
        let mut candidates = Vec::new();
        for x in rows {
            candidates.push(x?);
        }
        let mut ast=c.prepare("SELECT candidate_key,asset_key,provider,asset_type,external_id,title,summary,url,year,amount,dimension_id,metadata_json,relevance FROM competitor_assets WHERE run_id=?1 ORDER BY relevance DESC,id LIMIT 1000")?;
        let rows=ast.query_map([run_id],|r|{
            let metadata_raw=r.get::<_,String>(11)?;
            Ok(json!({"candidate_key":r.get::<_,String>(0)?,"asset_key":r.get::<_,String>(1)?,"provider":r.get::<_,String>(2)?,"asset_type":r.get::<_,String>(3)?,"external_id":r.get::<_,String>(4)?,"title":r.get::<_,String>(5)?,"summary":r.get::<_,String>(6)?,"url":r.get::<_,Option<String>>(7)?,"year":r.get::<_,Option<i64>>(8)?,"amount":r.get::<_,Option<f64>>(9)?,"dimension_id":r.get::<_,Option<String>>(10)?,"metadata":serde_json::from_str::<Value>(&metadata_raw).unwrap_or(json!({})),"relevance":r.get::<_,f64>(12)?}))
        })?;
        let mut assets = Vec::new();
        for x in rows {
            assets.push(x?);
        }
        let strategy = strategy_raw
            .as_deref()
            .and_then(|x| serde_json::from_str::<Value>(x).ok())
            .unwrap_or(Value::Null);
        let providers = serde_json::from_str::<Value>(&provider_raw).unwrap_or(json!([]));
        let current_config_sha = current_competitive_config_sha().ok();
        let config_fresh = current_config_sha.as_deref() == Some(config_sha.as_str());
        let refresh_ttl_seconds = std::env::var("COMPETITIVE_REFRESH_TTL_SECONDS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(14_400)
            .clamp(300, 604_800);
        let age_seconds:Option<i64>=c.query_row("SELECT CASE WHEN completed_at IS NULL THEN NULL ELSE MAX(0,CAST((julianday('now')-julianday(completed_at))*86400 AS INTEGER)) END FROM competitive_runs WHERE id=?1",[run_id],|r|r.get(0)).optional()?.flatten();
        let time_fresh = age_seconds
            .map(|age| age <= refresh_ttl_seconds)
            .unwrap_or(false);
        let input_fresh = input_fp == current;
        let complete = status == "complete";
        let fresh = complete && input_fresh && config_fresh && time_fresh;
        let mut stale_reasons = Vec::<String>::new();
        if !complete {
            stale_reasons.push(format!("status:{status}"));
        }
        if !input_fresh {
            stale_reasons.push("project_inputs_changed".into());
        }
        if !config_fresh {
            stale_reasons.push("competitive_config_changed".into());
        }
        if !time_fresh {
            stale_reasons.push("public_intelligence_refresh_due".into());
        }
        Ok(
            json!({"exists":true,"fresh":fresh,"run_id":run_id,"profile_version":profile_version,"input_fingerprint":input_fp,"current_fingerprint":current,"input_fresh":input_fresh,"config_sha256":config_sha,"current_config_sha256":current_config_sha,"config_fresh":config_fresh,"refresh_ttl_seconds":refresh_ttl_seconds,"age_seconds":age_seconds,"time_fresh":time_fresh,"stale_reasons":stale_reasons,"status":status,"provider_status":providers,"strategy":strategy,"strategy_sha256":strategy_sha,"strategy_model":strategy_model,"created_at":created,"completed_at":completed,"candidates":candidates,"assets":assets}),
        )
    }

    pub fn record_competitive_update_event(
        &self,
        project: &str,
        delta: &CompetitiveDelta,
        refresh_reason: &Value,
    ) -> Result<i64> {
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let status = if delta.material {
            "pending"
        } else {
            "complete"
        };
        if delta.material {
            // A newer material public-intelligence run supersedes unfinished proposals
            // from older runs. Keep the history, but never let stale older proposals
            // reappear after the newest strategy has been published.
            tx.execute("UPDATE competitive_section_updates SET status='superseded',resolved_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND status='pending'",[project])?;
            tx.execute("UPDATE competitive_update_events SET text_refresh_status='complete',text_refresh_errors_json='[\"superseded_by_newer_competitive_refresh\"]',processed_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND material=1 AND text_refresh_status!='complete'",[project])?;
        }
        tx.execute(r#"INSERT INTO competitive_update_events(project_id,from_run_id,to_run_id,refresh_reason_json,delta_json,summary,material,text_refresh_status,processed_at)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,CASE WHEN ?8='complete' THEN CURRENT_TIMESTAMP ELSE NULL END)"#,params![project,delta.from_run_id,delta.to_run_id,serde_json::to_string(refresh_reason)?,serde_json::to_string(delta)?,delta.summary,if delta.material{1}else{0},status])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn competitive_update_event_json(&self, project: &str, event_id: i64) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row(r#"SELECT id,from_run_id,to_run_id,refresh_reason_json,delta_json,summary,material,text_refresh_status,text_refresh_errors_json,created_at,processed_at
          FROM competitive_update_events WHERE project_id=?1 AND id=?2"#,params![project,event_id],|r|{
            let rr=r.get::<_,String>(3)?;let delta=r.get::<_,String>(4)?;let errors=r.get::<_,String>(8)?;
            Ok(json!({"event_id":r.get::<_,i64>(0)?,"from_run_id":r.get::<_,Option<i64>>(1)?,"to_run_id":r.get::<_,i64>(2)?,"refresh_reason":serde_json::from_str::<Value>(&rr).unwrap_or(json!([])),"delta":serde_json::from_str::<Value>(&delta).unwrap_or(json!({})),"summary":r.get::<_,String>(5)?,"material":r.get::<_,i64>(6)?!=0,"text_refresh_status":r.get::<_,String>(7)?,"text_refresh_errors":serde_json::from_str::<Value>(&errors).unwrap_or(json!([])),"created_at":r.get::<_,String>(9)?,"processed_at":r.get::<_,Option<String>>(10)?}))
        }).optional()?;
        Ok(row.unwrap_or_else(|| json!({})))
    }

    pub fn latest_unprocessed_competitive_update_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let id=c.query_row("SELECT id FROM competitive_update_events WHERE project_id=?1 AND material=1 AND text_refresh_status!='complete' ORDER BY id DESC LIMIT 1",[project],|r|r.get::<_,i64>(0)).optional()?;
        match id {
            Some(x) => self.competitive_update_event_json(project, x),
            None => Ok(json!({})),
        }
    }

    pub fn set_competitive_update_processing(
        &self,
        project: &str,
        event_id: i64,
        status: &str,
        errors: &Value,
    ) -> Result<()> {
        if !matches!(status, "pending" | "partial" | "complete") {
            bail!("invalid competitive update processing status");
        }
        let c = self.conn()?;
        c.execute("UPDATE competitive_update_events SET text_refresh_status=?1,text_refresh_errors_json=?2,processed_at=CASE WHEN ?1='complete' THEN CURRENT_TIMESTAMP ELSE processed_at END WHERE id=?3 AND project_id=?4",params![status,serde_json::to_string(errors)?,event_id,project])?;
        Ok(())
    }

    pub fn competitive_text_refresh_pending_count(&self, project: &str) -> Result<i64> {
        let c = self.conn()?;
        Ok(c.query_row("SELECT COUNT(*) FROM competitive_update_events WHERE project_id=?1 AND material=1 AND text_refresh_status!='complete'",[project],|r|r.get(0))?)
    }

    pub fn record_competitive_section_update(
        &self,
        event_id: i64,
        project: &str,
        section_key: &str,
        base_version_id: i64,
        proposed_version_id: i64,
    ) -> Result<i64> {
        let c = self.conn()?;
        // Supersede older unresolved proposals for the same section. The newest public intelligence wins, but the history remains auditable.
        c.execute("UPDATE competitive_section_updates SET status='superseded',resolved_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND section_key=?2 AND status='pending'",params![project,section_key])?;
        c.execute(r#"INSERT INTO competitive_section_updates(event_id,project_id,section_key,base_version_id,proposed_version_id,status) VALUES(?1,?2,?3,?4,?5,'pending')"#,params![event_id,project,section_key,base_version_id,proposed_version_id])?;
        Ok(c.last_insert_rowid())
    }

    pub fn record_competitive_section_no_change(
        &self,
        event_id: i64,
        project: &str,
        section_key: &str,
        base_version_id: i64,
    ) -> Result<i64> {
        let c = self.conn()?;
        c.execute(r#"INSERT OR IGNORE INTO competitive_section_updates(event_id,project_id,section_key,base_version_id,proposed_version_id,status,resolved_at) VALUES(?1,?2,?3,?4,?4,'no_change',CURRENT_TIMESTAMP)"#,params![event_id,project,section_key,base_version_id])?;
        Ok(c.last_insert_rowid())
    }

    pub fn competitive_section_update_exists(
        &self,
        event_id: i64,
        project: &str,
        section_key: &str,
    ) -> Result<bool> {
        let c = self.conn()?;
        let n:i64=c.query_row("SELECT COUNT(*) FROM competitive_section_updates WHERE event_id=?1 AND project_id=?2 AND section_key=?3",params![event_id,project,section_key],|r|r.get(0))?;
        Ok(n > 0)
    }

    pub fn competitive_pending_update_count(&self, project: &str) -> Result<i64> {
        let c = self.conn()?;
        Ok(c.query_row("SELECT COUNT(*) FROM competitive_section_updates WHERE project_id=?1 AND status='pending'",[project],|r|r.get(0))?)
    }

    pub fn competitive_pending_section_updates_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare(r#"SELECT csu.id,csu.event_id,csu.section_key,COALESCE(ps.title,csu.section_key),
          csu.base_version_id,csu.proposed_version_id,csu.status,e.summary,e.created_at
          FROM competitive_section_updates csu
          JOIN competitive_update_events e ON e.id=csu.event_id
          LEFT JOIN project_sections ps ON ps.project_id=csu.project_id AND ps.section_key=csu.section_key
          WHERE csu.project_id=?1 AND csu.status='pending'
          ORDER BY COALESCE(ps.position,999999),csu.event_id DESC"#)?;
        let rows=st.query_map([project],|r|Ok(json!({
            "id":r.get::<_,i64>(0)?,"event_id":r.get::<_,i64>(1)?,"section_key":r.get::<_,String>(2)?,
            "title":r.get::<_,String>(3)?,"base_version":r.get::<_,i64>(4)?,"proposed_version":r.get::<_,i64>(5)?,
            "status":r.get::<_,String>(6)?,"summary":r.get::<_,String>(7)?,"created_at":r.get::<_,String>(8)?
        })))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Value::Array(out))
    }

    pub fn pending_competitive_update_for_section_json(
        &self,
        project: &str,
        section_key: &str,
    ) -> Result<Value> {
        let c = self.conn()?;
        let row=c.query_row(r#"SELECT csu.id,csu.event_id,csu.base_version_id,csu.proposed_version_id,e.from_run_id,e.to_run_id,e.summary,e.delta_json,e.created_at
          FROM competitive_section_updates csu JOIN competitive_update_events e ON e.id=csu.event_id
          WHERE csu.project_id=?1 AND csu.section_key=?2 AND csu.status='pending' ORDER BY csu.event_id DESC LIMIT 1"#,params![project,section_key],|r|{
            let raw=r.get::<_,String>(7)?;Ok(json!({"id":r.get::<_,i64>(0)?,"event_id":r.get::<_,i64>(1)?,"base_version":r.get::<_,i64>(2)?,"proposed_version":r.get::<_,i64>(3)?,"from_run_id":r.get::<_,Option<i64>>(6)?,"to_run_id":r.get::<_,i64>(5)?,"summary":r.get::<_,String>(6)?,"delta":serde_json::from_str::<Value>(&raw).unwrap_or(json!({})),"created_at":r.get::<_,String>(8)?}))
        }).optional()?;
        Ok(row.unwrap_or_else(|| json!({})))
    }

    pub fn competitive_updates_json(&self, project: &str, limit: usize) -> Result<Value> {
        let c = self.conn()?;
        let cap = limit.clamp(1, 100) as i64;
        let mut st=c.prepare(r#"SELECT e.id,e.from_run_id,e.to_run_id,e.refresh_reason_json,e.delta_json,e.summary,e.material,e.text_refresh_status,e.text_refresh_errors_json,e.created_at,e.processed_at,
          (SELECT COUNT(*) FROM competitive_section_updates s WHERE s.event_id=e.id) section_updates,
          (SELECT COUNT(*) FROM competitive_section_updates s WHERE s.event_id=e.id AND s.status='pending') pending_updates
          FROM competitive_update_events e WHERE e.project_id=?1 ORDER BY e.id DESC LIMIT ?2"#)?;
        let rows=st.query_map(params![project,cap],|r|{let rr=r.get::<_,String>(3)?;let delta=r.get::<_,String>(4)?;let errors=r.get::<_,String>(8)?;Ok(json!({"event_id":r.get::<_,i64>(0)?,"from_run_id":r.get::<_,Option<i64>>(1)?,"to_run_id":r.get::<_,i64>(2)?,"refresh_reason":serde_json::from_str::<Value>(&rr).unwrap_or(json!([])),"delta":serde_json::from_str::<Value>(&delta).unwrap_or(json!({})),"summary":r.get::<_,String>(5)?,"material":r.get::<_,i64>(6)?!=0,"text_refresh_status":r.get::<_,String>(7)?,"text_refresh_errors":serde_json::from_str::<Value>(&errors).unwrap_or(json!([])),"created_at":r.get::<_,String>(9)?,"processed_at":r.get::<_,Option<String>>(10)?,"section_updates":r.get::<_,i64>(11)?,"pending_updates":r.get::<_,i64>(12)?}))})?;
        let mut events = Vec::new();
        for row in rows {
            events.push(row?);
        }
        Ok(json!({
            "pending":self.competitive_pending_update_count(project)?,
            "processing_pending":self.competitive_text_refresh_pending_count(project)?,
            "pending_sections":self.competitive_pending_section_updates_json(project)?,
            "events":events
        }))
    }

    pub fn competitive_ready(&self, project: &str) -> Result<bool> {
        let x = self.competitive_latest_json(project)?;
        Ok(x.get("exists").and_then(Value::as_bool).unwrap_or(false)
            && x.get("fresh").and_then(Value::as_bool).unwrap_or(false)
            && x.get("status").and_then(Value::as_str) == Some("complete"))
    }

    pub fn competitive_context(&self, project: &str, max_chars: usize) -> Result<String> {
        let x = self.competitive_latest_json(project)?;
        if !x.get("fresh").and_then(Value::as_bool).unwrap_or(false) {
            return Ok("COMPETITIVE APPLICANT INTELLIGENCE: not configured or stale for the current grant/clinical design".into());
        }
        let mut compact = json!({"notice":"Potential competitors are capability-overlap candidates inferred from public evidence, not confirmed applicants.","candidates":x.get("candidates").and_then(Value::as_array).map(|a|a.iter().take(12).cloned().collect::<Vec<_>>()).unwrap_or_default(),"strategy":x.get("strategy").cloned().unwrap_or(Value::Null),"provider_status":x.get("provider_status").cloned().unwrap_or(json!([]))});
        // Keep only top public assets explicitly referenced by the strategy/candidates for context efficiency.
        if let Some(obj) = compact.as_object_mut() {
            obj.insert(
                "public_asset_catalog".into(),
                Value::Array(
                    x.get("assets")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().take(80).cloned().collect())
                        .unwrap_or_default(),
                ),
            );
        }
        let mut text = serde_json::to_string_pretty(&compact)?;
        if text.len() > max_chars {
            text.truncate(max_chars);
        }
        Ok(format!(
            "COMPETITIVE APPLICANT INTELLIGENCE (PUBLIC; NOT CONFIRMED APPLICANTS):\n{text}"
        ))
    }

    pub fn opportunity_context(&self, project: &str, max_chars: usize) -> Result<String> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT name,kind,text FROM documents WHERE project_id=?1 AND kind LIKE 'funding_%' ORDER BY id")?;
        let rows = st.query_map([project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = String::new();
        for row in rows {
            let (name, kind, text) = row?;
            out.push_str(&format!("\n--- {kind}: {name} ---\n{text}\n"));
            if out.len() >= max_chars {
                out.truncate(max_chars);
                break;
            }
        }
        Ok(out)
    }

    /// Return each funding-opportunity document as its own immutable source
    /// buffer. Provenance offsets are always relative to one of these buffers,
    /// never to the display-only concatenation produced by opportunity_context.
    pub fn opportunity_documents(&self, project: &str) -> Result<Vec<SourceDocument>> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,name,kind,text,sha256 FROM documents WHERE project_id=?1 AND kind LIKE 'funding_%' ORDER BY id")?;
        let rows = st.query_map([project], |r| {
            Ok(SourceDocument {
                id: r.get(0)?,
                name: r.get(1)?,
                kind: r.get(2)?,
                text: r.get(3)?,
                sha256: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn opportunity_source_fingerprint(&self, project: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        let c = self.conn()?;
        let mut h = Sha256::new();
        h.update(project.as_bytes());
        let mut st=c.prepare("SELECT name,kind,sha256 FROM documents WHERE project_id=?1 AND kind LIKE 'funding_%' ORDER BY id")?;
        let rows = st.query_map([project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut count = 0u64;
        for row in rows {
            let (name, kind, sha) = row?;
            count += 1;
            h.update(name.as_bytes());
            h.update(kind.as_bytes());
            h.update(sha.as_bytes());
        }
        h.update(count.to_le_bytes());
        Ok(hex::encode(h.finalize()))
    }

    fn sync_compliance_required_sections_tx(
        tx: &rusqlite::Transaction<'_>,
        project: &str,
        profile: &ComplianceProfile,
    ) -> Result<()> {
        let required = profile
            .rules
            .iter()
            .filter(|r| {
                r.rule_type == "required_section" && r.mandatory && !r.target.trim().is_empty()
            })
            .map(|r| (section_key(&r.target), r.target.trim().to_string()))
            .collect::<Vec<_>>();
        // Sections introduced solely by an older compliance profile remain visible for audit/history,
        // but stop blocking export when the current sponsor rules no longer require them.
        tx.execute(
            "UPDATE project_sections SET required=0 WHERE project_id=?1 AND origin='compliance'",
            [project],
        )?;
        for (key, title) in required {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT origin FROM project_sections WHERE project_id=?1 AND section_key=?2",
                    params![project, key],
                    |r| r.get(0),
                )
                .optional()?;
            match existing {
                Some(origin) => {
                    if origin == "compliance" {
                        tx.execute("UPDATE project_sections SET title=?1,required=1 WHERE project_id=?2 AND section_key=?3",params![title,project,key])?;
                    } else {
                        tx.execute("UPDATE project_sections SET required=1 WHERE project_id=?1 AND section_key=?2",params![project,key])?;
                    }
                }
                None => {
                    let next:i64=tx.query_row("SELECT COALESCE(MAX(position),-1)+1 FROM project_sections WHERE project_id=?1",[project],|r|r.get(0))?;
                    tx.execute("INSERT INTO project_sections(project_id,section_key,title,position,required,origin) VALUES(?1,?2,?3,?4,1,'compliance')",params![project,key,title,next])?;
                }
            }
        }
        Ok(())
    }

    pub fn compliance_render_fingerprint(&self, project: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.approved_sections_fingerprint(project)?.as_bytes());
        let design = self.design_profile_json(project)?;
        h.update(
            design
                .get("sha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .as_bytes(),
        );
        let clinical = self.clinical_study_json(project)?;
        h.update(
            clinical
                .get("sha256")
                .and_then(Value::as_str)
                .unwrap_or("")
                .as_bytes(),
        );
        Ok(hex::encode(h.finalize()))
    }

    pub fn save_compliance_profile(
        &self,
        project: &str,
        profile: &ComplianceProfile,
        model: &str,
    ) -> Result<Value> {
        crate::compliance::validate_profile(profile)?;
        let documents = self.opportunity_documents(project)?;
        crate::source_locator::validate_exact_sources(profile, &documents)?;
        let source_fp = self.opportunity_source_fingerprint(project)?;
        let raw = serde_json::to_string(profile)?;
        let sha = sha256_hex(raw.as_bytes());
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let version: i64 = tx
            .query_row(
                "SELECT COALESCE(version,0)+1 FROM compliance_profiles WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(1);
        tx.execute("INSERT INTO compliance_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved) VALUES(?1,?2,?3,?4,?5,?6,0)",params![project,version,source_fp,raw,sha,model])?;
        tx.execute(r#"INSERT INTO compliance_profiles(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved,updated_at)
          VALUES(?1,?2,?3,?4,?5,?6,0,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET version=excluded.version,source_fingerprint=excluded.source_fingerprint,profile_json=excluded.profile_json,content_sha256=excluded.content_sha256,model=excluded.model,approved=0,updated_at=CURRENT_TIMESTAMP"#,params![project,version,source_fp,raw,sha,model])?;
        for rule in &profile.rules {
            tx.execute(r#"INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt)
              VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"#,params![project,version,rule.rule_id,rule.source_status,rule.source_hint,rule.source_document_id,rule.source_start_offset.map(|v|v as i64),rule.source_end_offset.map(|v|v as i64),rule.source_page.map(i64::from),rule.source_excerpt])?;
        }
        tx.execute(
            "DELETE FROM compliance_resolutions WHERE project_id=?1",
            [project],
        )?;
        Self::sync_compliance_required_sections_tx(&tx, project, profile)?;
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        Ok(
            json!({"version":version,"sha256":sha,"source_fingerprint":source_fp,"model":model,"approved":false,"profile":profile}),
        )
    }

    pub fn approve_compliance_profile(&self, project: &str) -> Result<Value> {
        let current = self.compliance_profile_json(project)?;
        if !current
            .get("exists")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!("compile the sponsor compliance profile before approval");
        }
        if !current
            .get("fresh")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!("compliance profile is stale because the funding opportunity source changed; recompile it before approval");
        }
        let profile = self
            .compliance_profile_typed(project)?
            .context("compile the sponsor compliance profile before approval")?;
        let unresolved = profile
            .rules
            .iter()
            .filter(|r| r.source_status != "located")
            .map(|r| r.rule_id.as_str())
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            bail!("exact source text was not located for rule(s) {}; correct their source hints and save again before approval",unresolved.join(", "));
        }
        crate::source_locator::validate_exact_sources(
            &profile,
            &self.opportunity_documents(project)?,
        )?;
        let c = self.conn()?;
        c.execute("UPDATE compliance_profiles SET approved=1,updated_at=CURRENT_TIMESTAMP WHERE project_id=?1",[project])?;
        let version = current.get("version").and_then(Value::as_i64).unwrap_or(0);
        c.execute(
            "UPDATE compliance_profile_history SET approved=1 WHERE project_id=?1 AND version=?2",
            params![project, version],
        )?;
        Self::touch_project_conn(&c, project)?;
        self.compliance_profile_json(project)
    }

    pub fn compliance_profile_typed(&self, project: &str) -> Result<Option<ComplianceProfile>> {
        let c = self.conn()?;
        let raw = c
            .query_row(
                "SELECT profile_json FROM compliance_profiles WHERE project_id=?1",
                [project],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        match raw {
            Some(x) => Ok(Some(
                serde_json::from_str(&x).context("stored compliance profile is invalid JSON")?,
            )),
            None => Ok(None),
        }
    }

    pub fn compliance_profile_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let current = self.opportunity_source_fingerprint(project)?;
        let row=c.query_row("SELECT version,source_fingerprint,profile_json,content_sha256,model,approved,updated_at FROM compliance_profiles WHERE project_id=?1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?!=0,r.get::<_,String>(6)?))).optional()?;
        if let Some((version, fp, raw, sha, model, approved, updated_at)) = row {
            let profile: Value = serde_json::from_str(&raw)?;
            Ok(
                json!({"exists":true,"fresh":fp==current,"version":version,"source_fingerprint":fp,"current_fingerprint":current,"sha256":sha,"model":model,"approved":approved,"updated_at":updated_at,"profile":profile}),
            )
        } else {
            Ok(
                json!({"exists":false,"fresh":false,"approved":false,"version":null,"profile":null,"current_fingerprint":current}),
            )
        }
    }

    pub fn resolve_compliance_rule(
        &self,
        project: &str,
        rule_id: &str,
        status: &str,
        notes: &str,
        resolved_by: Option<&str>,
    ) -> Result<Value> {
        if !matches!(
            status,
            "satisfied" | "not_applicable" | "waived" | "unresolved"
        ) {
            bail!("invalid compliance resolution status");
        }
        let profile = self
            .compliance_profile_typed(project)?
            .context("compile compliance profile first")?;
        if !profile.rules.iter().any(|r| r.rule_id == rule_id) {
            bail!("unknown compliance rule {rule_id}");
        }
        let c = self.conn()?;
        if status == "unresolved" {
            c.execute(
                "DELETE FROM compliance_resolutions WHERE project_id=?1 AND rule_id=?2",
                params![project, rule_id],
            )?;
        } else {
            c.execute(r#"INSERT INTO compliance_resolutions(project_id,rule_id,status,notes,resolved_by,created_at) VALUES(?1,?2,?3,?4,?5,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id,rule_id) DO UPDATE SET status=excluded.status,notes=excluded.notes,resolved_by=excluded.resolved_by,created_at=CURRENT_TIMESTAMP"#,params![project,rule_id,status,notes,resolved_by])?;
        }
        Self::touch_project_conn(&c, project)?;
        self.compliance_assessment_json(project)
    }

    pub fn register_submission_artifact(
        &self,
        project: &str,
        slot: &str,
        filename: &str,
        path: &str,
        sha: &str,
        extension: &str,
    ) -> Result<Value> {
        if slot.trim().is_empty() || filename.trim().is_empty() || sha.trim().is_empty() {
            bail!("submission artifact slot, filename, and sha256 are required");
        }
        let workspace = self
            .path
            .parent()
            .context("grant database has no workspace parent")?;
        let expected_root = workspace.join("projects").join(project).join("submission");
        let expected_root = expected_root.canonicalize().unwrap_or(expected_root);
        let artifact = std::path::PathBuf::from(path);
        let resolved = artifact
            .canonicalize()
            .with_context(|| format!("submission artifact does not exist: {path}"))?;
        if !resolved.starts_with(&expected_root) {
            bail!("submission artifact path must be inside the project submission workspace");
        }
        let bytes = std::fs::read(&resolved)?;
        let actual_sha = sha256_hex(&bytes);
        if actual_sha != sha {
            bail!("submission artifact SHA-256 does not match file contents");
        }
        let ext = extension
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        let c = self.conn()?;
        c.execute("INSERT OR IGNORE INTO submission_artifacts(project_id,slot,filename,path,sha256,extension) VALUES(?1,?2,?3,?4,?5,?6)",params![project,slot.trim(),filename.trim(),resolved.to_string_lossy().to_string(),sha,ext])?;
        Self::touch_project_conn(&c, project)?;
        self.submission_artifacts_json(project)
    }

    pub fn submission_artifacts_json(&self, project: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,slot,filename,path,sha256,extension,created_at FROM submission_artifacts WHERE project_id=?1 ORDER BY slot,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"slot":r.get::<_,String>(1)?,"filename":r.get::<_,String>(2)?,"path":r.get::<_,String>(3)?,"sha256":r.get::<_,String>(4)?,"extension":r.get::<_,String>(5)?,"created_at":r.get::<_,String>(6)?})))?;
        let mut out = vec![];
        for row in rows {
            out.push(row?);
        }
        Ok(Value::Array(out))
    }

    pub fn approved_sections_fingerprint(&self, project: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        let c = self.conn()?;
        let mut h = Sha256::new();
        h.update(project.as_bytes());
        let mut st=c.prepare(r#"SELECT ps.section_key,sv.id,sv.body FROM project_sections ps JOIN section_versions sv ON sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1 WHERE ps.project_id=?1 ORDER BY ps.position"#)?;
        let rows = st.query_map([project], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (k, id, b) = row?;
            h.update(k.as_bytes());
            h.update(id.to_le_bytes());
            h.update(b.as_bytes());
        }
        Ok(hex::encode(h.finalize()))
    }

    pub fn save_compliance_measurements(
        &self,
        project: &str,
        measurements: &Value,
    ) -> Result<Value> {
        let fp = self.compliance_render_fingerprint(project)?;
        let raw = serde_json::to_string(measurements)?;
        let c = self.conn()?;
        c.execute(r#"INSERT INTO compliance_measurements(project_id,approved_sections_fingerprint,measurements_json,updated_at) VALUES(?1,?2,?3,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET approved_sections_fingerprint=excluded.approved_sections_fingerprint,measurements_json=excluded.measurements_json,updated_at=CURRENT_TIMESTAMP"#,params![project,fp,raw])?;
        Self::touch_project_conn(&c, project)?;
        self.compliance_assessment_json(project)
    }

    pub fn compliance_assessment_json(&self, project: &str) -> Result<Value> {
        let profile_state = self.compliance_profile_json(project)?;
        let Some(profile) = self.compliance_profile_typed(project)? else {
            return Ok(
                json!({"exists":false,"ready":false,"hard_failures":1,"findings":[],"reason":"compliance profile not compiled"}),
            );
        };
        let c = self.conn()?;
        let mut resolutions = std::collections::HashMap::new();
        {
            let mut st = c.prepare(
                "SELECT rule_id,status,notes FROM compliance_resolutions WHERE project_id=?1",
            )?;
            let rows = st.query_map([project], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (k, s, n) = row?;
                resolutions.insert(k, (s, n));
            }
        }
        let sections = self
            .approved_sections_json(project)?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|x| {
                (
                    x.get("section_key")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    x.get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    x.get("body")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        let artifacts = self
            .submission_artifacts_json(project)?
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|x| {
                (
                    x.get("slot")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    x.get("filename")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    x.get("extension")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        let design = self
            .design_profile_json(project)?
            .get("profile")
            .cloned()
            .unwrap_or(json!({}));
        let current_fp = self.compliance_render_fingerprint(project)?;
        let measurement_row=c.query_row("SELECT approved_sections_fingerprint,measurements_json FROM compliance_measurements WHERE project_id=?1",[project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
        let measurements = measurement_row.and_then(|(fp, raw)| {
            if fp == current_fp {
                serde_json::from_str::<Value>(&raw).ok()
            } else {
                None
            }
        });
        let project_period_months = self.clinical_study_typed(project)?.and_then(|study| {
            study
                .timeline
                .iter()
                .map(|t| t.start_month + t.duration_months)
                .filter(|v| v.is_finite())
                .reduce(f64::max)
        });
        let facts = ComplianceFacts {
            approved_sections: sections,
            artifacts,
            design_profile: design,
            measurements,
            project_period_months,
        };
        let mut result = evaluate_compliance(&profile, &facts, &resolutions);
        if let Some(obj) = result.as_object_mut() {
            obj.insert("exists".into(), Value::Bool(true));
            obj.insert(
                "profile_approved".into(),
                profile_state
                    .get("approved")
                    .cloned()
                    .unwrap_or(Value::Bool(false)),
            );
            obj.insert(
                "profile_fresh".into(),
                profile_state
                    .get("fresh")
                    .cloned()
                    .unwrap_or(Value::Bool(false)),
            );
            obj.insert(
                "profile_version".into(),
                profile_state.get("version").cloned().unwrap_or(Value::Null),
            );
            obj.insert(
                "source_fingerprint".into(),
                profile_state
                    .get("source_fingerprint")
                    .cloned()
                    .unwrap_or(Value::Null),
            );
            let rule_ready = obj.get("ready").and_then(Value::as_bool).unwrap_or(false);
            let approved = profile_state
                .get("approved")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let fresh = profile_state
                .get("fresh")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            obj.insert("ready".into(), Value::Bool(rule_ready && approved && fresh));
        }
        Ok(result)
    }

    pub fn compliance_context(&self, project: &str, max_chars: usize) -> Result<String> {
        let profile = self.compliance_profile_json(project)?;
        let assessment = self.compliance_assessment_json(project)?;
        let mut s =
            serde_json::to_string_pretty(&json!({"profile":profile,"assessment":assessment}))?;
        if s.len() > max_chars {
            s.truncate(max_chars);
        }
        Ok(format!("SPONSOR COMPLIANCE / SUBMISSION RULES:\n{s}"))
    }

    pub fn retrieval_fingerprint(&self, project: &str) -> Result<String> {
        use sha2::{Digest, Sha256};
        let c = self.conn()?;
        let workflow=self.workflow_config(project)?;
        let mut h = Sha256::new();
        h.update(project.as_bytes());
        let mut aggregates=vec![
            ("documents","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM documents WHERE project_id=?1"),
            ("document_chunks","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM document_chunks WHERE project_id=?1"),
            ("requirements","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1"),
            ("evidence","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM evidence WHERE project_id=?1"),
            ("citations","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(verified),0) FROM citations WHERE project_id=?1"),
            ("approved_sections","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM section_versions WHERE project_id=?1 AND approved=1"),
            ("workflow_artifacts","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM workflow_artifacts WHERE project_id=?1"),
            ("workflow_configuration","SELECT COUNT(*),COALESCE(MAX(config_version),0),COALESCE(SUM(config_version),0) FROM project_workflows WHERE project_id=?1")
        ];
        if workflow.enabled("investigator_interview"){aggregates.push(("interview_answers","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM interview_answers WHERE project_id=?1"));}
        if workflow.enabled("clinical_design"){aggregates.push(("clinical_study","SELECT COUNT(*),COALESCE(MAX(version),0),COALESCE(SUM(version),0) FROM clinical_studies WHERE project_id=?1"));}
        if workflow.enabled("competitive_intelligence"){
            aggregates.push(("competitive_runs","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(CASE WHEN status='complete' THEN 1 ELSE 0 END),0) FROM competitive_runs WHERE project_id=?1"));
            aggregates.push(("competitor_candidates","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM competitor_candidates WHERE project_id=?1"));
            aggregates.push(("competitor_assets","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM competitor_assets WHERE project_id=?1"));
        }
        if workflow.enabled("sponsor_compliance"){
            aggregates.push(("compliance_profiles","SELECT COUNT(*),COALESCE(MAX(version),0),COALESCE(SUM(approved),0) FROM compliance_profiles WHERE project_id=?1"));
            aggregates.push(("submission_artifacts","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM submission_artifacts WHERE project_id=?1"));
        }
        for (name, sql) in aggregates {
            let (count, maxid, state): (i64, i64, i64) =
                c.query_row(sql, [project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            h.update(name.as_bytes());
            h.update(count.to_le_bytes());
            h.update(maxid.to_le_bytes());
            h.update(state.to_le_bytes());
        }
        Ok(hex::encode(h.finalize()))
    }

    pub fn retrieval_records(&self, project: &str) -> Result<Vec<RetrievalRecord>> {
        let c = self.conn()?;
        let workflow=self.workflow_config(project)?;
        let mut out = Vec::<RetrievalRecord>::new();
        {
            let mut st=c.prepare("SELECT external_id,requirement,mandatory,status,CAST(strftime('%s',created_at) AS INTEGER) FROM requirements WHERE project_id=?1 AND approved=1 ORDER BY id")?;
            let rows = st.query_map([project], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            for row in rows {
                let (id, text, mandatory, status, created) = row?;
                out.push(RetrievalRecord {
                    row: 0,
                    item_id: format!("requirement:{id}"),
                    kind: "requirement".into(),
                    requirement_id: Some(id.clone()),
                    source_ref: id,
                    source_url: None,
                    source_locator: None,
                    text,
                    confidence: if mandatory { 1.0 } else { 0.8 },
                    status,
                    created_unix: Some(created),
                });
            }
        }
        {
            let mut st=c.prepare("SELECT dc.id,d.name,dc.ordinal,dc.start_word,dc.end_word,dc.text,CAST(strftime('%s',d.created_at) AS INTEGER) FROM document_chunks dc JOIN documents d ON d.id=dc.document_id WHERE dc.project_id=?1 ORDER BY dc.id")?;
            let rows = st.query_map([project], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            })?;
            for row in rows {
                let (id, name, ord, start, end, text, created) = row?;
                out.push(RetrievalRecord {
                    row: 0,
                    item_id: format!("document_chunk:{id}"),
                    kind: "document_chunk".into(),
                    requirement_id: None,
                    source_ref: name,
                    source_url: None,
                    source_locator: Some(format!("chunk {ord}; words {start}-{end}")),
                    text,
                    confidence: 0.75,
                    status: "source_material".into(),
                    created_unix: Some(created),
                });
            }
        }
        {
            let mut st=c.prepare("SELECT id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status,CAST(strftime('%s',created_at) AS INTEGER) FROM evidence WHERE project_id=?1 ORDER BY id")?;
            let rows = st.query_map([project], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, f64>(8)?,
                    r.get::<_, String>(9)?,
                    r.get::<_, i64>(10)?,
                ))
            })?;
            for row in rows {
                let (id, req, kind, src, claim, passage, url, loc, conf, status, created) = row?;
                out.push(RetrievalRecord {
                    row: 0,
                    item_id: format!("evidence:{id}"),
                    kind,
                    requirement_id: req,
                    source_ref: src,
                    source_url: url,
                    source_locator: loc,
                    text: format!("{claim}\n\n{passage}"),
                    confidence: conf as f32,
                    status,
                    created_unix: Some(created),
                });
            }
        }
        if workflow.enabled("investigator_interview"){
            let mut st=c.prepare("SELECT a.id,q.requirement_external_id,q.question,a.value_json,a.confidence,a.classification,a.notes,CAST(strftime('%s',a.created_at) AS INTEGER) FROM interview_answers a JOIN interview_questions q ON q.id=a.question_id WHERE a.project_id=?1 ORDER BY a.id")?;
            let rows = st.query_map([project], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, i64>(7)?,
                ))
            })?;
            for row in rows {
                let (id, req, q, v, confidence, class, notes, created) = row?;
                let conf = match confidence.as_str() {
                    "high" => 0.95,
                    "medium" => 0.7,
                    "low" => 0.45,
                    _ => 0.5,
                };
                let text = format!(
                    "Question: {q}\nAnswer: {v}\nNotes: {}",
                    notes.unwrap_or_default()
                );
                out.push(RetrievalRecord {
                    row: 0,
                    item_id: format!("interview_answer:{id}"),
                    kind: "interview_answer".into(),
                    requirement_id: Some(req),
                    source_ref: format!("interview_answer:{id}"),
                    source_url: None,
                    source_locator: None,
                    text,
                    confidence: conf,
                    status: class,
                    created_unix: Some(created),
                });
            }
        }
        {
            let mut st=c.prepare(r#"SELECT sv.id,sv.section_key,ps.title,sv.body,CAST(strftime('%s',sv.created_at) AS INTEGER) FROM section_versions sv JOIN project_sections ps ON ps.project_id=sv.project_id AND ps.section_key=sv.section_key WHERE sv.project_id=?1 AND sv.approved=1 ORDER BY ps.position"#)?;
            let rows = st.query_map([project], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            for row in rows {
                let (id, key, title, body, created) = row?;
                out.push(RetrievalRecord {
                    row: 0,
                    item_id: format!("approved_section:{id}"),
                    kind: "approved_section".into(),
                    requirement_id: None,
                    source_ref: key,
                    source_url: None,
                    source_locator: None,
                    text: format!("{title}\n\n{body}"),
                    confidence: 1.0,
                    status: "approved".into(),
                    created_unix: Some(created),
                });
            }
        }
        {
            let mut st=c.prepare(r#"SELECT a.id,a.artifact_type,a.version,a.body_json,CAST(strftime('%s',a.approved_at) AS INTEGER)
              FROM workflow_artifacts a WHERE a.project_id=?1 AND a.approved=1 AND a.artifact_type IN ('solicitation_profile','research_framework','aim_set','literature_manifest')
              AND a.version=(SELECT MAX(b.version) FROM workflow_artifacts b WHERE b.project_id=a.project_id AND b.artifact_type=a.artifact_type AND b.approved=1)
              ORDER BY a.artifact_type"#)?;
            let rows=st.query_map([project],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,Option<i64>>(4)?)))?;
            for row in rows{let(id,artifact_type,version,body,created)=row?;out.push(RetrievalRecord{row:0,item_id:format!("workflow_artifact:{id}"),kind:artifact_type.clone(),requirement_id:None,source_ref:format!("{artifact_type}:v{version}"),source_url:None,source_locator:Some(format!("approved workflow artifact version {version}")),text:body,confidence:1.0,status:"approved".into(),created_unix:created});}
        }
        if workflow.enabled("institutional_memory"){
            let raw:Option<String>=c.query_row(r#"SELECT body_json FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='institutional_memory' AND approved=1 ORDER BY version DESC LIMIT 1"#,[project],|row|row.get(0)).optional()?;
            if let Some(raw)=raw{
                let library:crate::workflow_artifacts::InstitutionalMemoryLibrary=serde_json::from_str(&raw).context("approved institutional memory artifact is invalid")?;
                for entry in library.entries{out.push(RetrievalRecord{row:0,item_id:format!("institutional_memory:{}",entry.id),kind:format!("institutional_memory_{}",entry.kind),requirement_id:None,source_ref:entry.origin,source_url:None,source_locator:entry.source_document_id.map(|id|format!("project document {id}")),text:entry.content,confidence:1.0,status:"approved".into(),created_unix:None});}
            }
        }
        if workflow.enabled("clinical_design"){if let Some(study) = self.clinical_study_typed(project)? {
            let assessment =
                crate::clinical::assess(&study, &self.approved_sections_json(project)?);
            let text = format!(
                "Clinical Study Model\n{}\n\nDeterministic Assessment\n{}",
                serde_json::to_string_pretty(&study)?,
                serde_json::to_string_pretty(&assessment)?
            );
            out.push(RetrievalRecord {
                row: 0,
                item_id: "clinical_study:authoritative".into(),
                kind: "clinical_study".into(),
                requirement_id: None,
                source_ref: "clinical_study_model".into(),
                source_url: None,
                source_locator: None,
                text,
                confidence: 1.0,
                status: "authoritative".into(),
                created_unix: Some(time::OffsetDateTime::now_utc().unix_timestamp()),
            });
        }}
        if workflow.enabled("sponsor_compliance"){if let Some(profile) = self.compliance_profile_typed(project)? {
            let assessment = self.compliance_assessment_json(project)?;
            let text = format!(
                "Sponsor Compliance Profile\n{}\n\nDeterministic Compliance Assessment\n{}",
                serde_json::to_string_pretty(&profile)?,
                serde_json::to_string_pretty(&assessment)?
            );
            out.push(RetrievalRecord {
                row: 0,
                item_id: "sponsor_compliance:authoritative".into(),
                kind: "sponsor_compliance".into(),
                requirement_id: None,
                source_ref: "sponsor_compliance_profile".into(),
                source_url: None,
                source_locator: None,
                text,
                confidence: 1.0,
                status: if assessment
                    .get("ready")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "ready".into()
                } else {
                    "needs_attention".into()
                },
                created_unix: Some(time::OffsetDateTime::now_utc().unix_timestamp()),
            });
        }}
        if workflow.enabled("competitive_intelligence")&&self.competitive_ready(project).unwrap_or(false) {
            let competitive = self.competitive_latest_json(project)?;
            if let Some(run_id) = competitive.get("run_id").and_then(Value::as_i64) {
                if let Some(candidates) = competitive.get("candidates").and_then(Value::as_array) {
                    for c in candidates.iter().take(20) {
                        let key = c
                            .get("candidate_key")
                            .and_then(Value::as_str)
                            .unwrap_or("candidate");
                        let name = c.get("name").and_then(Value::as_str).unwrap_or(key);
                        let score = c
                            .get("overall_score")
                            .and_then(Value::as_f64)
                            .unwrap_or(0.0) as f32;
                        out.push(RetrievalRecord {
                            row: 0,
                            item_id: format!("competitive_candidate:{run_id}:{key}"),
                            kind: "competitive_candidate".into(),
                            requirement_id: None,
                            source_ref: name.into(),
                            source_url: None,
                            source_locator: Some(format!("competitive run {run_id}")),
                            text: serde_json::to_string_pretty(c)?,
                            confidence: score.clamp(0.0, 1.0),
                            status: "potential_match_public_evidence".into(),
                            created_unix: Some(time::OffsetDateTime::now_utc().unix_timestamp()),
                        });
                    }
                }
                if let Some(strategy) = competitive.get("strategy").filter(|v| !v.is_null()) {
                    out.push(RetrievalRecord {
                        row: 0,
                        item_id: format!("competitive_strategy:{run_id}"),
                        kind: "competitive_strategy".into(),
                        requirement_id: None,
                        source_ref: "public_competitive_positioning".into(),
                        source_url: None,
                        source_locator: Some(format!("competitive run {run_id}")),
                        text: serde_json::to_string_pretty(strategy)?,
                        confidence: 1.0,
                        status: "public_evidence_strategy".into(),
                        created_unix: Some(time::OffsetDateTime::now_utc().unix_timestamp()),
                    });
                }
            }
        }
        Ok(out)
    }
}

impl GenerationAudit for Store {
    fn workflow_config_for_model(&self, project: &str) -> Result<WorkflowConfig> {
        self.workflow_config(project)
    }

    fn begin_generation(
        &self,
        project: &str,
        task_kind: &str,
        routing_mode: &str,
        provider: &str,
        model: &str,
        prompt_sha256: &str,
        high_value: bool,
        output_contract: Option<&StructuredOutputContract>,
    ) -> Result<String> {
        if task_kind.trim().is_empty() || prompt_sha256.len() != 64 {
            bail!("generation audit requires a task kind and SHA-256 prompt digest");
        }
        let run_id = Uuid::new_v4().to_string();
        let mut c = self.conn()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let input_manifest=Self::generation_input_manifest_conn(&tx,project)?;
        let input_manifest_json=serde_json::to_string(&input_manifest)?;
        let input_manifest_sha256=sha256_hex(input_manifest_json.as_bytes());
        let output_schema_json=output_contract.map(|contract|serde_json::to_string(&contract.schema)).transpose()?;
        tx.execute(
            r#"INSERT INTO generation_runs(
                 id,project_id,task_kind,routing_mode,provider,model,prompt_sha256,input_manifest_json,input_manifest_sha256,high_value,
                 output_contract_name,output_contract_version,output_schema_json,output_schema_sha256,status
               ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,'running')"#,
            params![run_id, project, task_kind, routing_mode, provider, model, prompt_sha256,input_manifest_json,input_manifest_sha256, high_value as i64,output_contract.map(|contract|contract.name.as_str()),output_contract.map(|contract|contract.version as i64),output_schema_json,output_contract.map(|contract|contract.schema_sha256.as_str())],
        )?;
        tx.execute(
            "INSERT INTO workflow_events(project_id,event_type,payload_json) VALUES(?1,'model_generation_started',?2)",
            params![project, serde_json::to_string(&json!({
                "generation_run_id": run_id,
                "task_kind": task_kind,
                "routing_mode": routing_mode,
                "provider": provider,
                "model": model,
                "prompt_sha256": prompt_sha256,
                "input_manifest_sha256":input_manifest_sha256,
                "output_contract":output_contract.map(|contract|json!({"name":contract.name,"version":contract.version,"schema_sha256":contract.schema_sha256})),
                "high_value": high_value
            }))?],
        )?;
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        Ok(run_id)
    }

    fn complete_generation(&self, run_id: &str, response_sha256: &str) -> Result<()> {
        if response_sha256.len() != 64 {
            bail!("generation audit requires a SHA-256 response digest");
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let project: String = tx
            .query_row(
                "SELECT project_id FROM generation_runs WHERE id=?1 AND status='running'",
                [run_id],
                |row| row.get(0),
            )
            .context("running generation audit record not found")?;
        tx.execute(
            "UPDATE generation_runs SET response_sha256=?1,status='complete',completed_at=CURRENT_TIMESTAMP WHERE id=?2 AND status='running'",
            params![response_sha256, run_id],
        )?;
        tx.execute(
            "INSERT INTO workflow_events(project_id,event_type,payload_json) VALUES(?1,'model_generation_completed',?2)",
            params![project, serde_json::to_string(&json!({"generation_run_id":run_id,"response_sha256":response_sha256}))?],
        )?;
        Self::touch_project_conn(&tx, &project)?;
        tx.commit()?;
        Ok(())
    }

    fn fail_generation(&self, run_id: &str, error: &str) -> Result<()> {
        let error = error.chars().take(4000).collect::<String>();
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let project: String = tx
            .query_row(
                "SELECT project_id FROM generation_runs WHERE id=?1 AND status='running'",
                [run_id],
                |row| row.get(0),
            )
            .context("running generation audit record not found")?;
        tx.execute(
            "UPDATE generation_runs SET status='failed',error=?1,completed_at=CURRENT_TIMESTAMP WHERE id=?2 AND status='running'",
            params![error, run_id],
        )?;
        tx.execute(
            "INSERT INTO workflow_events(project_id,event_type,payload_json) VALUES(?1,'model_generation_failed',?2)",
            params![project, serde_json::to_string(&json!({"generation_run_id":run_id,"error":error}))?],
        )?;
        Self::touch_project_conn(&tx, &project)?;
        tx.commit()?;
        Ok(())
    }
}

fn current_competitive_config_sha() -> Result<String> {
    let path = std::env::var("COMPETITIVE_CONFIG_PATH")
        .unwrap_or_else(|_| "/app/config/competitive_intelligence.json".into());
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("read competitive intelligence config {path}"))?;
    let cfg: CompetitiveConfig = serde_json::from_str(&raw)
        .context("parse competitive intelligence config for freshness check")?;
    Ok(sha256_hex(&serde_json::to_vec(&cfg)?))
}

fn section_key(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut underscore = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            underscore = false;
        } else if !underscore && !out.is_empty() {
            out.push('_');
            underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn validate_portable_source_anchors(
    value:&Value,
    documents:&std::collections::BTreeMap<i64,&Value>,
)->Result<()> {
    match value {
        Value::Array(items)=>for item in items{validate_portable_source_anchors(item,documents)?;},
        Value::Object(object)=>{
            if object.contains_key("document_id")&&object.contains_key("document_sha256")&&object.contains_key("excerpt"){
                let id=object.get("document_id").and_then(Value::as_i64).context("portable source anchor document_id must be an integer")?;
                let document=documents.get(&id).context("portable source anchor references a missing document")?;
                let text=document.get("text").and_then(Value::as_str).context("portable source document text is missing")?;
                let sha=document.get("sha256").and_then(Value::as_str).context("portable source document hash is missing")?;
                if object.get("document_sha256").and_then(Value::as_str)!=Some(sha){bail!("portable source anchor document hash does not match its document");}
                let start=object.get("start_offset").and_then(Value::as_u64).context("portable source anchor start_offset is required")? as usize;
                let end=object.get("end_offset").and_then(Value::as_u64).context("portable source anchor end_offset is required")? as usize;
                let excerpt=object.get("excerpt").and_then(Value::as_str).context("portable source anchor excerpt is required")?;
                if end<=start||end>text.len()||text.as_bytes().get(start..end)!=Some(excerpt.as_bytes()){bail!("portable source anchor excerpt is not the exact document byte slice");}
            }
            for child in object.values(){validate_portable_source_anchors(child,documents)?;}
        }
        _=>{}
    }
    Ok(())
}

fn remap_portable_document_ids(value:&mut Value,map:&std::collections::BTreeMap<i64,i64>){
    match value{
        Value::Array(items)=>for item in items{remap_portable_document_ids(item,map);},
        Value::Object(object)=>{
            if object.contains_key("document_sha256")&&object.contains_key("excerpt"){
                if let Some(old)=object.get("document_id").and_then(Value::as_i64){if let Some(new)=map.get(&old){object.insert("document_id".into(),json!(new));}}
            }
            for child in object.values_mut(){remap_portable_document_ids(child,map);}
        }
        _=>{}
    }
}

fn portable_array_object<'a>(object:&'a serde_json::Map<String,Value>,key:&str)->Result<&'a Vec<Value>>{
    object.get(key).and_then(Value::as_array).with_context(||format!("portable project {key} must be an array"))
}

fn portable_str<'a>(value:&'a Value,key:&str)->Result<&'a str>{
    value.get(key).and_then(Value::as_str).filter(|item|!item.is_empty()).with_context(||format!("portable record {key} must be a non-empty string"))
}

fn portable_str_object<'a>(value:&'a serde_json::Map<String,Value>,key:&str)->Result<&'a str>{
    value.get(key).and_then(Value::as_str).filter(|item|!item.is_empty()).with_context(||format!("portable record {key} must be a non-empty string"))
}

fn portable_i64(value:&Value,key:&str)->Result<i64>{
    value.get(key).and_then(Value::as_i64).with_context(||format!("portable record {key} must be an integer"))
}

fn portable_bool(value:&Value,key:&str)->Result<bool>{
    value.get(key).and_then(Value::as_bool).with_context(||format!("portable record {key} must be a boolean"))
}

fn validate_sha256_field(value:&Value,key:&str,nullable:bool)->Result<()> {
    let Some(digest)=value.get(key).and_then(Value::as_str) else {
        if nullable&&value.get(key).is_some_and(Value::is_null){return Ok(());}
        bail!("portable record {key} must be a SHA-256 digest");
    };
    if digest.len()!=64||!digest.bytes().all(|byte|byte.is_ascii_hexdigit()){
        bail!("portable record {key} must be a 64-character hexadecimal SHA-256 digest");
    }
    Ok(())
}

#[cfg(test)]
mod phase6_storage_tests {
    use super::*;
    use crate::competitive_updates::CompetitiveDelta;
    use crate::domain::{RequirementDraft, RequirementsEnvelope};

    fn temp_db(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "grant-core-{name}-{}-{}.db",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        p
    }

    fn approve_test_solicitation(store: &Store, project: &str, purpose: &str) -> Result<i64> {
        let source = "Applicants must include a Research Strategy. Scientific merit is scored.";
        let sha = sha256_hex(source.as_bytes());
        let (document_id, _) = store.add_document(
            project,
            "Funding opportunity",
            "funding_paste",
            source,
            &sha,
        )?;
        if store.requirement_ids(project)?.is_empty() {
            store.replace_requirements(
                project,
                &[RequirementDraft {
                    external_id: "R-001".into(),
                    category: "required_section".into(),
                    requirement: "Applicants must include a Research Strategy.".into(),
                    mandatory: true,
                    evidence_needed: vec!["Research Strategy".into()],
                    dependencies: vec![],
                    source_clue: "Applicants must include a Research Strategy.".into(),
                    source_document: Some("Funding opportunity".into()),
                    source_locator: None,
                }],
            )?;
            store.approve_requirements(project)?;
        }
        let anchor = json!({
            "document_id": document_id,
            "document_sha256": sha,
            "locator": "bytes:0-72",
            "start_offset": 0,
            "end_offset": source.len(),
            "excerpt": source
        });
        let body = json!({
            "schema_version": 1,
            "working_title": "Test grant",
            "sponsor": "Test sponsor",
            "mechanism": "TEST",
            "purpose": purpose,
            "eligibility": [],
            "requirements": [{
                "id": "R-001", "label": "Required section",
                "value": "Research Strategy", "mandatory": true,
                "status": "human_approved", "sources": [anchor.clone()]
            }],
            "review_criteria": [{
                "id": "C-001", "title": "Scientific merit",
                "description": "Scientific merit is scored.", "scored": true,
                "scale": "1-9", "status": "human_approved", "sources": [anchor]
            }],
            "deadlines": [], "budget_rules": [], "attachments": [], "open_questions": []
        });
        let current = store
            .workflow_artifact_json(project, "solicitation_profile")?
            .get("version")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let saved = store.save_workflow_artifact(
            project,
            "solicitation_profile",
            &body,
            "test",
            Some("test-user"),
            Some(current),
        )?;
        let version = saved.get("version").and_then(Value::as_i64).context("version")?;
        store.approve_workflow_artifact(
            project,
            "solicitation_profile",
            version,
            Some("test-user"),
        )?;
        Ok(version)
    }

    fn framework_body(solicitation_version: i64) -> Value {
        json!({
            "schema_version": 1,
            "solicitation_profile_version": solicitation_version,
            "overall_argument": "The proposed work addresses the scored scientific need.",
            "nodes": [
                {
                    "key": "specific_aims", "title": "Specific Aims", "position": 10,
                    "requirement_ids": ["R-001"], "review_criterion_ids": ["C-001"],
                    "narrative_purpose": "State the aims", "key_argument": "The aims address the need",
                    "linked_aim_ids": [], "evidence_needs": ["Scientific premise"],
                    "missing_investigator_inputs": [], "owner_user_id": "test-user",
                    "approver_user_id": "test-user", "target_words": 700, "dependencies": []
                },
                {
                    "key": "research_strategy", "title": "Research Strategy", "position": 20,
                    "requirement_ids": ["R-001"], "review_criterion_ids": ["C-001"],
                    "narrative_purpose": "Explain the strategy", "key_argument": "The design is rigorous",
                    "linked_aim_ids": [], "evidence_needs": ["Feasibility evidence"],
                    "missing_investigator_inputs": [], "owner_user_id": "test-user",
                    "approver_user_id": "test-user", "target_words": 4000,
                    "dependencies": ["specific_aims"]
                }
            ]
        })
    }

    fn approve_test_framework_aims_and_search_plan(store:&Store,project:&str,solicitation_version:i64)->Result<(i64,i64,i64,crate::workflow_artifacts::LiteratureQueryRecord)>{
        let framework=store.save_workflow_artifact(project,"research_framework",&framework_body(solicitation_version),"test",Some("test-user"),Some(0))?;
        let framework_version=framework.get("version").and_then(Value::as_i64).context("framework version")?;
        store.approve_workflow_artifact(project,"research_framework",framework_version,Some("test-user"))?;
        let aims=json!({"schema_version":1,"framework_version":framework_version,"overall_objective":"Determine whether the proposed strategy addresses the need.","central_hypothesis_or_thesis":"The strategy will produce a measurable benefit.","aims":[{"id":"aim_1","title":"Evaluate the strategy","statement":"Evaluate the proposed strategy in the target population.","rationale":"The solicitation requires a rigorous research strategy.","approach_summary":"Use a prespecified comparative analysis.","expected_outcome":"A defensible estimate of the strategy's effect.","impact":"The result will inform future implementation.","innovation":"The work integrates sponsor criteria into the design.","classification":"assumption","dependencies":[],"supporting_evidence_ids":[]}]});
        let aim_set=store.save_workflow_artifact(project,"aim_set",&aims,"test",Some("test-user"),Some(0))?;
        let aim_set_version=aim_set.get("version").and_then(Value::as_i64).context("aim-set version")?;
        store.approve_workflow_artifact(project,"aim_set",aim_set_version,Some("test-user"))?;
        let query=crate::workflow_artifacts::LiteratureQueryRecord{id:"query_primary_evidence".into(),query:"comparative effectiveness target population primary study".into(),rationale:"Resolve the scientific-premise evidence gap.".into(),aim_ids:vec!["aim_1".into()],requirement_ids:vec!["R-001".into()],criterion_ids:vec!["C-001".into()],preferred_domains:vec!["nih.gov".into()]};
        let plan=serde_json::to_value(crate::workflow_artifacts::LiteratureSearchPlan{schema_version:1,solicitation_profile_version:solicitation_version,framework_version,aim_set_version,queries:vec![query.clone()]})?;
        let saved_plan=store.save_workflow_artifact(project,"literature_search_plan",&plan,"test",Some("test-user"),Some(0))?;
        let plan_version=saved_plan.get("version").and_then(Value::as_i64).context("search-plan version")?;
        store.approve_workflow_artifact(project,"literature_search_plan",plan_version,Some("test-user"))?;
        Ok((framework_version,aim_set_version,plan_version,query))
    }

    #[test]
    fn framework_approval_rebuilds_active_sections_without_deleting_version_history() -> Result<()> {
        let path = temp_db("framework-sections");
        let store = Store::open(&path)?;
        let project = "framework-sections-project";
        store.create_project_with_workflow(
            project,
            "Framework",
            None,
            None,
            &["Legacy Outline".into()],
            &store.default_workflow_config()?,
            Some("test-user"),
        )?;
        store.upsert_identity("test-user", "test-org", Some("test@example.org"), "Test user")?;
        store.add_project_member(project, "test-user", "owner", Some("test-user"))?;
        let legacy_version = store.save_section(
            project,
            "legacy_outline",
            "Legacy Outline",
            "Preserved historical prose",
            None,
            "human_edit",
        )?;
        store.approve_section_version(project, "legacy_outline", legacy_version)?;
        let solicitation_version = approve_test_solicitation(&store, project, "First purpose")?;
        let saved = store.save_workflow_artifact(
            project,
            "research_framework",
            &framework_body(solicitation_version),
            "test",
            Some("test-user"),
            Some(0),
        )?;
        let framework_version = saved.get("version").and_then(Value::as_i64).context("version")?;
        store.approve_workflow_artifact(
            project,
            "research_framework",
            framework_version,
            Some("test-user"),
        )?;

        let sections = store.project_sections_json(project)?;
        assert_eq!(sections.pointer("/0/section_key").and_then(Value::as_str), Some("specific_aims"));
        assert_eq!(sections.pointer("/1/section_key").and_then(Value::as_str), Some("research_strategy"));
        assert_eq!(sections.as_array().map(Vec::len), Some(2));
        let history_count: i64 = store.conn()?.query_row(
            "SELECT COUNT(*) FROM section_versions WHERE project_id=?1 AND section_key='legacy_outline'",
            [project],
            |row| row.get(0),
        )?;
        assert_eq!(history_count, 1);
        let active_approval_count: i64 = store.conn()?.query_row(
            "SELECT COUNT(*) FROM section_versions WHERE project_id=?1 AND section_key='legacy_outline' AND approved=1",
            [project],
            |row| row.get(0),
        )?;
        assert_eq!(active_approval_count, 0);
        let approval_history_count: i64 = store.conn()?.query_row(
            "SELECT COUNT(*) FROM approvals WHERE project_id=?1 AND section_key='legacy_outline' AND version_id=?2",
            params![project, legacy_version],
            |row| row.get(0),
        )?;
        assert_eq!(approval_history_count, 1);

        let newer_solicitation = approve_test_solicitation(&store, project, "Revised purpose")?;
        assert_eq!(newer_solicitation, solicitation_version + 1);
        let status = store.workflow_status_json(project)?;
        let framework = status
            .get("steps")
            .and_then(Value::as_array)
            .and_then(|steps| steps.iter().find(|step| step.get("key").and_then(Value::as_str) == Some("framework")))
            .context("framework status")?;
        assert_eq!(framework.get("status").and_then(Value::as_str), Some("awaiting_review"));

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn human_returns_are_exact_version_scoped_and_preserve_approval_history() -> Result<()> {
        let path=temp_db("human-return-for-revision");
        let store=Store::open(&path)?;
        store.upsert_identity("test-user","test-org",Some("test@example.org"),"Test user")?;
        let project="human-return-for-revision-project";
        store.create_project_with_workflow(
            project,"Human control",None,None,&["Specific Aims".into()],
            &store.default_workflow_config()?,Some("test-user"),
        )?;
        store.add_project_member(project,"test-user","owner",None)?;
        let solicitation_version=approve_test_solicitation(&store,project,"Human-controlled purpose")?;
        let section_version=store.save_section_by(
            project,"specific_aims","Specific Aims","Approved narrative",None,"human_edit",Some("test-user"),
        )?;
        store.approve_section_version_by(project,"specific_aims",section_version,Some("test-user"))?;

        let section_return=store.return_section_for_revision(
            project,"specific_aims",section_version,"test-user","The outcome statement needs investigator correction.",
        )?;
        assert_eq!(section_return.get("decision").and_then(Value::as_str),Some("returned_for_revision"));
        assert_eq!(store.section_state_json(project,"specific_aims")?.pointer("/latest/approved").and_then(Value::as_bool),Some(false));
        assert!(store.return_section_for_revision(project,"specific_aims",section_version,"test-user","Repeat return").is_err());
        store.approve_section_version_by(project,"specific_aims",section_version,Some("test-user"))?;

        let artifact_return=store.return_workflow_artifact_for_revision(
            project,"solicitation_profile",solicitation_version,"test-user","Sponsor criterion mapping must be corrected.",
        )?;
        assert_eq!(artifact_return.get("approved").and_then(Value::as_bool),Some(false));
        assert_eq!(store.section_state_json(project,"specific_aims")?.pointer("/latest/approved").and_then(Value::as_bool),Some(false));
        assert!(store.return_workflow_artifact_for_revision(project,"solicitation_profile",solicitation_version,"test-user","Repeat return").is_err());

        let c=store.conn()?;
        let artifact_events:i64=c.query_row(
            "SELECT COUNT(*) FROM artifact_approval_events WHERE project_id=?1 AND artifact_type='solicitation_profile' AND artifact_version=?2 AND decision IN ('approved','rejected')",
            params![project,solicitation_version],|row|row.get(0),
        )?;
        assert_eq!(artifact_events,2);
        let section_decisions:i64=c.query_row(
            "SELECT COUNT(*) FROM approvals WHERE project_id=?1 AND section_key='specific_aims' AND version_id=?2 AND decision IN ('approved','rejected')",
            params![project,section_version],|row|row.get(0),
        )?;
        assert_eq!(section_decisions,3);
        let _=std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn artifact_approval_rejects_unknown_and_cross_project_references() -> Result<()> {
        let path = temp_db("artifact-reference-integrity");
        let store = Store::open(&path)?;
        let project = "artifact-reference-project";
        store.create_project_with_workflow(project,"Reference integrity",None,None,&[],&store.default_workflow_config()?,Some("test-user"))?;
        store.upsert_identity("test-user","test-org",Some("test@example.org"),"Test user")?;
        store.add_project_member(project,"test-user","owner",Some("test-user"))?;
        let solicitation_version=approve_test_solicitation(&store,project,"Validate scoped references")?;

        let mut invalid=framework_body(solicitation_version);
        invalid["nodes"][0]["requirement_ids"]=json!(["R-DOES-NOT-EXIST"]);
        let saved=store.save_workflow_artifact(project,"research_framework",&invalid,"test",Some("test-user"),Some(0))?;
        let version=saved.get("version").and_then(Value::as_i64).context("version")?;
        assert!(store.approve_workflow_artifact(project,"research_framework",version,Some("test-user")).is_err());

        let mut nonmember=framework_body(solicitation_version);
        nonmember["nodes"][0]["owner_user_id"]=json!("outside-user");
        let saved=store.save_workflow_artifact(project,"research_framework",&nonmember,"test",Some("test-user"),Some(version))?;
        let version=saved.get("version").and_then(Value::as_i64).context("version")?;
        assert!(store.approve_workflow_artifact(project,"research_framework",version,Some("test-user")).is_err());

        let context=store.workflow_editor_context_json(project)?;
        assert_eq!(context.pointer("/approved_artifacts/solicitation_profile/version").and_then(Value::as_i64),Some(solicitation_version));
        assert_eq!(context.pointer("/members/0/user_id").and_then(Value::as_str),Some("test-user"));
        let _=std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn research_run_commit_is_atomic_and_bound_to_the_approved_plan()->Result<()>{
        let path=temp_db("atomic-research-run");let store=Store::open(&path)?;let project="atomic-research-project";
        store.create_project_with_workflow(project,"Atomic research",None,None,&[],&store.default_workflow_config()?,Some("test-user"))?;
        store.upsert_identity("test-user","test-org",Some("test@example.org"),"Test user")?;
        store.add_project_member(project,"test-user","owner",Some("test-user"))?;
        let solicitation_version=approve_test_solicitation(&store,project,"Test atomic research")?;
        let(framework_version,aim_set_version,plan_version,query)=approve_test_framework_aims_and_search_plan(&store,project,solicitation_version)?;

        let failed_run=store.begin_research_run(project,plan_version,"test_provider","test-user","2026-08-23T12:00:00Z")?;
        let mut wrong_query=query.clone();wrong_query.id="not_in_the_approved_plan".into();
        let invalid=StagedResearchRun{id:failed_run.clone(),search_plan_version:plan_version,solicitation_profile_version:solicitation_version,framework_version,aim_set_version,search_provider:"test_provider".into(),started_at:"2026-08-23T12:00:00Z".into(),completed_at:"2026-08-23T12:01:00Z".into(),queries:vec![StagedResearchQuery{query:wrong_query,terminal_status:"complete_no_sources".into(),sources:vec![]}],failures:vec![]};
        assert!(store.finalize_research_run_atomic(project,&invalid).is_err());
        let c=store.conn()?;
        for table in ["research_queries","research_sources","evidence","citations"]{let sql=format!("SELECT COUNT(*) FROM {table} WHERE project_id=?1");let count:i64=c.query_row(&sql,[project],|row|row.get(0))?;assert_eq!(count,0,"{table} must roll back with the failed run");}
        drop(c);
        store.fail_research_run(project,&failed_run,&["approved-plan mismatch".into()],"2026-08-23T12:01:00Z")?;

        let run_id=store.begin_research_run(project,plan_version,"test_provider","test-user","2026-08-23T13:00:00Z")?;
        let source_text="The comparative study found a measurable benefit in the target population.";
        let run=StagedResearchRun{id:run_id.clone(),search_plan_version:plan_version,solicitation_profile_version:solicitation_version,framework_version,aim_set_version,search_provider:"test_provider".into(),started_at:"2026-08-23T13:00:00Z".into(),completed_at:"2026-08-23T13:01:00Z".into(),queries:vec![StagedResearchQuery{query,terminal_status:"complete".into(),sources:vec![StagedResearchSource{source:FetchedSource{title:"Primary comparative study".into(),url:"https://example.org/primary-study".into(),text:source_text.into(),retrieved_at:"2026-08-23T13:00:30Z".into(),sha256:sha256_hex(source_text.as_bytes()),status:200},validation_status:"supported".into(),confidence:0.94,supporting_excerpt:source_text.into(),explanation:"The exact excerpt directly supports the evidence need.".into()}]}],failures:vec![]};
        let artifact=store.finalize_research_run_atomic(project,&run)?;
        assert_eq!(artifact.pointer("/body/schema_version").and_then(Value::as_i64),Some(2));
        assert_eq!(artifact.pointer("/body/search_plan_version").and_then(Value::as_i64),Some(plan_version));
        assert_eq!(artifact.pointer("/body/evidence_needs/0/disposition").and_then(Value::as_str),Some("supported"));
        let c=store.conn()?;
        for table in ["research_queries","research_sources","evidence","citations"]{let sql=format!("SELECT COUNT(*) FROM {table} WHERE project_id=?1");let count:i64=c.query_row(&sql,[project],|row|row.get(0))?;assert_eq!(count,1,"{table} should commit exactly once");}
        let status:String=c.query_row("SELECT status FROM research_runs WHERE id=?1",[&run_id],|row|row.get(0))?;assert_eq!(status,"complete");drop(c);
        let _=std::fs::remove_file(path);Ok(())
    }

    #[test]
    fn idempotency_keys_are_bound_to_the_exact_operation_and_request_payload() -> Result<()> {
        let path = temp_db("idempotency-payload");
        let store = Store::open(&path)?;
        store.upsert_identity("test-user","test-org",Some("test@example.org"),"Test user")?;
        let key="stable-request-key";
        let first_sha=sha256_hex(br#"{"title":"one"}"#);
        let second_sha=sha256_hex(br#"{"title":"two"}"#);
        assert!(matches!(
            store.claim_idempotency("test-user",key,"POST","/api/projects",&first_sha)?,
            IdempotencyClaim::New
        ));
        assert!(matches!(
            store.claim_idempotency("test-user",key,"POST","/api/projects",&first_sha)?,
            IdempotencyClaim::InProgress
        ));
        store.complete_idempotency("test-user",key,201,"application/json",br#"{"id":"p1"}"#)?;
        assert!(matches!(
            store.claim_idempotency("test-user",key,"POST","/api/projects",&first_sha)?,
            IdempotencyClaim::Replay{status_code:201,..}
        ));
        assert!(matches!(
            store.claim_idempotency("test-user",key,"POST","/api/projects",&second_sha)?,
            IdempotencyClaim::Conflict
        ));
        assert!(matches!(
            store.claim_idempotency("test-user",key,"POST","/api/projects/other",&first_sha)?,
            IdempotencyClaim::Conflict
        ));
        let _=std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn collaboration_tasks_enforce_ownership_and_expose_dependencies_and_routing() -> Result<()> {
        let path=temp_db("collaboration-workspace");let store=Store::open(&path)?;
        for(id,email,name) in [("owner-user","owner@example.org","Owner"),("viewer-user","viewer@example.org","Viewer"),("reviewer-user","reviewer@example.org","Reviewer")]{store.upsert_identity(id,"test-org",Some(email),name)?;}
        let mut workflow=store.default_workflow_config()?;workflow.enabled_modules.push("team_collaboration".into());
        let project="collaboration-workspace-project";
        store.create_project_with_workflow(project,"Collaboration",None,None,&["Specific Aims".into()],&workflow,Some("owner-user"))?;
        store.add_project_member(project,"owner-user","owner",None)?;
        store.add_project_member(project,"viewer-user","viewer",Some("owner-user"))?;
        store.add_project_member(project,"reviewer-user","reviewer",Some("owner-user"))?;
        let first=store.create_task(project,"Collect biosketches","","viewer-user","human","high",None,"owner-user",&[])?;
        let first_id=first.get("id").and_then(Value::as_str).context("first task ID")?.to_owned();
        let second=store.create_task(project,"Verify attachments","","owner-user","human","normal",None,"owner-user",std::slice::from_ref(&first_id))?;
        let second_id=second.get("id").and_then(Value::as_str).context("second task ID")?;
        assert!(store.create_task(project,"Invalid due date","","owner-user","human","normal",Some("not-a-date"),"owner-user",&[]).is_err());
        let overdue=store.create_task(project,"Resolve blocked review","Waiting on an assigned methodological correction.","owner-user","human","critical",Some("2000-01-01T00:00:00Z"),"owner-user",&[])?;
        let overdue_id=overdue.get("id").and_then(Value::as_str).context("overdue task ID")?.to_owned();
        store.update_task_status(project,&overdue_id,"blocked","owner-user","owner")?;
        let tasks=store.tasks_json(project)?;
        let dependent=tasks.as_array().and_then(|items|items.iter().find(|item|item.get("id").and_then(Value::as_str)==Some(second_id))).context("dependent task")?;
        assert_eq!(dependent.pointer("/dependencies/0").and_then(Value::as_str),Some(first_id.as_str()));
        store.update_task_status(project,&first_id,"in_progress","viewer-user","viewer")?;
        assert!(store.update_task_status(project,&first_id,"complete","reviewer-user","reviewer").is_err());
        store.update_task_status(project,&first_id,"complete","owner-user","owner")?;
        let section_version=store.save_section_by(project,"specific_aims","Specific Aims","Draft grant narrative",None,"human_edit",Some("owner-user"))?;
        let comment=store.add_comment(project,"section","specific_aims",section_version,Some(0),Some(5),Some("Draft"),"owner-user","Please replace this opening.",None,&[])?;
        assert!(comment.get("id").and_then(Value::as_i64).is_some());
        assert!(store.add_comment(project,"section","specific_aims",section_version,Some(0),Some(5),Some("Wrong"),"owner-user","Invalid quote",None,&[]).is_err());
        let routing=json!({"schema_version":1,"project_owner_user_id":"owner-user","routes":[{"artifact_type":"proposal_section","owner_user_id":"owner-user","approver_user_ids":["owner-user"],"minimum_approvals":1}]});
        let saved=store.save_workflow_artifact(project,"collaboration_record",&routing,"test",Some("owner-user"),Some(0))?;
        store.approve_workflow_artifact(project,"collaboration_record",saved.get("version").and_then(Value::as_i64).context("routing version")?,Some("owner-user"))?;
        let status=store.approval_routing_status_json(project)?;
        assert_eq!(status.get("configured").and_then(Value::as_bool),Some(true));
        assert_eq!(status.pointer("/routes/0/artifact_type").and_then(Value::as_str),Some("section:specific_aims"));
        let health=store.project_health_json(project)?;
        assert_eq!(health.get("state").and_then(Value::as_str),Some("critical"));
        let issue_codes:std::collections::BTreeSet<&str>=health.get("issues").and_then(Value::as_array).context("health issues")?.iter().filter_map(|item|item.get("code").and_then(Value::as_str)).collect();
        assert!(issue_codes.contains(format!("blocked_task_{overdue_id}").as_str()));
        assert!(issue_codes.contains(format!("overdue_task_{overdue_id}").as_str()));
        assert!(issue_codes.contains("open_version_comments"));
        let _=std::fs::remove_file(path);Ok(())
    }

    #[test]
    fn section_edits_compare_merge_and_restore_with_atomic_lineage() -> Result<()> {
        let path=temp_db("section-versioning");let store=Store::open(&path)?;
        store.upsert_identity("editor-user","test-org",Some("editor@example.org"),"Editor")?;
        let project="section-versioning-project";
        store.create_project_with_workflow(project,"Versioning",None,None,&["Specific Aims".into()],&store.default_workflow_config()?,Some("editor-user"))?;
        let first=store.save_section_by(project,"specific_aims","Specific Aims","one\ntwo\nthree\n",None,"human_edit",Some("editor-user"))?;
        let second=store.save_section_edit(project,"specific_aims","Specific Aims","ONE\ntwo\nthree\n",None,Some(first),"editor-user")?;
        assert!(store.save_section_edit(project,"specific_aims","Specific Aims","stale",None,Some(first),"editor-user").is_err());
        let history=store.section_versions_json(project,"specific_aims")?;
        assert_eq!(history.pointer("/0/base_version_id").and_then(Value::as_i64),Some(first));
        let comparison=store.section_compare_json(project,"specific_aims",first,second)?;
        assert_eq!(comparison.pointer("/from/body").and_then(Value::as_str),Some("one\ntwo\nthree\n"));
        let merge=store.section_merge_preview_json(project,"specific_aims",first,second,"one\ntwo\nTHREE\n")?;
        assert_eq!(merge.get("clean").and_then(Value::as_bool),Some(true));
        assert_eq!(merge.get("merged_body").and_then(Value::as_str),Some("ONE\ntwo\nTHREE\n"));
        let restored=store.restore_section_version(project,"specific_aims",first,second,Some("editor-user"))?;
        let restored_record=store.section_version_json(project,"specific_aims",restored)?;
        assert_eq!(restored_record.get("base_version_id").and_then(Value::as_i64),Some(second));
        assert_eq!(restored_record.get("restored_from_version_id").and_then(Value::as_i64),Some(first));
        assert_eq!(restored_record.get("approved").and_then(Value::as_bool),Some(false));
        let restored_events:i64=store.conn()?.query_row("SELECT COUNT(*) FROM workflow_events WHERE project_id=?1 AND event_type='section_version_restored'",[project],|row|row.get(0))?;
        assert_eq!(restored_events,1);
        let _=std::fs::remove_file(path);Ok(())
    }

    #[test]
    fn portable_project_round_trip_validates_hashes_and_preserves_core_history() -> Result<()> {
        let path=temp_db("portable-round-trip");
        let store=Store::open(&path)?;
        store.upsert_identity("owner-user","test-org",Some("owner@example.org"),"Owner")?;
        let source_project="portable-source";
        store.create_project_with_workflow(source_project,"Portable grant",Some("Sponsor"),Some("TEST"),&["Specific Aims".into()],&store.default_workflow_config()?,Some("owner-user"))?;
        let text="Applicants must provide Specific Aims.";let sha=sha256_hex(text.as_bytes());
        store.add_document(source_project,"NOFO","funding_opportunity",text,&sha)?;
        store.replace_requirements(source_project,&[RequirementDraft{external_id:"R-001".into(),category:"section".into(),requirement:text.into(),mandatory:true,evidence_needed:vec!["Specific Aims".into()],dependencies:vec![],source_clue:text.into(),source_document:Some("NOFO".into()),source_locator:None}])?;
        store.approve_requirements(source_project)?;
        let generated_body="Approved exact generated prose";
        let generation_run=store.begin_generation(source_project,"section_draft","hybrid","claude","test-model",&sha256_hex(b"immutable prompt"),true,None)?;
        store.complete_generation(&generation_run,&sha256_hex(generated_body.as_bytes()))?;
        let version=store.save_generated_section(source_project,"specific_aims","Specific Aims",generated_body,None,"claude:test-model",&generation_run,None,Some("owner-user"))?;
        store.approve_section_version_by(source_project,"specific_aims",version,Some("owner-user"))?;
        let contract=StructuredOutputContract::for_type::<RequirementsEnvelope>("requirements_envelope",1)?;
        let structured_response=r#"{"requirements":[]}"#;
        let structured_run=store.begin_generation(source_project,"requirement_decomposition","hybrid","claude","test-model",&sha256_hex(b"structured prompt"),true,Some(&contract))?;
        store.complete_generation(&structured_run,&sha256_hex(structured_response.as_bytes()))?;
        let package=store.portable_project_package(source_project)?;
        assert_eq!(package.get("schema_version").and_then(Value::as_u64),Some(2));
        assert_eq!(store.validate_portable_project_package(&package)?.get("valid").and_then(Value::as_bool),Some(true));
        let imported=store.import_portable_project_package(&package,"owner-user")?;
        let imported_id=imported.get("id").and_then(Value::as_str).context("imported project ID")?;
        assert_ne!(imported_id,source_project);
        assert_eq!(store.project_json(imported_id)?.get("title").and_then(Value::as_str),Some("Portable grant"));
        assert_eq!(store.section_versions_json(imported_id,"specific_aims")?.as_array().map(Vec::len),Some(1));
        assert_eq!(store.approved_sections_json(imported_id)?.pointer("/0/body").and_then(Value::as_str),Some(generated_body));
        let imported_version=store.section_versions_json(imported_id,"specific_aims")?.pointer("/0/version").and_then(Value::as_i64).context("imported version")?;
        let imported_lineage=store.section_version_json(imported_id,"specific_aims",imported_version)?;
        let imported_run_id=imported_lineage.get("generation_run_id").and_then(Value::as_str).context("imported generation lineage")?;
        assert_ne!(imported_run_id,generation_run);
        let imported_run=store.generation_run_json(imported_id,imported_run_id)?;
        assert_eq!(imported_run.get("response_sha256").and_then(Value::as_str),Some(sha256_hex(generated_body.as_bytes()).as_str()));
        assert_eq!(imported_run.pointer("/input_manifest/schema_version").and_then(Value::as_i64),Some(1));
        let imported_runs=store.generation_runs_json(imported_id,10)?;
        let imported_structured=imported_runs.as_array().context("imported runs")?.iter().find(|run|run.get("output_contract_name").and_then(Value::as_str)==Some("requirements_envelope")).context("imported structured run")?;
        let imported_structured_id=imported_structured.get("id").and_then(Value::as_str).context("imported structured run ID")?;
        let imported_structured=store.generation_run_json(imported_id,imported_structured_id)?;
        assert_eq!(imported_structured.get("output_schema_sha256").and_then(Value::as_str),Some(contract.schema_sha256.as_str()));
        assert_eq!(imported_structured.pointer("/output_schema/type").and_then(Value::as_str),Some("object"));
        let imported_source:String=store.conn()?.query_row("SELECT text FROM documents WHERE project_id=?1 AND kind='funding_opportunity'",[imported_id],|row|row.get(0))?;
        assert_eq!(imported_source,text);
        let mut tampered=package.clone();tampered["payload"]["project"]["title"]=json!("Tampered");
        assert!(store.validate_portable_project_package(&tampered).is_err());
        let mut lineage_tampered=package.clone();
        let manifest=lineage_tampered.pointer("/payload/generation_runs/0/input_manifest_json").and_then(Value::as_str).context("generation manifest")?.to_owned();
        lineage_tampered["payload"]["generation_runs"][0]["input_manifest_json"]=json!(format!("{manifest} "));
        lineage_tampered["payload_sha256"]=json!(sha256_hex(&serde_json::to_vec(lineage_tampered.get("payload").context("payload")?)?));
        assert!(store.validate_portable_project_package(&lineage_tampered).is_err());
        let _=std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn generated_sections_require_exact_complete_audit_lineage_and_current_base() -> Result<()> {
        let path=temp_db("generated-section-lineage");
        let store=Store::open(&path)?;
        let project="generated-section-project";
        store.create_project_with_workflow(project,"Generated lineage",None,None,&["Specific Aims".into()],&store.default_workflow_config()?,None)?;
        let first=store.save_section_by(project,"specific_aims","Specific Aims","Human base",None,"human_edit",None)?;
        let body="Model result";
        let run=store.begin_generation(project,"section_draft","local_only","ollama","qwen3:1.7b",&sha256_hex(b"prompt"),false,None)?;
        store.complete_generation(&run,&sha256_hex(body.as_bytes()))?;
        assert!(store.save_generated_section(project,"specific_aims","Specific Aims","different bytes",None,"ollama:qwen3:1.7b",&run,Some(first),None).is_err());
        let concurrent=store.save_section_edit(project,"specific_aims","Specific Aims","Concurrent human change",None,Some(first),"editor")?;
        assert!(store.save_generated_section(project,"specific_aims","Specific Aims",body,None,"ollama:qwen3:1.7b",&run,Some(first),None).is_err());
        let generated=store.save_generated_section(project,"specific_aims","Specific Aims",body,None,"ollama:qwen3:1.7b",&run,Some(concurrent),None)?;
        let record=store.section_version_json(project,"specific_aims",generated)?;
        assert_eq!(record.get("generation_run_id").and_then(Value::as_str),Some(run.as_str()));
        assert_eq!(record.get("base_version_id").and_then(Value::as_i64),Some(concurrent));
        let audit=store.generation_run_json(project,&run)?;
        let manifest_raw=serde_json::to_string(audit.get("input_manifest").context("input manifest")?)?;
        assert_eq!(audit.get("input_manifest_sha256").and_then(Value::as_str),Some(sha256_hex(manifest_raw.as_bytes()).as_str()));
        let _=std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn disabled_modules_never_appear_in_workflow_steps_or_blockers() -> Result<()> {
        let path = temp_db("lean-workflow-gates");
        let store = Store::open(&path)?;
        let workflow = store.default_workflow_config()?;
        let project = "lean-project";
        store.create_project_with_workflow(
            project,
            "Lean",
            None,
            None,
            &[],
            &workflow,
            Some("test-user"),
        )?;
        let status = store.workflow_status_json(project)?;
        let steps = status
            .get("steps")
            .and_then(Value::as_array)
            .context("steps missing")?;
        assert_eq!(steps.len(), 5);
        assert!(steps.iter().all(|step| step.get("optional").is_none()));
        let blockers = status
            .get("blockers")
            .and_then(Value::as_array)
            .context("blockers missing")?;
        assert!(blockers
            .iter()
            .all(|blocker| blocker.get("module").is_none()));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn removing_a_module_preserves_its_artifact_history() -> Result<()> {
        let path = temp_db("module-history");
        let store = Store::open(&path)?;
        let mut workflow = store.default_workflow_config()?;
        workflow.enabled_modules.push("opportunity_fit".into());
        let project = "module-history-project";
        store.create_project_with_workflow(
            project,
            "History",
            None,
            None,
            &[],
            &workflow,
            Some("test-user"),
        )?;
        store.save_workflow_artifact(
            project,
            "opportunity_fit",
            &json!({"schema_version":1,"solicitation_profile_version":1,
              "dimensions":[{"key":"mission","disposition":"unknown","rationale":"Awaiting sponsor evidence","sources":[]}],
              "decision":"hold","decision_rationale":"Awaiting review","decided_by_user_id":"test-user"}),
            "human",
            Some("test-user"),
            Some(0),
        )?;
        let mut proposed = workflow.clone();
        proposed.enabled_modules.clear();
        let impact = store.workflow_impact_json(project, &proposed)?;
        assert_eq!(
            impact
                .pointer("/preserved_hidden_history/0/module")
                .and_then(Value::as_str),
            Some("opportunity_fit")
        );
        store.update_workflow_config(project, &proposed, 1, "test-user")?;
        assert_eq!(
            store
                .workflow_artifact_json(project, "opportunity_fit")?
                .get("version")
                .and_then(Value::as_i64),
            Some(1)
        );
        assert!(store
            .workflow_status_json(project)?
            .get("steps")
            .and_then(Value::as_array)
            .is_some_and(|steps| !steps
                .iter()
                .any(|step| step.get("key").and_then(Value::as_str) == Some("opportunity_fit"))));
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn database_enforces_exact_compliance_source_bytes() -> Result<()> {
        let path = temp_db("compliance-source-trigger");
        let store = Store::open(&path)?;
        let project = "project-source-trigger";
        store.create_project(project, "Test", None, None, &[])?;
        let source = "Préface\nThe Research Strategy may not exceed 12 pages.";
        let (document_id, _) = store.add_document(
            project,
            "Pasted funding opportunity",
            "funding_paste",
            source,
            "source-sha",
        )?;
        let start = source.find("The Research").unwrap();
        let end = source.len();
        let c = store.conn()?;
        c.execute("INSERT INTO compliance_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved) VALUES(?1,1,'fp','{}','sha','test',0)",[project])?;
        c.execute(r#"INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt)
          VALUES(?1,1,'C-001','located','Research Strategy page limitation',?2,?3,?4,NULL,?5)"#,params![project,document_id,start as i64,end as i64,&source[start..end]])?;
        let error=c.execute(r#"INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt)
          VALUES(?1,1,'C-002','located','Research Strategy page limitation',?2,?3,?4,NULL,'The Research Strategy must not exceed twelve pages.')"#,params![project,document_id,start as i64,end as i64]).unwrap_err();
        assert!(error.to_string().contains("exact document byte slice"));
        drop(c);
        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn competitive_proposal_never_overwrites_human_approved_version() -> Result<()> {
        let path = temp_db("competitive-protection");
        let store = Store::open(&path)?;
        let project = "project-test";
        store.create_project(
            project,
            "Test",
            Some("Sponsor"),
            Some("R01"),
            &["Specific Aims".into()],
        )?;
        let base = store.save_section(
            project,
            "specific_aims",
            "Specific Aims",
            "Human approved baseline",
            None,
            "human_edit",
        )?;
        store.approve_section_version(project, "specific_aims", base)?;

        let c = store.conn()?;
        c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,created_at,completed_at) VALUES(?1,1,'fp','cfg','complete','[]',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",[project])?;
        let run1 = c.last_insert_rowid();
        c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,created_at,completed_at) VALUES(?1,1,'fp','cfg','complete','[]',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",[project])?;
        let run2 = c.last_insert_rowid();
        drop(c);

        let delta = CompetitiveDelta {
            from_run_id: Some(run1),
            to_run_id: run2,
            material: true,
            public_data_changed: true,
            provider_degraded: false,
            strategy_changed: true,
            broad_strategy_change: true,
            changed_section_keys: vec!["specific_aims".into()],
            new_candidates: vec!["candidate_b".into()],
            removed_candidates: vec![],
            score_changes: vec![],
            new_asset_keys: vec!["asset_b".into()],
            removed_asset_keys: vec![],
            summary: "New public competitor data".into(),
        };
        let event = store.record_competitive_update_event(
            project,
            &delta,
            &json!(["public_intelligence_refresh_due"]),
        )?;
        let proposed = store.save_section(
            project,
            "specific_aims",
            "Specific Aims",
            "Agent proposed updated text",
            None,
            "agentic_competitive_update",
        )?;
        store.record_competitive_section_update(event, project, "specific_aims", base, proposed)?;

        let state = store.section_state_json(project, "specific_aims")?;
        assert_eq!(
            state.pointer("/approved/version").and_then(Value::as_i64),
            Some(base)
        );
        assert_eq!(
            state.pointer("/latest/version").and_then(Value::as_i64),
            Some(proposed)
        );
        assert_eq!(store.competitive_pending_update_count(project)?, 1);
        let approved = store.approved_sections_json(project)?;
        assert_eq!(
            approved.pointer("/0/body").and_then(Value::as_str),
            Some("Human approved baseline")
        );

        store.approve_section_version(project, "specific_aims", proposed)?;
        assert_eq!(store.competitive_pending_update_count(project)?, 0);
        let approved = store.approved_sections_json(project)?;
        assert_eq!(
            approved.pointer("/0/body").and_then(Value::as_str),
            Some("Agent proposed updated text")
        );

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn newer_material_refresh_supersedes_older_pending_proposals() -> Result<()> {
        let path = temp_db("competitive-supersede");
        let store = Store::open(&path)?;
        let project = "project-supersede";
        store.create_project(project, "Test", None, None, &["Specific Aims".into()])?;
        let base = store.save_section(
            project,
            "specific_aims",
            "Specific Aims",
            "Baseline",
            None,
            "human_edit",
        )?;
        store.approve_section_version(project, "specific_aims", base)?;
        let c = store.conn()?;
        let mut runs = Vec::new();
        for _ in 0..3 {
            c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,created_at,completed_at) VALUES(?1,1,'fp','cfg','complete','[]',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",[project])?;
            runs.push(c.last_insert_rowid());
        }
        drop(c);
        let mk = |from: i64, to: i64, label: &str| CompetitiveDelta {
            from_run_id: Some(from),
            to_run_id: to,
            material: true,
            public_data_changed: true,
            provider_degraded: false,
            strategy_changed: true,
            broad_strategy_change: true,
            changed_section_keys: vec!["specific_aims".into()],
            new_candidates: vec![label.into()],
            removed_candidates: vec![],
            score_changes: vec![],
            new_asset_keys: vec![format!("asset-{label}")],
            removed_asset_keys: vec![],
            summary: label.into(),
        };
        let e1 = store.record_competitive_update_event(
            project,
            &mk(runs[0], runs[1], "first"),
            &json!(["public_intelligence_refresh_due"]),
        )?;
        let proposed = store.save_section(
            project,
            "specific_aims",
            "Specific Aims",
            "First proposal",
            None,
            "agentic_competitive_update",
        )?;
        store.record_competitive_section_update(e1, project, "specific_aims", base, proposed)?;
        assert_eq!(store.competitive_pending_update_count(project)?, 1);

        let _e2 = store.record_competitive_update_event(
            project,
            &mk(runs[1], runs[2], "second"),
            &json!(["public_intelligence_refresh_due"]),
        )?;
        assert_eq!(store.competitive_pending_update_count(project)?, 0);
        let old = store.competitive_update_event_json(project, e1)?;
        assert_eq!(
            old.get("text_refresh_status").and_then(Value::as_str),
            Some("complete")
        );
        assert!(old
            .get("text_refresh_errors")
            .and_then(Value::as_array)
            .is_some_and(|x| x
                .iter()
                .any(|v| v.as_str() == Some("superseded_by_newer_competitive_refresh"))));

        let _ = std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn internal_account_bootstrap_session_and_reset_are_single_use() -> Result<()> {
        let path=temp_db("internal-auth-lifecycle");
        let store=Store::open(&path)?;
        assert!(!store.internal_bootstrap_complete()?);
        let admin=store.bootstrap_internal_admin("org-1","Test Organization","admin.user","admin@example.org","Admin User","argon2-placeholder")?;
        assert_eq!(admin.system_role,"system_admin");
        assert!(admin.must_change_password);
        assert!(store.internal_bootstrap_complete()?);
        assert!(store.bootstrap_internal_admin("org-1","Test Organization","second","second@example.org","Second","hash").is_err());

        let expires=store.create_auth_session(&admin.id,"session-sha",60)?;
        assert!(!expires.is_empty());
        assert_eq!(store.internal_session("session-sha")?.map(|session|session.account.id),Some(admin.id.clone()));
        store.change_internal_password(&admin.id,"replacement-hash")?;
        assert!(!store.internal_account_by_id(&admin.id)?.context("admin")?.must_change_password);

        store.create_password_reset_token(&admin.id,"reset-sha","self_service",60,None)?;
        assert_eq!(store.password_reset_user("reset-sha")?,Some(admin.id.clone()));
        assert_eq!(store.consume_password_reset("reset-sha","newest-hash")?,admin.id);
        assert!(store.password_reset_user("reset-sha")?.is_none());
        assert!(store.consume_password_reset("reset-sha","replay-hash").is_err());
        assert!(store.internal_session("session-sha")?.is_none());
        let _=std::fs::remove_file(path);Ok(())
    }

    #[test]
    fn saved_projects_can_be_renamed_archived_and_restored_without_data_loss() -> Result<()> {
        let path=temp_db("project-lifecycle");let store=Store::open(&path)?;let project="persistent-grant";
        store.create_project(project,"Original title",Some("Sponsor"),Some("Mechanism"),&["Specific Aims".into()])?;
        let version=store.save_section(project,"specific_aims","Specific Aims","Persisted grant text",None,"human_edit")?;
        store.update_project_metadata(project,Some("Renamed grant"),Some(true),"owner-user")?;
        assert_eq!(store.list_projects_json(false)?.as_array().map(Vec::len),Some(0));
        let archived=store.list_projects_json(true)?;assert_eq!(archived.pointer("/0/title").and_then(Value::as_str),Some("Renamed grant"));
        assert!(archived.pointer("/0/archived_at").is_some_and(|value|!value.is_null()));
        assert_eq!(store.section_version_json(project,"specific_aims",version)?.get("body").and_then(Value::as_str),Some("Persisted grant text"));
        store.update_project_metadata(project,None,Some(false),"owner-user")?;
        assert_eq!(store.list_projects_json(false)?.pointer("/0/id").and_then(Value::as_str),Some(project));
        assert!(store.project_json(project)?.get("archived_at").is_some_and(Value::is_null));
        let events=store.workflow_events_json(project,20)?;
        assert!(events.as_array().is_some_and(|items|items.iter().any(|item|item.get("event_type").and_then(Value::as_str)==Some("project_archived"))));
        assert!(events.as_array().is_some_and(|items|items.iter().any(|item|item.get("event_type").and_then(Value::as_str)==Some("project_restored"))));
        let _=std::fs::remove_file(path);Ok(())
    }
}
