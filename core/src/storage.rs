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
use crate::models::GenerationAudit;
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
              user_id TEXT NOT NULL,key TEXT NOT NULL,method TEXT NOT NULL,path TEXT NOT NULL,
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
          error TEXT,started_at TEXT DEFAULT CURRENT_TIMESTAMP,completed_at TEXT,
          FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_generation_runs_project ON generation_runs(project_id,id);
        CREATE TABLE IF NOT EXISTS idempotency_keys(
          user_id TEXT NOT NULL,key TEXT NOT NULL,method TEXT NOT NULL,path TEXT NOT NULL,
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
          created_at TEXT DEFAULT CURRENT_TIMESTAMP
        );
        CREATE INDEX IF NOT EXISTS idx_research_queries ON research_queries(project_id,status,id);
        CREATE TABLE IF NOT EXISTS research_sources(
          id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL, query_id INTEGER, title TEXT NOT NULL,
          url TEXT NOT NULL, text TEXT NOT NULL, retrieved_at TEXT NOT NULL, content_sha256 TEXT NOT NULL, http_status INTEGER NOT NULL,
          UNIQUE(project_id,url,content_sha256), FOREIGN KEY(query_id) REFERENCES research_queries(id)
        );
        CREATE INDEX IF NOT EXISTS idx_research_sources ON research_sources(project_id,query_id);

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
        conn.execute("INSERT OR IGNORE INTO schema_migrations(version) VALUES(21)",[])?;
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
        use crate::workflow_artifacts::{AimSet,LiteratureManifest,OpportunityFitAssessment,ProposalSnapshot,ResearchFramework,SolicitationProfile};
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
                for requirement in profile.requirements.iter().filter(|fact|fact.mandatory){if !mapped_requirements.contains(requirement.id.as_str()){bail!("mandatory solicitation requirement {} is not mapped to the research framework",requirement.id);}}
                for criterion in &profile.review_criteria{if !mapped_criteria.contains(criterion.id.as_str()){bail!("review criterion {} is not mapped to the research framework",criterion.id);}}
            }
            "aim_set"=>{
                let aims:AimSet=serde_json::from_value(body.clone())?;
                let _=Self::approved_artifact_body_at(c,project,"research_framework",aims.framework_version)?;
            }
            "literature_manifest"=>{
                let manifest:LiteratureManifest=serde_json::from_value(body.clone())?;
                let _=Self::approved_artifact_body_at(c,project,"solicitation_profile",manifest.solicitation_profile_version)?;
                let _=Self::approved_artifact_body_at(c,project,"research_framework",manifest.framework_version)?;
                let _=Self::approved_artifact_body_at(c,project,"aim_set",manifest.aim_set_version)?;
                for need in &manifest.evidence_needs{for evidence_id in &need.evidence_ids{
                    let exists:i64=c.query_row("SELECT COUNT(*) FROM evidence WHERE id=?1 AND project_id=?2",params![evidence_id,project],|row|row.get(0))?;
                    if exists!=1{bail!("literature manifest references evidence {evidence_id} outside the project");}
                }}
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

    fn workflow_artifact_is_fresh(&self, project: &str, artifact_type: &str) -> Result<bool> {
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
            "literature_manifest" => {
                let manifest: crate::workflow_artifacts::LiteratureManifest = serde_json::from_value(body)?;
                self.current_approved_artifact_version(project, "solicitation_profile")?
                    == Some(manifest.solicitation_profile_version)
                    && self.current_approved_artifact_version(project, "research_framework")?
                        == Some(manifest.framework_version)
                    && self.current_approved_artifact_version(project, "aim_set")?
                        == Some(manifest.aim_set_version)
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
        if !core_artifact && !enabled_artifact {
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

    pub fn finalize_research_run(
        &self,
        project: &str,
        query_statuses: &[(i64, String)],
        manifest: &Value,
    ) -> Result<Value> {
        crate::workflow_artifacts::validate_artifact_document("literature_manifest", manifest, false)?;
        let raw = serde_json::to_string(manifest)?;
        let sha = sha256_hex(raw.as_bytes());
        let mut c = self.conn()?;
        Self::validate_artifact_source_anchors(&c, project, manifest)?;
        let tx = c.transaction()?;
        Self::validate_artifact_dependencies(&tx, project, "literature_manifest", manifest)?;
        for (query_id, status) in query_statuses {
            if !matches!(status.as_str(), "complete" | "complete_no_sources" | "failed") {
                bail!("unsupported terminal research query status: {status}");
            }
            let changed = tx.execute(
                "UPDATE research_queries SET status=?1 WHERE id=?2 AND project_id=?3",
                params![status, query_id, project],
            )?;
            if changed != 1 {
                bail!("research query {query_id} does not belong to this project");
            }
        }
        let current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version),0) FROM workflow_artifacts WHERE project_id=?1 AND artifact_type='literature_manifest'",
            [project],
            |row| row.get(0),
        )?;
        let version = current + 1;
        tx.execute(
            "INSERT INTO workflow_artifacts(project_id,artifact_type,version,body_json,content_sha256,source) VALUES(?1,'literature_manifest',?2,?3,?4,'research_pipeline')",
            params![project, version, raw, sha],
        )?;
        tx.execute(
            "INSERT INTO workflow_events(project_id,event_type,payload_json) VALUES(?1,'research_run_finalized',?2)",
            params![project, serde_json::to_string(&json!({
                "run_id":manifest.get("run_id"),
                "artifact_version":version,
                "artifact_sha256":sha,
                "query_count":query_statuses.len()
            }))?],
        )?;
        Self::touch_project_conn(&tx, project)?;
        tx.commit()?;
        self.workflow_artifact_json(project, "literature_manifest")
    }

    pub fn approve_workflow_artifact(
        &self,
        project: &str,
        artifact_type: &str,
        version: i64,
        approver: Option<&str>,
    ) -> Result<Value> {
        let config = self.workflow_config(project)?;
        let enabled = self
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
            "solicitation_profile" | "research_framework" | "aim_set" | "literature_manifest"
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

    pub fn list_projects_json(&self) -> Result<Value> {
        let c = self.conn()?;
        let mut st=c.prepare("SELECT id,title,sponsor,mechanism,created_at,COALESCE(updated_at,created_at) FROM projects ORDER BY COALESCE(updated_at,created_at) DESC LIMIT 250")?;
        let rows=st.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,"mechanism":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,"updated_at":r.get::<_,String>(5)?})))?;
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
        Ok(self.conn()?.query_row("SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,user_id],|row|row.get(0)).optional()?)
    }

    pub fn list_projects_for_user_json(&self,user_id:&str)->Result<Value> {
        let c=self.conn()?;
        let mut st=c.prepare(r#"SELECT p.id,p.title,p.sponsor,p.mechanism,p.created_at,COALESCE(p.updated_at,p.created_at),pm.role
          FROM projects p JOIN project_members pm ON pm.project_id=p.id WHERE pm.user_id=?1 ORDER BY COALESCE(p.updated_at,p.created_at) DESC LIMIT 250"#)?;
        let rows=st.query_map([user_id],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,"mechanism":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,"updated_at":r.get::<_,String>(5)?,"role":r.get::<_,String>(6)?})))?;
        let mut out=Vec::new();for row in rows{let mut value=row?;let id=value.get("id").and_then(Value::as_str).unwrap_or_default();value["stage"]=json!(self.compatibility_stage(id)?);out.push(value);}Ok(json!(out))
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
        let mut project=c.query_row("SELECT id,title,sponsor,mechanism,created_at,COALESCE(updated_at,created_at),interview_generated FROM projects WHERE id=?1",[id],|r|Ok(json!({
            "id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,
            "mechanism":r.get::<_,Option<String>>(3)?,"created_at":r.get::<_,String>(4)?,
            "updated_at":r.get::<_,String>(5)?,"interview_generated":r.get::<_,i64>(6)?!=0
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
        self.ensure_section(project, key, title)?;
        let c = self.conn()?;
        c.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source,editor_name,author_user_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?7)",params![project,key,title,body,html,source,editor])?;
        Self::touch_project_conn(&c, project)?;
        Ok(c.last_insert_rowid())
    }

    pub fn section_versions_json(&self, project: &str, key: &str) -> Result<Value> {
        let c = self.conn()?;
        let mut st = c.prepare(
            r#"
          SELECT id,created_at,source,COALESCE(author_user_id,editor_name),approved,length(body),
                 CASE WHEN length(body)>180 THEN substr(body,1,180)||'…' ELSE body END
          FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 100
        "#,
        )?;
        let rows=st.query_map(params![project,key],|r|Ok(json!({"version":r.get::<_,i64>(0)?,"created_at":r.get::<_,String>(1)?,"source":r.get::<_,String>(2)?,"editor":r.get::<_,Option<String>>(3)?,"approved":r.get::<_,i64>(4)?!=0,"characters":r.get::<_,i64>(5)?,"preview":r.get::<_,String>(6)?})))?;
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
        let c = self.conn()?;
        let latest:i64=c.query_row("SELECT id FROM section_versions WHERE project_id=?1 AND section_key=?2 ORDER BY id DESC LIMIT 1",params![project,key],|r|r.get(0)).context("section has no versions")?;
        if latest != expected_latest {
            bail!("section changed since history was loaded: expected latest version {expected_latest}, found {latest}");
        }
        let (title,body,html):(String,String,Option<String>)=c.query_row("SELECT title,body,html FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3",params![version_id,project,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).context("version does not belong to this project section")?;
        self.save_section_by(
            project,
            key,
            &title,
            &body,
            html.as_deref(),
            &format!("rollback:{version_id}"),
            actor,
        )
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
            let mut st=c.prepare("SELECT u.id,u.display_name,u.email,pm.role,pm.joined_at,pm.last_seen_at FROM project_members pm JOIN users u ON u.id=pm.user_id WHERE pm.project_id=?1 ORDER BY pm.last_seen_at DESC")?;
            let rows=st.query_map([project],|r|Ok(json!({"user_id":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"email":r.get::<_,Option<String>>(2)?,"role":r.get::<_,String>(3)?,"joined_at":r.get::<_,String>(4)?,"last_seen_at":r.get::<_,String>(5)?})))?;
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
        let body=body.trim();if body.is_empty()||body.len()>8000{bail!("comment must contain 1-8000 characters");}if start.zip(end).is_some_and(|(a,b)|a<0||b<=a){bail!("comment range is invalid");}
        let mut c=self.conn()?;let tx=c.transaction()?;
        let version_exists=match artifact_type{"section"=>tx.query_row("SELECT COUNT(*) FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3",params![version_id,project,artifact_key],|r|r.get::<_,i64>(0))?,_=>tx.query_row("SELECT COUNT(*) FROM workflow_artifacts WHERE id=?1 AND project_id=?2 AND artifact_type=?3",params![version_id,project,artifact_key],|r|r.get::<_,i64>(0))?};if version_exists!=1{bail!("comment target version is outside this project artifact");}
        if let Some(parent_id)=parent{let valid:i64=tx.query_row("SELECT COUNT(*) FROM comments WHERE id=?1 AND project_id=?2",params![parent_id,project],|r|r.get(0))?;if valid!=1{bail!("parent comment is outside this project");}}
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
        let id=Uuid::new_v4().to_string();let mut c=self.conn()?;let tx=c.transaction()?;let owner_member:i64=tx.query_row("SELECT COUNT(*) FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,owner],|r|r.get(0))?;if owner_member!=1{bail!("task owner must be a project member");}
        tx.execute("INSERT INTO tasks(id,project_id,title,description,owner_user_id,source,priority,due_at,created_by_user_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,project,title,description,owner,source,priority,due_at,created_by])?;for dependency in dependencies{let valid:i64=tx.query_row("SELECT COUNT(*) FROM tasks WHERE id=?1 AND project_id=?2",params![dependency,project],|r|r.get(0))?;if valid!=1{bail!("task dependency {dependency} is outside this project");}tx.execute("INSERT INTO task_dependencies(task_id,depends_on_task_id) VALUES(?1,?2)",params![id,dependency])?;}tx.execute("INSERT INTO notifications(user_id,project_id,kind,payload_json) VALUES(?1,?2,'task_assigned',?3)",params![owner,project,serde_json::to_string(&json!({"task_id":id,"title":title}))?])?;tx.commit()?;Ok(json!({"id":id}))
    }

    pub fn tasks_json(&self,project:&str)->Result<Value>{let c=self.conn()?;let mut st=c.prepare("SELECT id,title,description,owner_user_id,source,status,priority,due_at,completed_at,created_by_user_id,created_at,updated_at FROM tasks WHERE project_id=?1 ORDER BY CASE priority WHEN 'critical' THEN 0 WHEN 'high' THEN 1 WHEN 'normal' THEN 2 ELSE 3 END,due_at,id")?;let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"description":r.get::<_,String>(2)?,"owner_user_id":r.get::<_,String>(3)?,"source":r.get::<_,String>(4)?,"status":r.get::<_,String>(5)?,"priority":r.get::<_,String>(6)?,"due_at":r.get::<_,Option<String>>(7)?,"completed_at":r.get::<_,Option<String>>(8)?,"created_by_user_id":r.get::<_,String>(9)?,"created_at":r.get::<_,String>(10)?,"updated_at":r.get::<_,String>(11)?})))?;let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))}

    pub fn update_task_status(&self,project:&str,task_id:&str,status:&str)->Result<()> {if !matches!(status,"open"|"in_progress"|"blocked"|"complete"|"cancelled"){bail!("invalid task status");}let changed=self.conn()?.execute("UPDATE tasks SET status=?1,completed_at=CASE WHEN ?1='complete' THEN CURRENT_TIMESTAMP ELSE NULL END,updated_at=CURRENT_TIMESTAMP WHERE id=?2 AND project_id=?3",params![status,task_id,project])?;if changed!=1{bail!("task not found");}Ok(())}

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
        let role:Option<String>=if let Some(user)=actor{tx.query_row("SELECT role FROM project_members WHERE project_id=?1 AND user_id=?2",params![project,user],|row|row.get(0)).optional()?}else{None};
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
        let open: i64 = tx.query_row(
            "SELECT COUNT(*) FROM interview_questions WHERE project_id=?1 AND status='open'",
            [project],
            |r| r.get(0),
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

    pub fn save_causal_model_version(&self,project:&str,run_id:&str,body:&Value,author:&str,confirmed:bool)->Result<Value>{
        let causal:crate::workflow_artifacts::CausalAnalysisResult=serde_json::from_value(body.clone())?;let wrapper=crate::workflow_artifacts::ReviewSimulationResult{schema_version:1,snapshot_id:"validation".into(),rubric_version_id:"validation".into(),panel_plan_id:"validation".into(),reviews:Vec::new(),causal_analysis:Some(causal),panel_summary:json!({}),revision_tasks:Vec::new(),synthetic_review_notice:"validation".into()};crate::workflow_artifacts::validate_review_result(&wrapper,false)?;
        let raw=serde_json::to_string(body)?;let sha=sha256_hex(raw.as_bytes());let c=self.conn()?;let version:i64=c.query_row("SELECT COALESCE(MAX(version),0)+1 FROM causal_models WHERE review_run_id=?1",[run_id],|r|r.get(0))?;c.execute("INSERT INTO causal_models(project_id,review_run_id,version,body_json,content_sha256,author_user_id,confirmed) SELECT ?1,?2,?3,?4,?5,?6,?7 WHERE EXISTS(SELECT 1 FROM review_simulation_runs WHERE id=?2 AND project_id=?1 AND status='complete')",params![project,run_id,version,raw,sha,author,confirmed as i64])?;Ok(json!({"review_run_id":run_id,"version":version,"body":body,"sha256":sha,"confirmed":confirmed,"author_user_id":author}))
    }

    pub fn causal_models_json(&self,project:&str,run_id:&str)->Result<Value>{let c=self.conn()?;let mut st=c.prepare("SELECT version,body_json,content_sha256,author_user_id,confirmed,created_at FROM causal_models WHERE project_id=?1 AND review_run_id=?2 ORDER BY version DESC")?;let rows=st.query_map(params![project,run_id],|r|Ok(json!({"version":r.get::<_,i64>(0)?,"body":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or(json!({})),"sha256":r.get::<_,String>(2)?,"author_user_id":r.get::<_,String>(3)?,"confirmed":r.get::<_,i64>(4)?!=0,"created_at":r.get::<_,String>(5)?})))?;let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))}

    pub fn generation_runs_json(&self, project: &str, limit: usize) -> Result<Value> {
        let c = self.conn()?;
        let mut statement = c.prepare(
            r#"SELECT id,task_kind,routing_mode,provider,model,prompt_sha256,response_sha256,
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
                "high_value": row.get::<_, i64>(7)? != 0,
                "status": row.get::<_, String>(8)?,
                "error": row.get::<_, Option<String>>(9)?,
                "started_at": row.get::<_, String>(10)?,
                "completed_at": row.get::<_, Option<String>>(11)?
            }))
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(Value::Array(result))
    }

    pub fn claim_idempotency(
        &self,
        user_id: &str,
        key: &str,
        method: &str,
        path: &str,
    ) -> Result<IdempotencyClaim> {
        if key.len() < 8 || key.len() > 200 || key.chars().any(char::is_whitespace) {
            bail!("Idempotency-Key must be 8-200 non-whitespace characters");
        }
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        let existing = tx
            .query_row(
                "SELECT method,path,state,status_code,content_type,response_body FROM idempotency_keys WHERE user_id=?1 AND key=?2",
                params![user_id, key],
                |row| Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                )),
            )
            .optional()?;
        let claim = match existing {
            Some((stored_method, stored_path, _, _, _, _))
                if stored_method != method || stored_path != path => IdempotencyClaim::Conflict,
            Some((_, _, state, status, content_type, body)) if state == "complete" => {
                IdempotencyClaim::Replay {
                    status_code: status.unwrap_or(500).clamp(100, 599) as u16,
                    content_type: content_type.unwrap_or_else(|| "application/json".into()),
                    body: body.unwrap_or_default(),
                }
            }
            Some(_) => IdempotencyClaim::InProgress,
            None => {
                tx.execute(
                    "INSERT INTO idempotency_keys(user_id,key,method,path,state) VALUES(?1,?2,?3,?4,'in_progress')",
                    params![user_id, key, method, path],
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
    ) -> Result<String> {
        if task_kind.trim().is_empty() || prompt_sha256.len() != 64 {
            bail!("generation audit requires a task kind and SHA-256 prompt digest");
        }
        let run_id = Uuid::new_v4().to_string();
        let mut c = self.conn()?;
        let tx = c.transaction()?;
        tx.execute(
            r#"INSERT INTO generation_runs(
                 id,project_id,task_kind,routing_mode,provider,model,prompt_sha256,high_value,status
               ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'running')"#,
            params![run_id, project, task_kind, routing_mode, provider, model, prompt_sha256, high_value as i64],
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

#[cfg(test)]
mod phase6_storage_tests {
    use super::*;
    use crate::competitive_updates::CompetitiveDelta;
    use crate::domain::RequirementDraft;

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
}
