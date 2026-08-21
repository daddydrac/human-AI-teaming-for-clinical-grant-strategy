use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::domain::{InterviewQuestionDraft, RequirementDraft, RetrievalRecord};
use crate::clinical::ClinicalStudy;
use crate::competitive::{CompetitiveConfig, CompetitiveProfile, CompetitiveRunOutput};
use crate::competitive_updates::CompetitiveDelta;
use crate::compliance::{ComplianceFacts, ComplianceProfile, evaluate as evaluate_compliance};
use crate::research::FetchedSource;
use crate::source_locator::SourceDocument;
use crate::workflow::Stage;

pub struct Store { path: PathBuf }

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

    fn has_column(conn:&Connection, table:&str, column:&str)->Result<bool> {
        let mut st=conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows=st.query_map([],|r|r.get::<_,String>(1))?;
        for row in rows { if row? == column { return Ok(true); } }
        Ok(false)
    }

    fn migrate(conn:&Connection)->Result<()> {
        let current:i64=conn.query_row("SELECT COALESCE(MAX(version),0) FROM schema_migrations",[],|r|r.get(0)).unwrap_or(0);
        if !Self::has_column(conn,"projects","stage")? {
            conn.execute("ALTER TABLE projects ADD COLUMN stage TEXT NOT NULL DEFAULT 'intake'",[])?;
        }
        if !Self::has_column(conn,"projects","updated_at")? {
            conn.execute("ALTER TABLE projects ADD COLUMN updated_at TEXT",[])?;
            conn.execute("UPDATE projects SET updated_at=created_at WHERE updated_at IS NULL",[])?;
        }
        if !Self::has_column(conn,"projects","interview_generated")? {
            conn.execute("ALTER TABLE projects ADD COLUMN interview_generated INTEGER NOT NULL DEFAULT 0",[])?;
        }
        if !Self::has_column(conn,"project_sections","origin")? {
            conn.execute("ALTER TABLE project_sections ADD COLUMN origin TEXT NOT NULL DEFAULT 'configured'",[])?;
        }
        // Section catalog backfill is idempotent and safe on every startup.
        conn.execute_batch(r#"
        INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required)
        SELECT project_id,section_key,title,position,1 FROM (
          SELECT project_id,section_key,MAX(title) title,
                 ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY MIN(id)) - 1 AS position
          FROM section_versions GROUP BY project_id,section_key
        );
        "#)?;
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
          body TEXT NOT NULL,html TEXT,source TEXT NOT NULL,approved INTEGER NOT NULL DEFAULT 0,
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
          version_id INTEGER NOT NULL,approved_at TEXT DEFAULT CURRENT_TIMESTAMP,
          FOREIGN KEY(version_id) REFERENCES section_versions(id)
        );

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
        conn.execute("INSERT OR IGNORE INTO schema_migrations(version) VALUES(10)",[])?;
        Ok(Self { path: path_buf })
    }

    fn touch_project_conn(c:&Connection, project:&str)->Result<()> {
        c.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        Ok(())
    }

    pub fn create_project(&self,id:&str,title:&str,sponsor:Option<&str>,mechanism:Option<&str>,sections:&[String])->Result<()> {
        let mut c=self.conn()?; let tx=c.transaction()?;
        tx.execute("INSERT INTO projects(id,title,sponsor,mechanism,stage,updated_at) VALUES(?1,?2,?3,?4,'intake',CURRENT_TIMESTAMP)",params![id,title,sponsor,mechanism])?;
        for (position,title) in sections.iter().filter(|s|!s.trim().is_empty()).enumerate() {
            let key=section_key(title);
            tx.execute("INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required) VALUES(?1,?2,?3,?4,1)",params![id,key,title.trim(),position as i64])?;
        }
        tx.commit()?; Ok(())
    }

    pub fn list_projects_json(&self)->Result<Value>{
        let c=self.conn()?;
        let mut st=c.prepare("SELECT id,title,sponsor,mechanism,stage,created_at,COALESCE(updated_at,created_at) FROM projects ORDER BY COALESCE(updated_at,created_at) DESC LIMIT 250")?;
        let rows=st.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,"mechanism":r.get::<_,Option<String>>(3)?,"stage":r.get::<_,String>(4)?,"created_at":r.get::<_,String>(5)?,"updated_at":r.get::<_,String>(6)?})))?;
        let mut out=Vec::new(); for row in rows{out.push(row?);} Ok(json!(out))
    }

    pub fn project_json(&self,id:&str)->Result<Value>{
        let c=self.conn()?;
        c.query_row("SELECT id,title,sponsor,mechanism,stage,created_at,COALESCE(updated_at,created_at),interview_generated FROM projects WHERE id=?1",[id],|r|Ok(json!({
            "id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"sponsor":r.get::<_,Option<String>>(2)?,
            "mechanism":r.get::<_,Option<String>>(3)?,"stage":r.get::<_,String>(4)?,"created_at":r.get::<_,String>(5)?,
            "updated_at":r.get::<_,String>(6)?,"interview_generated":r.get::<_,i64>(7)?!=0
        }))).context("project not found")
    }

    pub fn project_stage(&self,project:&str)->Result<Stage>{
        let c=self.conn()?;
        let stage:String=c.query_row("SELECT stage FROM projects WHERE id=?1",[project],|r|r.get(0)).context("project not found")?;
        Stage::from_str(&stage)
    }

    pub fn set_stage(&self,project:&str,stage:Stage)->Result<()> {
        let c=self.conn()?;
        c.execute("UPDATE projects SET stage=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",params![stage.as_str(),project])?;
        Ok(())
    }

    pub fn advance_stage(&self,project:&str,stage:Stage)->Result<()> {
        let current=self.project_stage(project)?;
        if stage>current { self.set_stage(project,stage)?; }
        Ok(())
    }

    pub fn add_document(&self,project:&str,name:&str,kind:&str,text:&str,sha:&str)->Result<(i64,bool)>{
        if text.trim().is_empty(){bail!("document contains no readable text");}
        let mut c=self.conn()?; let tx=c.transaction()?;
        let n=tx.execute("INSERT OR IGNORE INTO documents(project_id,name,kind,text,sha256) VALUES(?1,?2,?3,?4,?5)",params![project,name,kind,text,sha])?;
        let id=if n>0 { tx.last_insert_rowid() } else { tx.query_row("SELECT id FROM documents WHERE project_id=?1 AND sha256=?2",params![project,sha],|r|r.get::<_,i64>(0)).context("document disappeared after duplicate check")? };
        if n>0 {
            let current:String=tx.query_row("SELECT stage FROM projects WHERE id=?1",[project],|r|r.get(0))?;
            let current=Stage::from_str(&current)?;
            if current >= Stage::Requirements {
                tx.execute("UPDATE requirements SET approved=0 WHERE project_id=?1",[project])?;
                tx.execute("UPDATE section_versions SET approved=0 WHERE project_id=?1",[project])?;
                tx.execute("UPDATE projects SET stage='documents', interview_generated=0,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
            } else {
                tx.execute("UPDATE projects SET stage='documents',updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
            }
        }
        tx.commit()?; Ok((id,n>0))
    }

    pub fn document_count(&self,project:&str)->Result<i64>{Ok(self.conn()?.query_row("SELECT COUNT(*) FROM documents WHERE project_id=?1",[project],|r|r.get(0))?)}

    pub fn replace_document_chunks(&self,project:&str,document_id:i64,chunks:&[crate::chunker::TextChunk])->Result<()> {
        let mut c=self.conn()?; let tx=c.transaction()?;
        tx.execute("DELETE FROM document_chunks WHERE document_id=?1 AND project_id=?2",params![document_id,project])?;
        for ch in chunks { tx.execute("INSERT INTO document_chunks(project_id,document_id,ordinal,start_word,end_word,text) VALUES(?1,?2,?3,?4,?5,?6)",params![project,document_id,ch.ordinal as i64,ch.start_word as i64,ch.end_word as i64,ch.text])?; }
        tx.commit()?; Ok(())
    }

    pub fn document_context(&self,project:&str,max_chars:usize)->Result<String>{
        let c=self.conn()?; let mut st=c.prepare("SELECT name,kind,text FROM documents WHERE project_id=?1 ORDER BY id")?;
        let rows=st.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;
        let mut out=String::new();
        for row in rows {let(name,kind,text)=row?; let chunk=format!("\n\n=== {kind}: {name} ===\n{text}"); if out.len()+chunk.len()>max_chars{break;} out.push_str(&chunk);}
        Ok(out)
    }

    pub fn save_analysis(&self,project:&str,kind:&str,content:&str)->Result<i64>{let c=self.conn()?; c.execute("INSERT INTO analyses(project_id,kind,content) VALUES(?1,?2,?3)",params![project,kind,content])?; Ok(c.last_insert_rowid())}

    pub fn ensure_section(&self,project:&str,key:&str,title:&str)->Result<()> {
        let c=self.conn()?;
        let next:i64=c.query_row("SELECT COALESCE(MAX(position),-1)+1 FROM project_sections WHERE project_id=?1",[project],|r|r.get(0))?;
        c.execute("INSERT OR IGNORE INTO project_sections(project_id,section_key,title,position,required) VALUES(?1,?2,?3,?4,1)",params![project,key,title,next])?;
        c.execute("UPDATE project_sections SET title=?1 WHERE project_id=?2 AND section_key=?3",params![title,project,key])?;
        Ok(())
    }

    pub fn project_sections_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?; let mut st=c.prepare(r#"
          SELECT ps.section_key,ps.title,ps.position,ps.required,ps.origin,
                 (SELECT sv.id FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key ORDER BY sv.id DESC LIMIT 1) latest_version,
                 (SELECT sv.id FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1 ORDER BY sv.id DESC LIMIT 1) approved_version
          FROM project_sections ps WHERE ps.project_id=?1 ORDER BY ps.position,ps.section_key
        "#)?;
        let rows=st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"required":r.get::<_,i64>(3)?!=0,"origin":r.get::<_,String>(4)?,"latest_version":r.get::<_,Option<i64>>(5)?,"approved_version":r.get::<_,Option<i64>>(6)?})))?;
        let mut out=Vec::new(); for row in rows{out.push(row?);} Ok(json!(out))
    }

    pub fn save_section(&self,project:&str,key:&str,title:&str,body:&str,html:Option<&str>,source:&str)->Result<i64>{
        self.ensure_section(project,key,title)?;
        let c=self.conn()?;
        c.execute("INSERT INTO section_versions(project_id,section_key,title,body,html,source) VALUES(?1,?2,?3,?4,?5,?6)",params![project,key,title,body,html,source])?;
        Self::touch_project_conn(&c,project)?; Ok(c.last_insert_rowid())
    }

    pub fn section_state_json(&self,project:&str,key:&str)->Result<Value>{
        let c=self.conn()?;
        let meta=c.query_row("SELECT title,position,required FROM project_sections WHERE project_id=?1 AND section_key=?2",params![project,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?!=0))).optional()?;
        let Some((title,position,required))=meta else { return Ok(json!({"section_key":key,"exists":false})); };
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
        Ok(json!({"section_key":key,"exists":true,"title":title,"position":position,"required":required,"latest":latest,"approved":approved,"competitive_update":competitive_update}))
    }

    pub fn latest_sections_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?; let mut st=c.prepare(r#"
          SELECT ps.section_key,ps.title,ps.position,sv.id,sv.body,sv.source,sv.approved
          FROM project_sections ps
          JOIN section_versions sv ON sv.id=(SELECT id FROM section_versions x WHERE x.project_id=ps.project_id AND x.section_key=ps.section_key ORDER BY id DESC LIMIT 1)
          WHERE ps.project_id=?1 ORDER BY ps.position,ps.section_key
        "#)?;
        let rows=st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"position":r.get::<_,i64>(2)?,"version":r.get::<_,i64>(3)?,"body":r.get::<_,String>(4)?,"source":r.get::<_,String>(5)?,"approved":r.get::<_,i64>(6)?!=0})))?;
        let mut out=Vec::new();for row in rows{out.push(row?);}Ok(json!(out))
    }

    pub fn approve_section_version(&self,project:&str,key:&str,version_id:i64)->Result<i64>{
        let mut c=self.conn()?; let tx=c.transaction()?;
        let exists:i64=tx.query_row("SELECT COUNT(*) FROM section_versions WHERE id=?1 AND project_id=?2 AND section_key=?3",params![version_id,project,key],|r|r.get(0))?;
        if exists!=1{bail!("section version {version_id} does not belong to project/section");}
        tx.execute("UPDATE section_versions SET approved=0 WHERE project_id=?1 AND section_key=?2",params![project,key])?;
        tx.execute("UPDATE section_versions SET approved=1 WHERE id=?1",[version_id])?;
        tx.execute("INSERT INTO approvals(project_id,section_key,version_id) VALUES(?1,?2,?3)",params![project,key,version_id])?;
        // Any explicit post-update human approval resolves pending competitive text proposals for this section.
        // The human may approve the proposed version, an edited derivative, or deliberately re-approve the prior text.
        tx.execute("UPDATE competitive_section_updates SET status='resolved_by_human',resolved_version_id=?1,resolved_at=CURRENT_TIMESTAMP WHERE project_id=?2 AND section_key=?3 AND status='pending'",params![version_id,project,key])?;
        tx.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?; Ok(version_id)
    }

    pub fn approved_sections_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;
        let mut st=c.prepare(r#"
          SELECT ps.section_key,ps.title,sv.body,sv.html,sv.id,ps.position
          FROM project_sections ps
          JOIN section_versions sv ON sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1
          WHERE ps.project_id=?1
          ORDER BY ps.position ASC, ps.section_key ASC
        "#)?;
        let rows=st.query_map([project],|r|Ok(json!({"section_key":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"body":r.get::<_,String>(2)?,"html":r.get::<_,Option<String>>(3)?,"version":r.get::<_,i64>(4)?,"position":r.get::<_,i64>(5)?})))?;
        let mut out=Vec::new(); for row in rows{out.push(row?);} Ok(json!(out))
    }

    pub fn all_required_sections_approved(&self,project:&str)->Result<bool>{
        let c=self.conn()?;
        let missing:i64=c.query_row(r#"
          SELECT COUNT(*) FROM project_sections ps
          WHERE ps.project_id=?1 AND ps.required=1 AND NOT EXISTS(
            SELECT 1 FROM section_versions sv WHERE sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1)
        "#,[project],|r|r.get(0))?;
        Ok(missing==0)
    }

    pub fn replace_requirements(&self,project:&str,reqs:&[RequirementDraft])->Result<()> {
        let mut c=self.conn()?; let tx=c.transaction()?;
        // Delete downstream objects in foreign-key-safe order before replacing the
        // authoritative requirement set.  Research sources reference queries and
        // citations reference evidence, so parents must be removed last.
        tx.execute("DELETE FROM citations WHERE project_id=?1",[project])?;
        tx.execute("DELETE FROM evidence WHERE project_id=?1",[project])?;
        tx.execute("DELETE FROM research_sources WHERE project_id=?1",[project])?;
        tx.execute("DELETE FROM research_queries WHERE project_id=?1",[project])?;
        tx.execute("DELETE FROM interview_answers WHERE project_id=?1",[project])?;
        tx.execute("DELETE FROM interview_questions WHERE project_id=?1",[project])?;
        tx.execute("DELETE FROM requirements WHERE project_id=?1",[project])?;
        tx.execute("UPDATE section_versions SET approved=0 WHERE project_id=?1",[project])?;
        for r in reqs { tx.execute("INSERT INTO requirements(project_id,external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,source_clue,source_document,source_locator) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![project,r.external_id,r.category,r.requirement,r.mandatory as i32,serde_json::to_string(&r.evidence_needed)?,serde_json::to_string(&r.dependencies)?,r.source_clue,r.source_document,r.source_locator])?; }
        tx.execute("UPDATE projects SET stage='requirements',interview_generated=0,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?; Ok(())
    }

    pub fn requirements_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?; let mut st=c.prepare("SELECT external_id,category,requirement,mandatory,evidence_needed_json,dependencies_json,source_clue,source_document,source_locator,status,approved FROM requirements WHERE project_id=?1 ORDER BY mandatory DESC,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,String>(0)?,"category":r.get::<_,String>(1)?,"requirement":r.get::<_,String>(2)?,"mandatory":r.get::<_,i64>(3)?!=0,"evidence_needed":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"dependencies":serde_json::from_str::<Value>(&r.get::<_,String>(5)?).unwrap_or(json!([])),"source_clue":r.get::<_,Option<String>>(6)?,"source_document":r.get::<_,Option<String>>(7)?,"source_locator":r.get::<_,Option<String>>(8)?,"status":r.get::<_,String>(9)?,"approved":r.get::<_,i64>(10)?!=0})))?;
        let mut out=vec![]; for row in rows{out.push(row?);} Ok(json!(out))
    }
    pub fn requirements_context(&self,project:&str)->Result<String>{Ok(serde_json::to_string_pretty(&self.requirements_json(project)?)?)}
    pub fn requirements_all_approved(&self,project:&str)->Result<bool>{
        let c=self.conn()?; let (total,approved):(i64,i64)=c.query_row("SELECT COUNT(*),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1",[project],|r|Ok((r.get(0)?,r.get(1)?)))?;
        Ok(total>0 && total==approved)
    }
    pub fn approve_requirements(&self,project:&str)->Result<usize>{
        let mut c=self.conn()?; let tx=c.transaction()?;
        let total:i64=tx.query_row("SELECT COUNT(*) FROM requirements WHERE project_id=?1",[project],|r|r.get(0))?;
        if total==0{bail!("no parsed requirements exist to approve");}
        let n=tx.execute("UPDATE requirements SET approved=1 WHERE project_id=?1",[project])?;
        tx.execute("UPDATE projects SET stage='interview',interview_generated=0,updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        tx.commit()?; Ok(n)
    }

    pub fn replace_open_interview_questions(&self,project:&str,questions:&[InterviewQuestionDraft])->Result<()> {
        let mut c=self.conn()?; let tx=c.transaction()?;
        tx.execute("DELETE FROM interview_questions WHERE project_id=?1 AND status='open'",[project])?;
        for q in questions {tx.execute("INSERT INTO interview_questions(project_id,requirement_external_id,question,answer_type,choices_json,unit,why_needed,evidence_requested,priority) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![project,q.requirement_id,q.question,q.answer_type,serde_json::to_string(&q.choices)?,q.unit,q.why_needed,q.evidence_requested as i32,q.priority])?;}
        let stage=if questions.is_empty(){"research"}else{"interview"};
        tx.execute("UPDATE projects SET stage=?1,interview_generated=1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",params![stage,project])?;
        tx.commit()?; Ok(())
    }
    pub fn interview_generated(&self,project:&str)->Result<bool>{Ok(self.conn()?.query_row("SELECT interview_generated FROM projects WHERE id=?1",[project],|r|r.get::<_,i64>(0))?!=0)}
    pub fn interview_open_count(&self,project:&str)->Result<i64>{Ok(self.conn()?.query_row("SELECT COUNT(*) FROM interview_questions WHERE project_id=?1 AND status='open'",[project],|r|r.get(0))?)}
    pub fn interview_questions_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?; let mut st=c.prepare("SELECT id,requirement_external_id,question,answer_type,choices_json,unit,why_needed,evidence_requested,priority,status FROM interview_questions WHERE project_id=?1 ORDER BY CASE status WHEN 'open' THEN 0 ELSE 1 END,priority DESC,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_id":r.get::<_,String>(1)?,"question":r.get::<_,String>(2)?,"answer_type":r.get::<_,String>(3)?,"choices":serde_json::from_str::<Value>(&r.get::<_,String>(4)?).unwrap_or(json!([])),"unit":r.get::<_,Option<String>>(5)?,"why_needed":r.get::<_,Option<String>>(6)?,"evidence_requested":r.get::<_,i64>(7)?!=0,"priority":r.get::<_,i64>(8)?,"status":r.get::<_,String>(9)?})))?;
        let mut out=vec![]; for row in rows{out.push(row?);} Ok(json!(out))
    }
    pub fn save_interview_answer(&self,project:&str,question_id:i64,value:&Value,confidence:&str,classification:&str,notes:Option<&str>,answered_by:Option<&str>)->Result<i64>{
        let mut c=self.conn()?; let tx=c.transaction()?;
        let status:String=tx.query_row("SELECT status FROM interview_questions WHERE id=?1 AND project_id=?2",params![question_id,project],|r|r.get(0)).context("interview question not found for project")?;
        if status!="open"{bail!("interview question {question_id} is not open");}
        tx.execute("INSERT INTO interview_answers(project_id,question_id,value_json,confidence,classification,notes,answered_by) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![project,question_id,serde_json::to_string(value)?,confidence,classification,notes,answered_by])?;
        let id=tx.last_insert_rowid(); tx.execute("UPDATE interview_questions SET status='answered' WHERE id=?1 AND project_id=?2",params![question_id,project])?;
        let open:i64=tx.query_row("SELECT COUNT(*) FROM interview_questions WHERE project_id=?1 AND status='open'",[project],|r|r.get(0))?;
        if open==0{tx.execute("UPDATE projects SET stage='research',updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;} else {tx.execute("UPDATE projects SET updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;}
        tx.commit()?; Ok(id)
    }
    pub fn interview_context(&self,project:&str)->Result<String>{
        let c=self.conn()?; let mut st=c.prepare("SELECT q.requirement_external_id,q.question,a.value_json,a.confidence,a.classification,a.notes FROM interview_answers a JOIN interview_questions q ON q.id=a.question_id WHERE a.project_id=?1 ORDER BY a.id")?;
        let rows=st.query_map([project],|r|Ok(json!({"requirement_id":r.get::<_,String>(0)?,"question":r.get::<_,String>(1)?,"answer":serde_json::from_str::<Value>(&r.get::<_,String>(2)?).unwrap_or(json!(null)),"confidence":r.get::<_,String>(3)?,"classification":r.get::<_,String>(4)?,"notes":r.get::<_,Option<String>>(5)?})))?;
        let mut out=vec![]; for row in rows{out.push(row?);} Ok(serde_json::to_string_pretty(&out)?)
    }

    pub fn insert_research_query(&self,project:&str,requirement_id:&str,query:&str,domains:&[String],rationale:&str)->Result<i64>{let c=self.conn()?; c.execute("INSERT INTO research_queries(project_id,requirement_external_id,query,preferred_domains_json,rationale) VALUES(?1,?2,?3,?4,?5)",params![project,requirement_id,query,serde_json::to_string(domains)?,rationale])?; Ok(c.last_insert_rowid())}
    pub fn mark_research_query(&self,id:i64,status:&str)->Result<()>{self.conn()?.execute("UPDATE research_queries SET status=?1 WHERE id=?2",params![status,id])?; Ok(())}
    pub fn add_research_source(&self,project:&str,query_id:i64,src:&FetchedSource)->Result<Option<i64>>{let c=self.conn()?; let n=c.execute("INSERT OR IGNORE INTO research_sources(project_id,query_id,title,url,text,retrieved_at,content_sha256,http_status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project,query_id,src.title,src.url,src.text,src.retrieved_at,src.sha256,src.status])?; if n>0{return Ok(Some(c.last_insert_rowid()));} Ok(c.query_row("SELECT id FROM research_sources WHERE project_id=?1 AND url=?2 AND content_sha256=?3",params![project,src.url,src.sha256],|r|r.get::<_,i64>(0)).optional()?)}
    pub fn add_evidence(&self,project:&str,requirement_id:Option<&str>,source_type:&str,source_ref:&str,claim:&str,passage:&str,url:Option<&str>,locator:Option<&str>,confidence:f64,status:&str)->Result<i64>{let c=self.conn()?; c.execute("INSERT INTO evidence(project_id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![project,requirement_id,source_type,source_ref,claim,passage,url,locator,confidence,status])?; Ok(c.last_insert_rowid())}
    pub fn add_citation(&self,project:&str,evidence_id:i64,key:&str,title:&str,url:Option<&str>,passage:&str,sha:&str,verified:bool)->Result<i64>{let c=self.conn()?; c.execute("INSERT INTO citations(project_id,evidence_id,citation_key,title,url,passage,content_sha256,verified) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![project,evidence_id,key,title,url,passage,sha,verified as i32])?; Ok(c.last_insert_rowid())}
    pub fn evidence_json(&self,project:&str)->Result<Value>{let c=self.conn()?; let mut st=c.prepare("SELECT id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status FROM evidence WHERE project_id=?1 ORDER BY id DESC")?; let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"requirement_id":r.get::<_,Option<String>>(1)?,"source_type":r.get::<_,String>(2)?,"source_ref":r.get::<_,String>(3)?,"claim":r.get::<_,String>(4)?,"passage":r.get::<_,String>(5)?,"url":r.get::<_,Option<String>>(6)?,"locator":r.get::<_,Option<String>>(7)?,"confidence":r.get::<_,f64>(8)?,"status":r.get::<_,String>(9)?})))?; let mut out=vec![]; for row in rows{out.push(row?);} Ok(json!(out))}
    pub fn evidence_context(&self,project:&str,max_chars:usize)->Result<String>{let mut s=serde_json::to_string_pretty(&self.evidence_json(project)?)?; if s.len()>max_chars{s.truncate(max_chars);} Ok(s)}
    pub fn requirement_ids(&self,project:&str)->Result<Vec<String>>{let c=self.conn()?; let mut st=c.prepare("SELECT external_id FROM requirements WHERE project_id=?1 ORDER BY id")?; let rows=st.query_map([project],|r|r.get::<_,String>(0))?; let mut out=Vec::new(); for r in rows{out.push(r?);} Ok(out)}

    pub fn save_clinical_study(&self,project:&str,study:&ClinicalStudy)->Result<Value>{
        crate::clinical::validate_study(study)?;
        let bytes=serde_json::to_vec(study)?; let sha=sha256_hex(&bytes); let study_json=String::from_utf8(bytes)?;
        let mut c=self.conn()?; let tx=c.transaction()?;
        let exists:i64=tx.query_row("SELECT COUNT(*) FROM projects WHERE id=?1",[project],|r|r.get(0))?;
        if exists!=1{bail!("project not found");}
        let version:i64=tx.query_row("SELECT COALESCE(version,0)+1 FROM clinical_studies WHERE project_id=?1",[project],|r|r.get(0)).optional()?.unwrap_or(1);
        tx.execute("INSERT INTO clinical_study_history(project_id,version,study_json,content_sha256) VALUES(?1,?2,?3,?4)",params![project,version,study_json,sha])?;
        tx.execute(r#"INSERT INTO clinical_studies(project_id,version,study_json,content_sha256,updated_at)
            VALUES(?1,?2,?3,?4,CURRENT_TIMESTAMP)
            ON CONFLICT(project_id) DO UPDATE SET version=excluded.version,study_json=excluded.study_json,content_sha256=excluded.content_sha256,updated_at=CURRENT_TIMESTAMP"#,
            params![project,version,study_json,sha])?;
        let target=Stage::Science;
        // Any authoritative scientific-design change invalidates the public competitive comparison. Preserve
        // approved prose versions, but reopen the workflow at science so competitive intelligence must be rerun
        // before new prose can be drafted/approved or the package can be exported.
        tx.execute("UPDATE projects SET stage=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",params![target.as_str(),project])?;
        tx.commit()?;
        Ok(json!({"version":version,"sha256":sha,"study":study}))
    }

    pub fn clinical_study_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;
        let row=c.query_row("SELECT version,study_json,content_sha256,updated_at FROM clinical_studies WHERE project_id=?1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional()?;
        if let Some((version,raw,sha,updated_at))=row {
            let study:Value=serde_json::from_str(&raw).context("stored clinical study JSON is invalid")?;
            Ok(json!({"exists":true,"version":version,"sha256":sha,"updated_at":updated_at,"study":study}))
        } else { Ok(json!({"exists":false,"version":null,"sha256":null,"updated_at":null,"study":null})) }
    }

    pub fn clinical_study_typed(&self,project:&str)->Result<Option<ClinicalStudy>>{
        let c=self.conn()?;
        let raw=c.query_row("SELECT study_json FROM clinical_studies WHERE project_id=?1",[project],|r|r.get::<_,String>(0)).optional()?;
        match raw { Some(x)=>Ok(Some(serde_json::from_str(&x).context("stored clinical study JSON is invalid")?)),None=>Ok(None) }
    }

    pub fn clinical_context(&self,project:&str)->Result<String>{
        let Some(study)=self.clinical_study_typed(project)? else { return Ok("CLINICAL STUDY MODEL: not configured".into()); };
        let assessment=crate::clinical::assess(&study,&self.approved_sections_json(project)?);
        Ok(format!("AUTHORITATIVE CLINICAL STUDY MODEL:\n{}\n\nDETERMINISTIC CLINICAL ASSESSMENT:\n{}",serde_json::to_string_pretty(&study)?,serde_json::to_string_pretty(&assessment)?))
    }

    pub fn clinical_assessment_json(&self,project:&str)->Result<Value>{
        let Some(study)=self.clinical_study_typed(project)? else { return Ok(json!({"exists":false,"errors":[{"code":"missing_clinical_study","message":"Clinical study model has not been configured"}],"warnings":[],"cross_section_consistency":{"count":0,"conflicts":[]}})); };
        let mut assessment=crate::clinical::assess(&study,&self.approved_sections_json(project)?);
        if let Some(obj)=assessment.as_object_mut(){obj.insert("exists".into(),Value::Bool(true));}
        Ok(assessment)
    }

    pub fn save_design_profile(&self,project:&str,profile:&Value)->Result<Value>{
        // Serialize once so the same exact bytes are hashed and persisted for snapshot reproducibility.
        let bytes=serde_json::to_vec(profile)?;
        let sha=sha256_hex(&bytes);
        let json=String::from_utf8(bytes)?;
        let c=self.conn()?;
        let exists:i64=c.query_row("SELECT COUNT(*) FROM projects WHERE id=?1",[project],|r|r.get(0))?;
        if exists!=1{bail!("project not found");}
        c.execute(r#"INSERT INTO project_design(project_id,profile_json,content_sha256,updated_at)
          VALUES(?1,?2,?3,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET profile_json=excluded.profile_json,content_sha256=excluded.content_sha256,updated_at=CURRENT_TIMESTAMP"#,
          params![project,json,sha])?;
        Self::touch_project_conn(&c,project)?;
        Ok(json!({"profile":profile,"sha256":sha}))
    }

    pub fn design_profile_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;
        let row=c.query_row("SELECT profile_json,content_sha256,updated_at FROM project_design WHERE project_id=?1",[project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
        if let Some((profile,sha,updated_at))=row{
            let parsed=serde_json::from_str::<Value>(&profile).context("stored design profile is invalid JSON")?;
            Ok(json!({"profile":parsed,"sha256":sha,"updated_at":updated_at}))
        }else{Ok(json!({"profile":null,"sha256":null,"updated_at":null}))}
    }

    pub fn readiness_json(&self,project:&str)->Result<Value>{
        let requirements=self.requirements_all_approved(project)?;
        let interview_generated=self.interview_generated(project)?;
        let open=self.interview_open_count(project)?;
        let sections=self.all_required_sections_approved(project)?;
        let stage=self.project_stage(project)?;
        let design=self.design_profile_json(project)?;
        let design_profile_present=!design.get("profile").unwrap_or(&Value::Null).is_null();
        let clinical=self.clinical_assessment_json(project)?;
        let clinical_present=clinical.get("exists").and_then(Value::as_bool).unwrap_or(false);
        let clinical_errors=clinical.get("errors").and_then(Value::as_array).map(|x|x.len()).unwrap_or(0);
        let cross_section_conflicts=clinical.get("cross_section_consistency").and_then(|x|x.get("count")).and_then(Value::as_u64).unwrap_or(0);
        let blocking_cross_section_conflicts=clinical.get("cross_section_consistency").and_then(|x|x.get("conflicts")).and_then(Value::as_array)
            .map(|rows|rows.iter().filter(|x|x.get("severity").and_then(Value::as_str)==Some("error")).count() as u64).unwrap_or(0);
        let clinical_consistent=clinical_present && clinical_errors==0 && blocking_cross_section_conflicts==0;
        let competitive=self.competitive_latest_json(project)?;
        let competitive_present=competitive.get("exists").and_then(Value::as_bool).unwrap_or(false);
        let competitive_fresh=competitive_present && competitive.get("fresh").and_then(Value::as_bool).unwrap_or(false) && competitive.get("status").and_then(Value::as_str)==Some("complete");
        let competitive_updates_pending=self.competitive_pending_update_count(project)?;
        let competitive_refresh_processing=self.competitive_text_refresh_pending_count(project)?;
        let compliance=self.compliance_assessment_json(project)?;
        let compliance_ready=compliance.get("ready").and_then(Value::as_bool).unwrap_or(false);
        let compliance_hard_failures=compliance.get("hard_failures").and_then(Value::as_u64).unwrap_or(1);
        let ready=requirements && interview_generated && open==0 && sections && design_profile_present && clinical_consistent && competitive_fresh && competitive_updates_pending==0 && competitive_refresh_processing==0 && compliance_ready && stage>=Stage::Review;
        Ok(json!({"ready":ready,"stage":stage.as_str(),"requirements_approved":requirements,"interview_generated":interview_generated,"open_interview_questions":open,"required_sections_approved":sections,"design_profile_present":design_profile_present,"clinical_study_present":clinical_present,"clinical_errors":clinical_errors,"cross_section_conflicts":cross_section_conflicts,"blocking_cross_section_conflicts":blocking_cross_section_conflicts,"clinical_consistent":clinical_consistent,"competitive_intelligence_present":competitive_present,"competitive_intelligence_fresh":competitive_fresh,"competitive_run_id":competitive.get("run_id").cloned().unwrap_or(Value::Null),"competitive_text_updates_pending":competitive_updates_pending,"competitive_refresh_processing_pending":competitive_refresh_processing,"sponsor_compliance_ready":compliance_ready,"sponsor_compliance_hard_failures":compliance_hard_failures,"sponsor_compliance":compliance}))
    }

    pub fn create_export_snapshot(&self,project:&str)->Result<Value>{
        let readiness=self.readiness_json(project)?;
        if !readiness.get("ready").and_then(Value::as_bool).unwrap_or(false){bail!("project is not ready for export: {}",serde_json::to_string(&readiness)?);}
        let sections=self.approved_sections_json(project)?;
        let project_meta=self.project_json(project)?;
        let design=self.design_profile_json(project)?;
        let design_profile=design.get("profile").cloned().unwrap_or(Value::Null);
        let design_profile_sha256=design.get("sha256").cloned().unwrap_or(Value::Null);
        let clinical=self.clinical_study_json(project)?;
        let competitive=self.competitive_latest_json(project)?;
        let competitive_updates=self.competitive_updates_json(project,25)?;
        let compliance_profile=self.compliance_profile_json(project)?;
        let compliance_assessment=self.compliance_assessment_json(project)?;
        let submission_artifacts=self.submission_artifacts_json(project)?;
        let snapshot=json!({"project":project_meta,"sections":sections,"design_profile":design_profile,"design_profile_sha256":design_profile_sha256,"clinical_study":clinical,"competitive_intelligence":competitive,"competitive_updates":competitive_updates,"sponsor_compliance_profile":compliance_profile,"sponsor_compliance_assessment":compliance_assessment,"submission_artifacts":submission_artifacts});
        let bytes=serde_json::to_vec(&snapshot)?; let sha=sha256_hex(&bytes);
        let c=self.conn()?; c.execute("INSERT INTO export_snapshots(project_id,snapshot_json,content_sha256) VALUES(?1,?2,?3)",params![project,String::from_utf8(bytes)?,sha])?;
        let snapshot_id=c.last_insert_rowid(); c.execute("UPDATE projects SET stage='export',updated_at=CURRENT_TIMESTAMP WHERE id=?1",[project])?;
        Ok(json!({"snapshot_id":snapshot_id,"sha256":sha,"sections":sections,"project":project_meta,"design_profile":design_profile,"design_profile_sha256":design_profile_sha256,"clinical_study":clinical,"competitive_intelligence":competitive,"competitive_updates":competitive_updates,"sponsor_compliance_profile":compliance_profile,"sponsor_compliance_assessment":compliance_assessment,"submission_artifacts":submission_artifacts}))
    }


    pub fn competitive_input_fingerprint(&self,project:&str)->Result<String>{
        use sha2::{Digest,Sha256};
        let c=self.conn()?; let mut h=Sha256::new(); h.update(project.as_bytes());
        let meta:(String,Option<String>,Option<String>)=c.query_row("SELECT title,sponsor,mechanism FROM projects WHERE id=?1",[project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        h.update(meta.0.as_bytes()); h.update(meta.1.unwrap_or_default().as_bytes()); h.update(meta.2.unwrap_or_default().as_bytes());
        let aggregates=[
            ("documents","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM documents WHERE project_id=?1"),
            ("requirements","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1"),
            ("interview_answers","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM interview_answers WHERE project_id=?1"),
            ("evidence","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM evidence WHERE project_id=?1"),
            ("clinical_study","SELECT COUNT(*),COALESCE(MAX(version),0),COALESCE(SUM(version),0) FROM clinical_studies WHERE project_id=?1")
        ];
        for(name,sql) in aggregates {let(a,b,cstate):(i64,i64,i64)=c.query_row(sql,[project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;h.update(name.as_bytes());h.update(a.to_le_bytes());h.update(b.to_le_bytes());h.update(cstate.to_le_bytes());}
        Ok(hex::encode(h.finalize()))
    }

    pub fn save_competitive_profile(&self,project:&str,profile:&CompetitiveProfile,source_fingerprint:&str,model:&str)->Result<Value>{
        let bytes=serde_json::to_vec(profile)?; let sha=sha256_hex(&bytes); let raw=String::from_utf8(bytes)?;
        let mut c=self.conn()?; let tx=c.transaction()?;
        let version:i64=tx.query_row("SELECT COALESCE(version,0)+1 FROM competitive_profiles WHERE project_id=?1",[project],|r|r.get(0)).optional()?.unwrap_or(1);
        tx.execute("INSERT INTO competitive_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model) VALUES(?1,?2,?3,?4,?5,?6)",params![project,version,source_fingerprint,raw,sha,model])?;
        tx.execute(r#"INSERT INTO competitive_profiles(project_id,version,source_fingerprint,profile_json,content_sha256,model,updated_at)
          VALUES(?1,?2,?3,?4,?5,?6,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET version=excluded.version,source_fingerprint=excluded.source_fingerprint,profile_json=excluded.profile_json,content_sha256=excluded.content_sha256,model=excluded.model,updated_at=CURRENT_TIMESTAMP"#,params![project,version,source_fingerprint,raw,sha,model])?;
        Self::touch_project_conn(&tx,project)?; tx.commit()?;
        Ok(json!({"version":version,"sha256":sha,"source_fingerprint":source_fingerprint,"model":model,"profile":profile}))
    }

    pub fn competitive_profile_typed(&self,project:&str)->Result<Option<CompetitiveProfile>>{
        let c=self.conn()?; let raw=c.query_row("SELECT profile_json FROM competitive_profiles WHERE project_id=?1",[project],|r|r.get::<_,String>(0)).optional()?;
        match raw{Some(x)=>Ok(Some(serde_json::from_str(&x).context("stored competitive profile is invalid JSON")?)),None=>Ok(None)}
    }

    pub fn competitive_profile_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?; let row=c.query_row("SELECT version,source_fingerprint,profile_json,content_sha256,model,updated_at FROM competitive_profiles WHERE project_id=?1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).optional()?;
        let current=self.competitive_input_fingerprint(project)?;
        if let Some((version,source_fp,raw,sha,model,updated_at))=row{let profile:Value=serde_json::from_str(&raw)?;Ok(json!({"exists":true,"fresh":source_fp==current,"version":version,"source_fingerprint":source_fp,"current_fingerprint":current,"sha256":sha,"model":model,"updated_at":updated_at,"profile":profile}))}else{Ok(json!({"exists":false,"fresh":false,"version":null,"profile":null,"current_fingerprint":current}))}
    }

    pub fn begin_competitive_run(&self,project:&str,profile_version:i64,input_fingerprint:&str,config_sha256:&str)->Result<i64>{
        let c=self.conn()?;c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status) VALUES(?1,?2,?3,?4,'running')",params![project,profile_version,input_fingerprint,config_sha256])?;Ok(c.last_insert_rowid())
    }

    pub fn fail_competitive_run(&self,run_id:i64,detail:&str)->Result<()> {
        let c=self.conn()?;c.execute("UPDATE competitive_runs SET status='failed',provider_status_json=?1,completed_at=CURRENT_TIMESTAMP WHERE id=?2",params![serde_json::to_string(&json!([{"provider":"run","ok":false,"records":0,"detail":detail}]))?,run_id])?;Ok(())
    }

    pub fn finish_competitive_run(&self,project:&str,run_id:i64,out:&CompetitiveRunOutput,resume_stage:Stage)->Result<Value>{
        let strategy_bytes=serde_json::to_vec(&out.strategy)?;let strategy_sha=sha256_hex(&strategy_bytes);let strategy_raw=String::from_utf8(strategy_bytes)?;
        let mut c=self.conn()?;let tx=c.transaction()?;
        let owner:String=tx.query_row("SELECT project_id FROM competitive_runs WHERE id=?1",[run_id],|r|r.get(0))?;if owner!=project{bail!("competitive run does not belong to project");}
        tx.execute("DELETE FROM competitor_candidates WHERE run_id=?1",[run_id])?;tx.execute("DELETE FROM competitor_assets WHERE run_id=?1",[run_id])?;
        for (rank,cand) in out.candidates.iter().enumerate(){tx.execute(r#"INSERT INTO competitor_candidates(run_id,project_id,candidate_key,name,rank,overall_score,grant_score,publication_score,clinical_trial_score,patent_ip_score,technology_score,breadth_score,asset_count,asset_counts_json,dimension_coverage_json)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,params![run_id,project,cand.candidate_key,cand.name,(rank+1) as i64,cand.overall_score,cand.grant_score,cand.publication_score,cand.clinical_trial_score,cand.patent_ip_score,cand.technology_score,cand.breadth_score,cand.asset_count as i64,serde_json::to_string(&cand.asset_counts)?,serde_json::to_string(&cand.dimension_coverage)?])?;}
        for a in &out.assets{tx.execute(r#"INSERT INTO competitor_assets(run_id,project_id,candidate_key,asset_key,provider,asset_type,external_id,title,summary,url,year,amount,dimension_id,metadata_json,relevance)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)"#,params![run_id,project,a.candidate_key,a.asset_key,a.provider,a.asset_type,a.external_id,a.title,a.summary,a.url,a.year,a.amount,a.dimension_id,serde_json::to_string(&a.metadata)?,a.relevance])?;}
        tx.execute("UPDATE competitive_runs SET status='complete',provider_status_json=?1,strategy_json=?2,strategy_sha256=?3,strategy_model=?4,completed_at=CURRENT_TIMESTAMP WHERE id=?5",params![serde_json::to_string(&out.provider_status)?,strategy_raw,strategy_sha,out.strategy_model,run_id])?;
        let target=if resume_stage>=Stage::Writing{resume_stage}else{Stage::Strategy};
        tx.execute("UPDATE projects SET stage=?1,updated_at=CURRENT_TIMESTAMP WHERE id=?2",params![target.as_str(),project])?;tx.commit()?;
        self.competitive_latest_json(project)
    }

    pub fn competitive_latest_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;let current=self.competitive_input_fingerprint(project)?;
        let row=c.query_row("SELECT id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,strategy_json,strategy_sha256,strategy_model,created_at,completed_at FROM competitive_runs WHERE project_id=?1 ORDER BY id DESC LIMIT 1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,String>(9)?,r.get::<_,Option<String>>(10)?))).optional()?;
        let Some((run_id,profile_version,input_fp,config_sha,status,provider_raw,strategy_raw,strategy_sha,strategy_model,created,completed))=row else{return Ok(json!({"exists":false,"fresh":false,"current_fingerprint":current,"candidates":[],"assets":[],"strategy":null}));};
        let mut st=c.prepare("SELECT candidate_key,name,rank,overall_score,grant_score,publication_score,clinical_trial_score,patent_ip_score,technology_score,breadth_score,asset_count,asset_counts_json,dimension_coverage_json FROM competitor_candidates WHERE run_id=?1 ORDER BY rank")?;
        let rows=st.query_map([run_id],|r|Ok(json!({"candidate_key":r.get::<_,String>(0)?,"name":r.get::<_,String>(1)?,"rank":r.get::<_,i64>(2)?,"overall_score":r.get::<_,f64>(3)?,"grant_score":r.get::<_,f64>(4)?,"publication_score":r.get::<_,f64>(5)?,"clinical_trial_score":r.get::<_,f64>(6)?,"patent_ip_score":r.get::<_,f64>(7)?,"technology_score":r.get::<_,f64>(8)?,"breadth_score":r.get::<_,f64>(9)?,"asset_count":r.get::<_,i64>(10)?,"asset_counts":serde_json::from_str::<Value>(&r.get::<_,String>(11)?).unwrap_or(json!({})),"dimension_coverage":serde_json::from_str::<Value>(&r.get::<_,String>(12)?).unwrap_or(json!([]))})))?;let mut candidates=Vec::new();for x in rows{candidates.push(x?);}
        let mut ast=c.prepare("SELECT candidate_key,asset_key,provider,asset_type,external_id,title,summary,url,year,amount,dimension_id,metadata_json,relevance FROM competitor_assets WHERE run_id=?1 ORDER BY relevance DESC,id LIMIT 1000")?;
        let rows=ast.query_map([run_id],|r|{
            let metadata_raw=r.get::<_,String>(11)?;
            Ok(json!({"candidate_key":r.get::<_,String>(0)?,"asset_key":r.get::<_,String>(1)?,"provider":r.get::<_,String>(2)?,"asset_type":r.get::<_,String>(3)?,"external_id":r.get::<_,String>(4)?,"title":r.get::<_,String>(5)?,"summary":r.get::<_,String>(6)?,"url":r.get::<_,Option<String>>(7)?,"year":r.get::<_,Option<i64>>(8)?,"amount":r.get::<_,Option<f64>>(9)?,"dimension_id":r.get::<_,Option<String>>(10)?,"metadata":serde_json::from_str::<Value>(&metadata_raw).unwrap_or(json!({})),"relevance":r.get::<_,f64>(12)?}))
        })?;let mut assets=Vec::new();for x in rows{assets.push(x?);}
        let strategy=strategy_raw.as_deref().and_then(|x|serde_json::from_str::<Value>(x).ok()).unwrap_or(Value::Null);let providers=serde_json::from_str::<Value>(&provider_raw).unwrap_or(json!([]));
        let current_config_sha=current_competitive_config_sha().ok();
        let config_fresh=current_config_sha.as_deref()==Some(config_sha.as_str());
        let refresh_ttl_seconds=std::env::var("COMPETITIVE_REFRESH_TTL_SECONDS").ok().and_then(|v|v.parse::<i64>().ok()).unwrap_or(14_400).clamp(300,604_800);
        let age_seconds:Option<i64>=c.query_row("SELECT CASE WHEN completed_at IS NULL THEN NULL ELSE MAX(0,CAST((julianday('now')-julianday(completed_at))*86400 AS INTEGER)) END FROM competitive_runs WHERE id=?1",[run_id],|r|r.get(0)).optional()?.flatten();
        let time_fresh=age_seconds.map(|age|age<=refresh_ttl_seconds).unwrap_or(false);
        let input_fresh=input_fp==current;
        let complete=status=="complete";
        let fresh=complete&&input_fresh&&config_fresh&&time_fresh;
        let mut stale_reasons=Vec::<String>::new();
        if !complete{stale_reasons.push(format!("status:{status}"));}
        if !input_fresh{stale_reasons.push("project_inputs_changed".into());}
        if !config_fresh{stale_reasons.push("competitive_config_changed".into());}
        if !time_fresh{stale_reasons.push("public_intelligence_refresh_due".into());}
        Ok(json!({"exists":true,"fresh":fresh,"run_id":run_id,"profile_version":profile_version,"input_fingerprint":input_fp,"current_fingerprint":current,"input_fresh":input_fresh,"config_sha256":config_sha,"current_config_sha256":current_config_sha,"config_fresh":config_fresh,"refresh_ttl_seconds":refresh_ttl_seconds,"age_seconds":age_seconds,"time_fresh":time_fresh,"stale_reasons":stale_reasons,"status":status,"provider_status":providers,"strategy":strategy,"strategy_sha256":strategy_sha,"strategy_model":strategy_model,"created_at":created,"completed_at":completed,"candidates":candidates,"assets":assets}))
    }

    pub fn record_competitive_update_event(&self,project:&str,delta:&CompetitiveDelta,refresh_reason:&Value)->Result<i64>{
        let mut c=self.conn()?;
        let tx=c.transaction()?;
        let status=if delta.material{"pending"}else{"complete"};
        if delta.material {
            // A newer material public-intelligence run supersedes unfinished proposals
            // from older runs. Keep the history, but never let stale older proposals
            // reappear after the newest strategy has been published.
            tx.execute("UPDATE competitive_section_updates SET status='superseded',resolved_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND status='pending'",[project])?;
            tx.execute("UPDATE competitive_update_events SET text_refresh_status='complete',text_refresh_errors_json='[\"superseded_by_newer_competitive_refresh\"]',processed_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND material=1 AND text_refresh_status!='complete'",[project])?;
        }
        tx.execute(r#"INSERT INTO competitive_update_events(project_id,from_run_id,to_run_id,refresh_reason_json,delta_json,summary,material,text_refresh_status,processed_at)
          VALUES(?1,?2,?3,?4,?5,?6,?7,?8,CASE WHEN ?8='complete' THEN CURRENT_TIMESTAMP ELSE NULL END)"#,params![project,delta.from_run_id,delta.to_run_id,serde_json::to_string(refresh_reason)?,serde_json::to_string(delta)?,delta.summary,if delta.material{1}else{0},status])?;
        let id=tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    pub fn competitive_update_event_json(&self,project:&str,event_id:i64)->Result<Value>{
        let c=self.conn()?;let row=c.query_row(r#"SELECT id,from_run_id,to_run_id,refresh_reason_json,delta_json,summary,material,text_refresh_status,text_refresh_errors_json,created_at,processed_at
          FROM competitive_update_events WHERE project_id=?1 AND id=?2"#,params![project,event_id],|r|{
            let rr=r.get::<_,String>(3)?;let delta=r.get::<_,String>(4)?;let errors=r.get::<_,String>(8)?;
            Ok(json!({"event_id":r.get::<_,i64>(0)?,"from_run_id":r.get::<_,Option<i64>>(1)?,"to_run_id":r.get::<_,i64>(2)?,"refresh_reason":serde_json::from_str::<Value>(&rr).unwrap_or(json!([])),"delta":serde_json::from_str::<Value>(&delta).unwrap_or(json!({})),"summary":r.get::<_,String>(5)?,"material":r.get::<_,i64>(6)?!=0,"text_refresh_status":r.get::<_,String>(7)?,"text_refresh_errors":serde_json::from_str::<Value>(&errors).unwrap_or(json!([])),"created_at":r.get::<_,String>(9)?,"processed_at":r.get::<_,Option<String>>(10)?}))
        }).optional()?;Ok(row.unwrap_or_else(||json!({})))
    }

    pub fn latest_unprocessed_competitive_update_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;let id=c.query_row("SELECT id FROM competitive_update_events WHERE project_id=?1 AND material=1 AND text_refresh_status!='complete' ORDER BY id DESC LIMIT 1",[project],|r|r.get::<_,i64>(0)).optional()?;
        match id{Some(x)=>self.competitive_update_event_json(project,x),None=>Ok(json!({}))}
    }

    pub fn set_competitive_update_processing(&self,project:&str,event_id:i64,status:&str,errors:&Value)->Result<()> {
        if !matches!(status,"pending"|"partial"|"complete"){bail!("invalid competitive update processing status");}
        let c=self.conn()?;c.execute("UPDATE competitive_update_events SET text_refresh_status=?1,text_refresh_errors_json=?2,processed_at=CASE WHEN ?1='complete' THEN CURRENT_TIMESTAMP ELSE processed_at END WHERE id=?3 AND project_id=?4",params![status,serde_json::to_string(errors)?,event_id,project])?;Ok(())
    }

    pub fn competitive_text_refresh_pending_count(&self,project:&str)->Result<i64>{
        let c=self.conn()?;Ok(c.query_row("SELECT COUNT(*) FROM competitive_update_events WHERE project_id=?1 AND material=1 AND text_refresh_status!='complete'",[project],|r|r.get(0))?)
    }

    pub fn record_competitive_section_update(&self,event_id:i64,project:&str,section_key:&str,base_version_id:i64,proposed_version_id:i64)->Result<i64>{
        let c=self.conn()?;
        // Supersede older unresolved proposals for the same section. The newest public intelligence wins, but the history remains auditable.
        c.execute("UPDATE competitive_section_updates SET status='superseded',resolved_at=CURRENT_TIMESTAMP WHERE project_id=?1 AND section_key=?2 AND status='pending'",params![project,section_key])?;
        c.execute(r#"INSERT INTO competitive_section_updates(event_id,project_id,section_key,base_version_id,proposed_version_id,status) VALUES(?1,?2,?3,?4,?5,'pending')"#,params![event_id,project,section_key,base_version_id,proposed_version_id])?;
        Ok(c.last_insert_rowid())
    }

    pub fn record_competitive_section_no_change(&self,event_id:i64,project:&str,section_key:&str,base_version_id:i64)->Result<i64>{
        let c=self.conn()?;
        c.execute(r#"INSERT OR IGNORE INTO competitive_section_updates(event_id,project_id,section_key,base_version_id,proposed_version_id,status,resolved_at) VALUES(?1,?2,?3,?4,?4,'no_change',CURRENT_TIMESTAMP)"#,params![event_id,project,section_key,base_version_id])?;
        Ok(c.last_insert_rowid())
    }

    pub fn competitive_section_update_exists(&self,event_id:i64,project:&str,section_key:&str)->Result<bool>{
        let c=self.conn()?;let n:i64=c.query_row("SELECT COUNT(*) FROM competitive_section_updates WHERE event_id=?1 AND project_id=?2 AND section_key=?3",params![event_id,project,section_key],|r|r.get(0))?;Ok(n>0)
    }

    pub fn competitive_pending_update_count(&self,project:&str)->Result<i64>{
        let c=self.conn()?;Ok(c.query_row("SELECT COUNT(*) FROM competitive_section_updates WHERE project_id=?1 AND status='pending'",[project],|r|r.get(0))?)
    }

    pub fn competitive_pending_section_updates_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;
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
        let mut out=Vec::new();for row in rows{out.push(row?);}Ok(Value::Array(out))
    }

    pub fn pending_competitive_update_for_section_json(&self,project:&str,section_key:&str)->Result<Value>{
        let c=self.conn()?;
        let row=c.query_row(r#"SELECT csu.id,csu.event_id,csu.base_version_id,csu.proposed_version_id,e.from_run_id,e.to_run_id,e.summary,e.delta_json,e.created_at
          FROM competitive_section_updates csu JOIN competitive_update_events e ON e.id=csu.event_id
          WHERE csu.project_id=?1 AND csu.section_key=?2 AND csu.status='pending' ORDER BY csu.event_id DESC LIMIT 1"#,params![project,section_key],|r|{
            let raw=r.get::<_,String>(7)?;Ok(json!({"id":r.get::<_,i64>(0)?,"event_id":r.get::<_,i64>(1)?,"base_version":r.get::<_,i64>(2)?,"proposed_version":r.get::<_,i64>(3)?,"from_run_id":r.get::<_,Option<i64>>(6)?,"to_run_id":r.get::<_,i64>(5)?,"summary":r.get::<_,String>(6)?,"delta":serde_json::from_str::<Value>(&raw).unwrap_or(json!({})),"created_at":r.get::<_,String>(8)?}))
        }).optional()?;
        Ok(row.unwrap_or_else(||json!({})))
    }

    pub fn competitive_updates_json(&self,project:&str,limit:usize)->Result<Value>{
        let c=self.conn()?; let cap=limit.clamp(1,100) as i64;
        let mut st=c.prepare(r#"SELECT e.id,e.from_run_id,e.to_run_id,e.refresh_reason_json,e.delta_json,e.summary,e.material,e.text_refresh_status,e.text_refresh_errors_json,e.created_at,e.processed_at,
          (SELECT COUNT(*) FROM competitive_section_updates s WHERE s.event_id=e.id) section_updates,
          (SELECT COUNT(*) FROM competitive_section_updates s WHERE s.event_id=e.id AND s.status='pending') pending_updates
          FROM competitive_update_events e WHERE e.project_id=?1 ORDER BY e.id DESC LIMIT ?2"#)?;
        let rows=st.query_map(params![project,cap],|r|{let rr=r.get::<_,String>(3)?;let delta=r.get::<_,String>(4)?;let errors=r.get::<_,String>(8)?;Ok(json!({"event_id":r.get::<_,i64>(0)?,"from_run_id":r.get::<_,Option<i64>>(1)?,"to_run_id":r.get::<_,i64>(2)?,"refresh_reason":serde_json::from_str::<Value>(&rr).unwrap_or(json!([])),"delta":serde_json::from_str::<Value>(&delta).unwrap_or(json!({})),"summary":r.get::<_,String>(5)?,"material":r.get::<_,i64>(6)?!=0,"text_refresh_status":r.get::<_,String>(7)?,"text_refresh_errors":serde_json::from_str::<Value>(&errors).unwrap_or(json!([])),"created_at":r.get::<_,String>(9)?,"processed_at":r.get::<_,Option<String>>(10)?,"section_updates":r.get::<_,i64>(11)?,"pending_updates":r.get::<_,i64>(12)?}))})?;
        let mut events=Vec::new();for row in rows{events.push(row?);}
        Ok(json!({
            "pending":self.competitive_pending_update_count(project)?,
            "processing_pending":self.competitive_text_refresh_pending_count(project)?,
            "pending_sections":self.competitive_pending_section_updates_json(project)?,
            "events":events
        }))
    }

    pub fn competitive_ready(&self,project:&str)->Result<bool>{let x=self.competitive_latest_json(project)?;Ok(x.get("exists").and_then(Value::as_bool).unwrap_or(false)&&x.get("fresh").and_then(Value::as_bool).unwrap_or(false)&&x.get("status").and_then(Value::as_str)==Some("complete"))}

    pub fn competitive_context(&self,project:&str,max_chars:usize)->Result<String>{
        let x=self.competitive_latest_json(project)?;if !x.get("fresh").and_then(Value::as_bool).unwrap_or(false){return Ok("COMPETITIVE APPLICANT INTELLIGENCE: not configured or stale for the current grant/clinical design".into());}
        let mut compact=json!({"notice":"Potential competitors are capability-overlap candidates inferred from public evidence, not confirmed applicants.","candidates":x.get("candidates").and_then(Value::as_array).map(|a|a.iter().take(12).cloned().collect::<Vec<_>>()).unwrap_or_default(),"strategy":x.get("strategy").cloned().unwrap_or(Value::Null),"provider_status":x.get("provider_status").cloned().unwrap_or(json!([]))});
        // Keep only top public assets explicitly referenced by the strategy/candidates for context efficiency.
        if let Some(obj)=compact.as_object_mut(){obj.insert("public_asset_catalog".into(),Value::Array(x.get("assets").and_then(Value::as_array).map(|a|a.iter().take(80).cloned().collect()).unwrap_or_default()));}
        let mut text=serde_json::to_string_pretty(&compact)?;if text.len()>max_chars{text.truncate(max_chars);}Ok(format!("COMPETITIVE APPLICANT INTELLIGENCE (PUBLIC; NOT CONFIRMED APPLICANTS):\n{text}"))
    }


    pub fn opportunity_context(&self,project:&str,max_chars:usize)->Result<String>{
        let c=self.conn()?;
        let mut st=c.prepare("SELECT name,kind,text FROM documents WHERE project_id=?1 AND kind LIKE 'funding_%' ORDER BY id")?;
        let rows=st.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;
        let mut out=String::new();
        for row in rows{let(name,kind,text)=row?;out.push_str(&format!("\n--- {kind}: {name} ---\n{text}\n"));if out.len()>=max_chars{out.truncate(max_chars);break;}}
        Ok(out)
    }

    /// Return each funding-opportunity document as its own immutable source
    /// buffer. Provenance offsets are always relative to one of these buffers,
    /// never to the display-only concatenation produced by opportunity_context.
    pub fn opportunity_documents(&self,project:&str)->Result<Vec<SourceDocument>>{
        let c=self.conn()?;
        let mut st=c.prepare("SELECT id,name,kind,text FROM documents WHERE project_id=?1 AND kind LIKE 'funding_%' ORDER BY id")?;
        let rows=st.query_map([project],|r|Ok(SourceDocument{id:r.get(0)?,name:r.get(1)?,kind:r.get(2)?,text:r.get(3)?}))?;
        let mut out=Vec::new();for row in rows{out.push(row?);}Ok(out)
    }

    pub fn opportunity_source_fingerprint(&self,project:&str)->Result<String>{
        use sha2::{Digest,Sha256}; let c=self.conn()?; let mut h=Sha256::new();h.update(project.as_bytes());
        let mut st=c.prepare("SELECT name,kind,sha256 FROM documents WHERE project_id=?1 AND kind LIKE 'funding_%' ORDER BY id")?;
        let rows=st.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;
        let mut count=0u64;for row in rows{let(name,kind,sha)=row?;count+=1;h.update(name.as_bytes());h.update(kind.as_bytes());h.update(sha.as_bytes());}
        h.update(count.to_le_bytes());Ok(hex::encode(h.finalize()))
    }

    fn sync_compliance_required_sections_tx(tx:&rusqlite::Transaction<'_>,project:&str,profile:&ComplianceProfile)->Result<()> {
        let required=profile.rules.iter().filter(|r|r.rule_type=="required_section" && r.mandatory && !r.target.trim().is_empty()).map(|r|(section_key(&r.target),r.target.trim().to_string())).collect::<Vec<_>>();
        // Sections introduced solely by an older compliance profile remain visible for audit/history,
        // but stop blocking export when the current sponsor rules no longer require them.
        tx.execute("UPDATE project_sections SET required=0 WHERE project_id=?1 AND origin='compliance'",[project])?;
        for (key,title) in required {
            let existing:Option<String>=tx.query_row("SELECT origin FROM project_sections WHERE project_id=?1 AND section_key=?2",params![project,key],|r|r.get(0)).optional()?;
            match existing {
                Some(origin)=>{
                    if origin=="compliance" {tx.execute("UPDATE project_sections SET title=?1,required=1 WHERE project_id=?2 AND section_key=?3",params![title,project,key])?;}
                    else {tx.execute("UPDATE project_sections SET required=1 WHERE project_id=?1 AND section_key=?2",params![project,key])?;}
                },
                None=>{
                    let next:i64=tx.query_row("SELECT COALESCE(MAX(position),-1)+1 FROM project_sections WHERE project_id=?1",[project],|r|r.get(0))?;
                    tx.execute("INSERT INTO project_sections(project_id,section_key,title,position,required,origin) VALUES(?1,?2,?3,?4,1,'compliance')",params![project,key,title,next])?;
                }
            }
        }
        Ok(())
    }

    pub fn compliance_render_fingerprint(&self,project:&str)->Result<String>{
        use sha2::{Digest,Sha256};
        let mut h=Sha256::new();
        h.update(self.approved_sections_fingerprint(project)?.as_bytes());
        let design=self.design_profile_json(project)?;
        h.update(design.get("sha256").and_then(Value::as_str).unwrap_or("").as_bytes());
        let clinical=self.clinical_study_json(project)?;
        h.update(clinical.get("sha256").and_then(Value::as_str).unwrap_or("").as_bytes());
        Ok(hex::encode(h.finalize()))
    }

    pub fn save_compliance_profile(&self,project:&str,profile:&ComplianceProfile,model:&str)->Result<Value>{
        crate::compliance::validate_profile(profile)?;
        let documents=self.opportunity_documents(project)?;
        crate::source_locator::validate_exact_sources(profile,&documents)?;
        let source_fp=self.opportunity_source_fingerprint(project)?;let raw=serde_json::to_string(profile)?;let sha=sha256_hex(raw.as_bytes());
        let mut c=self.conn()?;let tx=c.transaction()?;
        let version:i64=tx.query_row("SELECT COALESCE(version,0)+1 FROM compliance_profiles WHERE project_id=?1",[project],|r|r.get(0)).optional()?.unwrap_or(1);
        tx.execute("INSERT INTO compliance_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved) VALUES(?1,?2,?3,?4,?5,?6,0)",params![project,version,source_fp,raw,sha,model])?;
        tx.execute(r#"INSERT INTO compliance_profiles(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved,updated_at)
          VALUES(?1,?2,?3,?4,?5,?6,0,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET version=excluded.version,source_fingerprint=excluded.source_fingerprint,profile_json=excluded.profile_json,content_sha256=excluded.content_sha256,model=excluded.model,approved=0,updated_at=CURRENT_TIMESTAMP"#,params![project,version,source_fp,raw,sha,model])?;
        for rule in &profile.rules {
            tx.execute(r#"INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt)
              VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"#,params![project,version,rule.rule_id,rule.source_status,rule.source_hint,rule.source_document_id,rule.source_start_offset.map(|v|v as i64),rule.source_end_offset.map(|v|v as i64),rule.source_page.map(i64::from),rule.source_excerpt])?;
        }
        tx.execute("DELETE FROM compliance_resolutions WHERE project_id=?1",[project])?;
        Self::sync_compliance_required_sections_tx(&tx,project,profile)?;
        Self::touch_project_conn(&tx,project)?;tx.commit()?;
        Ok(json!({"version":version,"sha256":sha,"source_fingerprint":source_fp,"model":model,"approved":false,"profile":profile}))
    }

    pub fn approve_compliance_profile(&self,project:&str)->Result<Value>{
        let current=self.compliance_profile_json(project)?;
        if !current.get("exists").and_then(Value::as_bool).unwrap_or(false){bail!("compile the sponsor compliance profile before approval");}
        if !current.get("fresh").and_then(Value::as_bool).unwrap_or(false){bail!("compliance profile is stale because the funding opportunity source changed; recompile it before approval");}
        let profile=self.compliance_profile_typed(project)?.context("compile the sponsor compliance profile before approval")?;
        let unresolved=profile.rules.iter().filter(|r|r.source_status!="located").map(|r|r.rule_id.as_str()).collect::<Vec<_>>();
        if !unresolved.is_empty(){bail!("exact source text was not located for rule(s) {}; correct their source hints and save again before approval",unresolved.join(", "));}
        crate::source_locator::validate_exact_sources(&profile,&self.opportunity_documents(project)?)?;
        let c=self.conn()?;c.execute("UPDATE compliance_profiles SET approved=1,updated_at=CURRENT_TIMESTAMP WHERE project_id=?1",[project])?;
        let version=current.get("version").and_then(Value::as_i64).unwrap_or(0);c.execute("UPDATE compliance_profile_history SET approved=1 WHERE project_id=?1 AND version=?2",params![project,version])?;Self::touch_project_conn(&c,project)?;
        self.compliance_profile_json(project)
    }

    pub fn compliance_profile_typed(&self,project:&str)->Result<Option<ComplianceProfile>>{
        let c=self.conn()?;let raw=c.query_row("SELECT profile_json FROM compliance_profiles WHERE project_id=?1",[project],|r|r.get::<_,String>(0)).optional()?;
        match raw{Some(x)=>Ok(Some(serde_json::from_str(&x).context("stored compliance profile is invalid JSON")?)),None=>Ok(None)}
    }

    pub fn compliance_profile_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;let current=self.opportunity_source_fingerprint(project)?;
        let row=c.query_row("SELECT version,source_fingerprint,profile_json,content_sha256,model,approved,updated_at FROM compliance_profiles WHERE project_id=?1",[project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?!=0,r.get::<_,String>(6)?))).optional()?;
        if let Some((version,fp,raw,sha,model,approved,updated_at))=row{let profile:Value=serde_json::from_str(&raw)?;Ok(json!({"exists":true,"fresh":fp==current,"version":version,"source_fingerprint":fp,"current_fingerprint":current,"sha256":sha,"model":model,"approved":approved,"updated_at":updated_at,"profile":profile}))}else{Ok(json!({"exists":false,"fresh":false,"approved":false,"version":null,"profile":null,"current_fingerprint":current}))}
    }

    pub fn resolve_compliance_rule(&self,project:&str,rule_id:&str,status:&str,notes:&str,resolved_by:Option<&str>)->Result<Value>{
        if !matches!(status,"satisfied"|"not_applicable"|"waived"|"unresolved"){bail!("invalid compliance resolution status");}
        let profile=self.compliance_profile_typed(project)?.context("compile compliance profile first")?;
        if !profile.rules.iter().any(|r|r.rule_id==rule_id){bail!("unknown compliance rule {rule_id}");}
        let c=self.conn()?;
        if status=="unresolved"{c.execute("DELETE FROM compliance_resolutions WHERE project_id=?1 AND rule_id=?2",params![project,rule_id])?;}
        else{c.execute(r#"INSERT INTO compliance_resolutions(project_id,rule_id,status,notes,resolved_by,created_at) VALUES(?1,?2,?3,?4,?5,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id,rule_id) DO UPDATE SET status=excluded.status,notes=excluded.notes,resolved_by=excluded.resolved_by,created_at=CURRENT_TIMESTAMP"#,params![project,rule_id,status,notes,resolved_by])?;}
        Self::touch_project_conn(&c,project)?;self.compliance_assessment_json(project)
    }

    pub fn register_submission_artifact(&self,project:&str,slot:&str,filename:&str,path:&str,sha:&str,extension:&str)->Result<Value>{
        if slot.trim().is_empty()||filename.trim().is_empty()||sha.trim().is_empty(){bail!("submission artifact slot, filename, and sha256 are required");}
        let workspace=self.path.parent().context("grant database has no workspace parent")?;
        let expected_root=workspace.join("projects").join(project).join("submission");
        let expected_root=expected_root.canonicalize().unwrap_or(expected_root);
        let artifact=std::path::PathBuf::from(path);
        let resolved=artifact.canonicalize().with_context(||format!("submission artifact does not exist: {path}"))?;
        if !resolved.starts_with(&expected_root){bail!("submission artifact path must be inside the project submission workspace");}
        let bytes=std::fs::read(&resolved)?;let actual_sha=sha256_hex(&bytes);
        if actual_sha!=sha{bail!("submission artifact SHA-256 does not match file contents");}
        let ext=extension.trim().trim_start_matches('.').to_ascii_lowercase();
        let c=self.conn()?;c.execute("INSERT OR IGNORE INTO submission_artifacts(project_id,slot,filename,path,sha256,extension) VALUES(?1,?2,?3,?4,?5,?6)",params![project,slot.trim(),filename.trim(),resolved.to_string_lossy().to_string(),sha,ext])?;Self::touch_project_conn(&c,project)?;self.submission_artifacts_json(project)
    }

    pub fn submission_artifacts_json(&self,project:&str)->Result<Value>{
        let c=self.conn()?;let mut st=c.prepare("SELECT id,slot,filename,path,sha256,extension,created_at FROM submission_artifacts WHERE project_id=?1 ORDER BY slot,id")?;
        let rows=st.query_map([project],|r|Ok(json!({"id":r.get::<_,i64>(0)?,"slot":r.get::<_,String>(1)?,"filename":r.get::<_,String>(2)?,"path":r.get::<_,String>(3)?,"sha256":r.get::<_,String>(4)?,"extension":r.get::<_,String>(5)?,"created_at":r.get::<_,String>(6)?})))?;let mut out=vec![];for row in rows{out.push(row?);}Ok(Value::Array(out))
    }

    pub fn approved_sections_fingerprint(&self,project:&str)->Result<String>{
        use sha2::{Digest,Sha256};let c=self.conn()?;let mut h=Sha256::new();h.update(project.as_bytes());let mut st=c.prepare(r#"SELECT ps.section_key,sv.id,sv.body FROM project_sections ps JOIN section_versions sv ON sv.project_id=ps.project_id AND sv.section_key=ps.section_key AND sv.approved=1 WHERE ps.project_id=?1 ORDER BY ps.position"#)?;let rows=st.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))?;for row in rows{let(k,id,b)=row?;h.update(k.as_bytes());h.update(id.to_le_bytes());h.update(b.as_bytes());}Ok(hex::encode(h.finalize()))
    }

    pub fn save_compliance_measurements(&self,project:&str,measurements:&Value)->Result<Value>{
        let fp=self.compliance_render_fingerprint(project)?;let raw=serde_json::to_string(measurements)?;let c=self.conn()?;c.execute(r#"INSERT INTO compliance_measurements(project_id,approved_sections_fingerprint,measurements_json,updated_at) VALUES(?1,?2,?3,CURRENT_TIMESTAMP)
          ON CONFLICT(project_id) DO UPDATE SET approved_sections_fingerprint=excluded.approved_sections_fingerprint,measurements_json=excluded.measurements_json,updated_at=CURRENT_TIMESTAMP"#,params![project,fp,raw])?;Self::touch_project_conn(&c,project)?;self.compliance_assessment_json(project)
    }

    pub fn compliance_assessment_json(&self,project:&str)->Result<Value>{
        let profile_state=self.compliance_profile_json(project)?;
        let Some(profile)=self.compliance_profile_typed(project)? else{return Ok(json!({"exists":false,"ready":false,"hard_failures":1,"findings":[],"reason":"compliance profile not compiled"}));};
        let c=self.conn()?;
        let mut resolutions=std::collections::HashMap::new();{let mut st=c.prepare("SELECT rule_id,status,notes FROM compliance_resolutions WHERE project_id=?1")?;let rows=st.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?;for row in rows{let(k,s,n)=row?;resolutions.insert(k,(s,n));}}
        let sections=self.approved_sections_json(project)?.as_array().cloned().unwrap_or_default().into_iter().map(|x|(x.get("section_key").and_then(Value::as_str).unwrap_or("").to_string(),x.get("title").and_then(Value::as_str).unwrap_or("").to_string(),x.get("body").and_then(Value::as_str).unwrap_or("").to_string())).collect::<Vec<_>>();
        let artifacts=self.submission_artifacts_json(project)?.as_array().cloned().unwrap_or_default().into_iter().map(|x|(x.get("slot").and_then(Value::as_str).unwrap_or("").to_string(),x.get("filename").and_then(Value::as_str).unwrap_or("").to_string(),x.get("extension").and_then(Value::as_str).unwrap_or("").to_string())).collect::<Vec<_>>();
        let design=self.design_profile_json(project)?.get("profile").cloned().unwrap_or(json!({}));
        let current_fp=self.compliance_render_fingerprint(project)?;let measurement_row=c.query_row("SELECT approved_sections_fingerprint,measurements_json FROM compliance_measurements WHERE project_id=?1",[project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;let measurements=measurement_row.and_then(|(fp,raw)|if fp==current_fp{serde_json::from_str::<Value>(&raw).ok()}else{None});
        let project_period_months=self.clinical_study_typed(project)?.and_then(|study|study.timeline.iter().map(|t|t.start_month+t.duration_months).filter(|v|v.is_finite()).reduce(f64::max));
        let facts=ComplianceFacts{approved_sections:sections,artifacts,design_profile:design,measurements,project_period_months};let mut result=evaluate_compliance(&profile,&facts,&resolutions);
        if let Some(obj)=result.as_object_mut(){obj.insert("exists".into(),Value::Bool(true));obj.insert("profile_approved".into(),profile_state.get("approved").cloned().unwrap_or(Value::Bool(false)));obj.insert("profile_fresh".into(),profile_state.get("fresh").cloned().unwrap_or(Value::Bool(false)));obj.insert("profile_version".into(),profile_state.get("version").cloned().unwrap_or(Value::Null));obj.insert("source_fingerprint".into(),profile_state.get("source_fingerprint").cloned().unwrap_or(Value::Null));let rule_ready=obj.get("ready").and_then(Value::as_bool).unwrap_or(false);let approved=profile_state.get("approved").and_then(Value::as_bool).unwrap_or(false);let fresh=profile_state.get("fresh").and_then(Value::as_bool).unwrap_or(false);obj.insert("ready".into(),Value::Bool(rule_ready&&approved&&fresh));}
        Ok(result)
    }

    pub fn compliance_context(&self,project:&str,max_chars:usize)->Result<String>{let profile=self.compliance_profile_json(project)?;let assessment=self.compliance_assessment_json(project)?;let mut s=serde_json::to_string_pretty(&json!({"profile":profile,"assessment":assessment}))?;if s.len()>max_chars{s.truncate(max_chars);}Ok(format!("SPONSOR COMPLIANCE / SUBMISSION RULES:\n{s}"))}

    pub fn retrieval_fingerprint(&self,project:&str)->Result<String>{
        use sha2::{Digest,Sha256}; let c=self.conn()?; let mut h=Sha256::new(); h.update(project.as_bytes());
        let aggregates=[
            ("documents","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM documents WHERE project_id=?1"),
            ("document_chunks","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM document_chunks WHERE project_id=?1"),
            ("requirements","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM requirements WHERE project_id=?1"),
            ("interview_answers","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM interview_answers WHERE project_id=?1"),
            ("evidence","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM evidence WHERE project_id=?1"),
            ("citations","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(verified),0) FROM citations WHERE project_id=?1"),
            ("approved_sections","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(approved),0) FROM section_versions WHERE project_id=?1 AND approved=1"),
            ("clinical_study","SELECT COUNT(*),COALESCE(MAX(version),0),COALESCE(SUM(version),0) FROM clinical_studies WHERE project_id=?1"),
            ("competitive_runs","SELECT COUNT(*),COALESCE(MAX(id),0),COALESCE(SUM(CASE WHEN status='complete' THEN 1 ELSE 0 END),0) FROM competitive_runs WHERE project_id=?1"),
            ("competitor_candidates","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM competitor_candidates WHERE project_id=?1"),
            ("competitor_assets","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM competitor_assets WHERE project_id=?1"),
            ("compliance_profiles","SELECT COUNT(*),COALESCE(MAX(version),0),COALESCE(SUM(approved),0) FROM compliance_profiles WHERE project_id=?1"),
            ("submission_artifacts","SELECT COUNT(*),COALESCE(MAX(id),0),0 FROM submission_artifacts WHERE project_id=?1")
        ];
        for(name,sql)in aggregates{let(count,maxid,state):(i64,i64,i64)=c.query_row(sql,[project],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?; h.update(name.as_bytes()); h.update(count.to_le_bytes()); h.update(maxid.to_le_bytes()); h.update(state.to_le_bytes());}
        Ok(hex::encode(h.finalize()))
    }

    pub fn retrieval_records(&self,project:&str)->Result<Vec<RetrievalRecord>>{
        let c=self.conn()?; let mut out=Vec::<RetrievalRecord>::new();
        {let mut st=c.prepare("SELECT external_id,requirement,mandatory,status,CAST(strftime('%s',created_at) AS INTEGER) FROM requirements WHERE project_id=?1 AND approved=1 ORDER BY id")?; let rows=st.query_map([project],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?!=0,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))?; for row in rows{let(id,text,mandatory,status,created)=row?; out.push(RetrievalRecord{row:0,item_id:format!("requirement:{id}"),kind:"requirement".into(),requirement_id:Some(id.clone()),source_ref:id,source_url:None,source_locator:None,text,confidence:if mandatory{1.0}else{0.8},status,created_unix:Some(created)});}}
        {let mut st=c.prepare("SELECT dc.id,d.name,dc.ordinal,dc.start_word,dc.end_word,dc.text,CAST(strftime('%s',d.created_at) AS INTEGER) FROM document_chunks dc JOIN documents d ON d.id=dc.document_id WHERE dc.project_id=?1 ORDER BY dc.id")?; let rows=st.query_map([project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,i64>(6)?)))?; for row in rows{let(id,name,ord,start,end,text,created)=row?; out.push(RetrievalRecord{row:0,item_id:format!("document_chunk:{id}"),kind:"document_chunk".into(),requirement_id:None,source_ref:name,source_url:None,source_locator:Some(format!("chunk {ord}; words {start}-{end}")),text,confidence:0.75,status:"source_material".into(),created_unix:Some(created)});}}
        {let mut st=c.prepare("SELECT id,requirement_external_id,source_type,source_ref,claim,passage,source_url,source_locator,confidence,status,CAST(strftime('%s',created_at) AS INTEGER) FROM evidence WHERE project_id=?1 ORDER BY id")?; let rows=st.query_map([project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,f64>(8)?,r.get::<_,String>(9)?,r.get::<_,i64>(10)?)))?; for row in rows{let(id,req,kind,src,claim,passage,url,loc,conf,status,created)=row?; out.push(RetrievalRecord{row:0,item_id:format!("evidence:{id}"),kind,requirement_id:req,source_ref:src,source_url:url,source_locator:loc,text:format!("{claim}\n\n{passage}"),confidence:conf as f32,status,created_unix:Some(created)});}}
        {let mut st=c.prepare("SELECT a.id,q.requirement_external_id,q.question,a.value_json,a.confidence,a.classification,a.notes,CAST(strftime('%s',a.created_at) AS INTEGER) FROM interview_answers a JOIN interview_questions q ON q.id=a.question_id WHERE a.project_id=?1 ORDER BY a.id")?; let rows=st.query_map([project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,i64>(7)?)))?; for row in rows{let(id,req,q,v,confidence,class,notes,created)=row?; let conf=match confidence.as_str(){"high"=>0.95,"medium"=>0.7,"low"=>0.45,_=>0.5}; let text=format!("Question: {q}\nAnswer: {v}\nNotes: {}",notes.unwrap_or_default()); out.push(RetrievalRecord{row:0,item_id:format!("interview_answer:{id}"),kind:"interview_answer".into(),requirement_id:Some(req),source_ref:format!("interview_answer:{id}"),source_url:None,source_locator:None,text,confidence:conf,status:class,created_unix:Some(created)});}}
        {let mut st=c.prepare(r#"SELECT sv.id,sv.section_key,ps.title,sv.body,CAST(strftime('%s',sv.created_at) AS INTEGER) FROM section_versions sv JOIN project_sections ps ON ps.project_id=sv.project_id AND ps.section_key=sv.section_key WHERE sv.project_id=?1 AND sv.approved=1 ORDER BY ps.position"#)?; let rows=st.query_map([project],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?)))?; for row in rows{let(id,key,title,body,created)=row?; out.push(RetrievalRecord{row:0,item_id:format!("approved_section:{id}"),kind:"approved_section".into(),requirement_id:None,source_ref:key,source_url:None,source_locator:None,text:format!("{title}\n\n{body}"),confidence:1.0,status:"approved".into(),created_unix:Some(created)});}}
        if let Some(study)=self.clinical_study_typed(project)? {
            let assessment=crate::clinical::assess(&study,&self.approved_sections_json(project)?);
            let text=format!("Clinical Study Model\n{}\n\nDeterministic Assessment\n{}",serde_json::to_string_pretty(&study)?,serde_json::to_string_pretty(&assessment)?);
            out.push(RetrievalRecord{row:0,item_id:"clinical_study:authoritative".into(),kind:"clinical_study".into(),requirement_id:None,source_ref:"clinical_study_model".into(),source_url:None,source_locator:None,text,confidence:1.0,status:"authoritative".into(),created_unix:Some(time::OffsetDateTime::now_utc().unix_timestamp())});
        }
        if let Some(profile)=self.compliance_profile_typed(project)? {
            let assessment=self.compliance_assessment_json(project)?;
            let text=format!("Sponsor Compliance Profile\n{}\n\nDeterministic Compliance Assessment\n{}",serde_json::to_string_pretty(&profile)?,serde_json::to_string_pretty(&assessment)?);
            out.push(RetrievalRecord{row:0,item_id:"sponsor_compliance:authoritative".into(),kind:"sponsor_compliance".into(),requirement_id:None,source_ref:"sponsor_compliance_profile".into(),source_url:None,source_locator:None,text,confidence:1.0,status:if assessment.get("ready").and_then(Value::as_bool).unwrap_or(false){"ready".into()}else{"needs_attention".into()},created_unix:Some(time::OffsetDateTime::now_utc().unix_timestamp())});
        }
        if self.competitive_ready(project).unwrap_or(false) {
            let competitive=self.competitive_latest_json(project)?;
            if let Some(run_id)=competitive.get("run_id").and_then(Value::as_i64){
                if let Some(candidates)=competitive.get("candidates").and_then(Value::as_array){for c in candidates.iter().take(20){
                    let key=c.get("candidate_key").and_then(Value::as_str).unwrap_or("candidate");let name=c.get("name").and_then(Value::as_str).unwrap_or(key);let score=c.get("overall_score").and_then(Value::as_f64).unwrap_or(0.0) as f32;
                    out.push(RetrievalRecord{row:0,item_id:format!("competitive_candidate:{run_id}:{key}"),kind:"competitive_candidate".into(),requirement_id:None,source_ref:name.into(),source_url:None,source_locator:Some(format!("competitive run {run_id}")),text:serde_json::to_string_pretty(c)?,confidence:score.clamp(0.0,1.0),status:"potential_match_public_evidence".into(),created_unix:Some(time::OffsetDateTime::now_utc().unix_timestamp())});
                }}
                if let Some(strategy)=competitive.get("strategy").filter(|v|!v.is_null()){out.push(RetrievalRecord{row:0,item_id:format!("competitive_strategy:{run_id}"),kind:"competitive_strategy".into(),requirement_id:None,source_ref:"public_competitive_positioning".into(),source_url:None,source_locator:Some(format!("competitive run {run_id}")),text:serde_json::to_string_pretty(strategy)?,confidence:1.0,status:"public_evidence_strategy".into(),created_unix:Some(time::OffsetDateTime::now_utc().unix_timestamp())});}
            }
        }
        Ok(out)
    }
}

fn current_competitive_config_sha()->Result<String>{
    let path=std::env::var("COMPETITIVE_CONFIG_PATH").unwrap_or_else(|_|"/app/config/competitive_intelligence.json".into());
    let raw=std::fs::read_to_string(&path).with_context(||format!("read competitive intelligence config {path}"))?;
    let cfg:CompetitiveConfig=serde_json::from_str(&raw).context("parse competitive intelligence config for freshness check")?;
    Ok(sha256_hex(&serde_json::to_vec(&cfg)?))
}

fn section_key(title:&str)->String{
    let mut out=String::with_capacity(title.len()); let mut underscore=false;
    for ch in title.chars(){ if ch.is_ascii_alphanumeric(){out.push(ch.to_ascii_lowercase());underscore=false;} else if !underscore && !out.is_empty(){out.push('_');underscore=true;} }
    out.trim_matches('_').to_string()
}

fn sha256_hex(bytes:&[u8])->String{use sha2::{Digest,Sha256}; let mut h=Sha256::new(); h.update(bytes); hex::encode(h.finalize())}

#[cfg(test)]
mod phase6_storage_tests {
    use super::*;
    use crate::competitive_updates::CompetitiveDelta;

    fn temp_db(name:&str)->std::path::PathBuf {
        let mut p=std::env::temp_dir();
        p.push(format!("grant-core-{name}-{}-{}.db",std::process::id(),uuid::Uuid::new_v4()));
        p
    }

    #[test]
    fn database_enforces_exact_compliance_source_bytes() -> Result<()> {
        let path=temp_db("compliance-source-trigger");
        let store=Store::open(&path)?;
        let project="project-source-trigger";
        store.create_project(project,"Test",None,None,&[])?;
        let source="Préface\nThe Research Strategy may not exceed 12 pages.";
        let(document_id,_)=store.add_document(project,"Pasted funding opportunity","funding_paste",source,"source-sha")?;
        let start=source.find("The Research").unwrap();let end=source.len();
        let c=store.conn()?;
        c.execute("INSERT INTO compliance_profile_history(project_id,version,source_fingerprint,profile_json,content_sha256,model,approved) VALUES(?1,1,'fp','{}','sha','test',0)",[project])?;
        c.execute(r#"INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt)
          VALUES(?1,1,'C-001','located','Research Strategy page limitation',?2,?3,?4,NULL,?5)"#,params![project,document_id,start as i64,end as i64,&source[start..end]])?;
        let error=c.execute(r#"INSERT INTO compliance_rule_sources(project_id,profile_version,rule_id,source_status,source_hint,source_document_id,source_start_offset,source_end_offset,source_page,source_excerpt)
          VALUES(?1,1,'C-002','located','Research Strategy page limitation',?2,?3,?4,NULL,'The Research Strategy must not exceed twelve pages.')"#,params![project,document_id,start as i64,end as i64]).unwrap_err();
        assert!(error.to_string().contains("exact document byte slice"));
        drop(c);let _=std::fs::remove_file(path);Ok(())
    }

    #[test]
    fn competitive_proposal_never_overwrites_human_approved_version() -> Result<()> {
        let path=temp_db("competitive-protection");
        let store=Store::open(&path)?;
        let project="project-test";
        store.create_project(project,"Test",Some("Sponsor"),Some("R01"),&["Specific Aims".into()])?;
        let base=store.save_section(project,"specific_aims","Specific Aims","Human approved baseline",None,"human_edit")?;
        store.approve_section_version(project,"specific_aims",base)?;

        let c=store.conn()?;
        c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,created_at,completed_at) VALUES(?1,1,'fp','cfg','complete','[]',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",[project])?;
        let run1=c.last_insert_rowid();
        c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,created_at,completed_at) VALUES(?1,1,'fp','cfg','complete','[]',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",[project])?;
        let run2=c.last_insert_rowid();
        drop(c);

        let delta=CompetitiveDelta{
            from_run_id:Some(run1),to_run_id:run2,material:true,public_data_changed:true,provider_degraded:false,
            strategy_changed:true,broad_strategy_change:true,changed_section_keys:vec!["specific_aims".into()],
            new_candidates:vec!["candidate_b".into()],removed_candidates:vec![],score_changes:vec![],
            new_asset_keys:vec!["asset_b".into()],removed_asset_keys:vec![],summary:"New public competitor data".into()
        };
        let event=store.record_competitive_update_event(project,&delta,&json!(["public_intelligence_refresh_due"]))?;
        let proposed=store.save_section(project,"specific_aims","Specific Aims","Agent proposed updated text",None,"agentic_competitive_update")?;
        store.record_competitive_section_update(event,project,"specific_aims",base,proposed)?;

        let state=store.section_state_json(project,"specific_aims")?;
        assert_eq!(state.pointer("/approved/version").and_then(Value::as_i64),Some(base));
        assert_eq!(state.pointer("/latest/version").and_then(Value::as_i64),Some(proposed));
        assert_eq!(store.competitive_pending_update_count(project)?,1);
        let approved=store.approved_sections_json(project)?;
        assert_eq!(approved.pointer("/0/body").and_then(Value::as_str),Some("Human approved baseline"));

        store.approve_section_version(project,"specific_aims",proposed)?;
        assert_eq!(store.competitive_pending_update_count(project)?,0);
        let approved=store.approved_sections_json(project)?;
        assert_eq!(approved.pointer("/0/body").and_then(Value::as_str),Some("Agent proposed updated text"));

        let _=std::fs::remove_file(path);
        Ok(())
    }

    #[test]
    fn newer_material_refresh_supersedes_older_pending_proposals() -> Result<()> {
        let path=temp_db("competitive-supersede");
        let store=Store::open(&path)?;
        let project="project-supersede";
        store.create_project(project,"Test",None,None,&["Specific Aims".into()])?;
        let base=store.save_section(project,"specific_aims","Specific Aims","Baseline",None,"human_edit")?;
        store.approve_section_version(project,"specific_aims",base)?;
        let c=store.conn()?;
        let mut runs=Vec::new();
        for _ in 0..3 {
            c.execute("INSERT INTO competitive_runs(project_id,profile_version,input_fingerprint,config_sha256,status,provider_status_json,created_at,completed_at) VALUES(?1,1,'fp','cfg','complete','[]',CURRENT_TIMESTAMP,CURRENT_TIMESTAMP)",[project])?;
            runs.push(c.last_insert_rowid());
        }
        drop(c);
        let mk=|from:i64,to:i64,label:&str| CompetitiveDelta{
            from_run_id:Some(from),to_run_id:to,material:true,public_data_changed:true,provider_degraded:false,
            strategy_changed:true,broad_strategy_change:true,changed_section_keys:vec!["specific_aims".into()],
            new_candidates:vec![label.into()],removed_candidates:vec![],score_changes:vec![],new_asset_keys:vec![format!("asset-{label}")],removed_asset_keys:vec![],summary:label.into()
        };
        let e1=store.record_competitive_update_event(project,&mk(runs[0],runs[1],"first"),&json!(["public_intelligence_refresh_due"]))?;
        let proposed=store.save_section(project,"specific_aims","Specific Aims","First proposal",None,"agentic_competitive_update")?;
        store.record_competitive_section_update(e1,project,"specific_aims",base,proposed)?;
        assert_eq!(store.competitive_pending_update_count(project)?,1);

        let _e2=store.record_competitive_update_event(project,&mk(runs[1],runs[2],"second"),&json!(["public_intelligence_refresh_due"]))?;
        assert_eq!(store.competitive_pending_update_count(project)?,0);
        let old=store.competitive_update_event_json(project,e1)?;
        assert_eq!(old.get("text_refresh_status").and_then(Value::as_str),Some("complete"));
        assert!(old.get("text_refresh_errors").and_then(Value::as_array).is_some_and(|x|x.iter().any(|v|v.as_str()==Some("superseded_by_newer_competitive_refresh"))));

        let _=std::fs::remove_file(path);
        Ok(())
    }
}
