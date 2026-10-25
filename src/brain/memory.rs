//! Memória de longo prazo em SQLite: histórico da conversa, fatos sobre o usuário e lembretes.

use std::path::PathBuf;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

pub struct Memory {
    db: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    /// "user" ou "assistant".
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub id: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub id: i64,
    /// Horário em segundos Unix.
    pub due: i64,
    pub text: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    text TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS reminders (
    id INTEGER PRIMARY KEY,
    created INTEGER NOT NULL,
    due INTEGER NOT NULL,
    text TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS reminders_pending ON reminders(done, due);
";

pub fn now() -> i64 {
    chrono::Local::now().timestamp()
}

impl Memory {
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "sexta-feira").map(|d| d.data_dir().join("memoria.db"))
    }

    pub fn open_default() -> Result<Self> {
        match Self::path() {
            Some(path) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                Self::from_conn(Connection::open(path)?)
            }
            None => Self::in_memory(),
        }
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(db: Connection) -> Result<Self> {
        // WAL deixa as gravações baratas e não bloqueia leitura.
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db })
    }

    pub fn add_turn(&self, role: &str, content: &str) -> Result<()> {
        self.db.execute("INSERT INTO messages (ts, role, content) VALUES (?1, ?2, ?3)", params![now(), role, content])?;
        Ok(())
    }

    /// As últimas `n` mensagens, da mais antiga para a mais nova.
    pub fn recent_turns(&self, n: usize) -> Result<Vec<Turn>> {
        let mut stmt = self.db.prepare("SELECT role, content FROM messages ORDER BY id DESC LIMIT ?1")?;
        let mut turns: Vec<Turn> = stmt
            .query_map([n as i64], |r| Ok(Turn { role: r.get(0)?, content: r.get(1)? }))?
            .collect::<Result<_, _>>()?;
        turns.reverse();
        Ok(turns)
    }

    pub fn add_fact(&self, text: &str) -> Result<i64> {
        self.db.execute("INSERT INTO facts (ts, text) VALUES (?1, ?2)", params![now(), text])?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn facts(&self) -> Result<Vec<Fact>> {
        let mut stmt = self.db.prepare("SELECT id, text FROM facts ORDER BY id")?;
        let facts = stmt.query_map([], |r| Ok(Fact { id: r.get(0)?, text: r.get(1)? }))?.collect::<Result<_, _>>()?;
        Ok(facts)
    }

    pub fn delete_fact(&self, id: i64) -> Result<bool> {
        Ok(self.db.execute("DELETE FROM facts WHERE id = ?1", [id])? > 0)
    }

    pub fn add_reminder(&self, due: i64, text: &str) -> Result<i64> {
        self.db.execute(
            "INSERT INTO reminders (created, due, text) VALUES (?1, ?2, ?3)",
            params![now(), due, text],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn pending_reminders(&self) -> Result<Vec<Reminder>> {
        let mut stmt = self.db.prepare("SELECT id, due, text FROM reminders WHERE done = 0 ORDER BY due")?;
        let r = stmt
            .query_map([], |r| Ok(Reminder { id: r.get(0)?, due: r.get(1)?, text: r.get(2)? }))?
            .collect::<Result<_, _>>()?;
        Ok(r)
    }

    pub fn next_due(&self) -> Result<Option<i64>> {
        Ok(self
            .db
            .query_row("SELECT MIN(due) FROM reminders WHERE done = 0", [], |r| r.get::<_, Option<i64>>(0))
            .optional()?
            .flatten())
    }

    /// Marca como feitos e devolve os lembretes que já venceram.
    pub fn take_due(&self, at: i64) -> Result<Vec<Reminder>> {
        let due: Vec<Reminder> = {
            let mut stmt =
                self.db.prepare("SELECT id, due, text FROM reminders WHERE done = 0 AND due <= ?1 ORDER BY due")?;
            let rows = stmt.query_map([at], |r| Ok(Reminder { id: r.get(0)?, due: r.get(1)?, text: r.get(2)? }))?;
            rows.collect::<Result<_, _>>()?
        };
        for r in &due {
            self.db.execute("UPDATE reminders SET done = 1 WHERE id = ?1", [r.id])?;
        }
        Ok(due)
    }

    pub fn cancel_reminder(&self, id: i64) -> Result<bool> {
        Ok(self.db.execute("UPDATE reminders SET done = 1 WHERE id = ?1 AND done = 0", [id])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historico_volta_em_ordem() {
        let m = Memory::in_memory().unwrap();
        for i in 0..5 {
            m.add_turn("user", &format!("msg {i}")).unwrap();
        }
        let t = m.recent_turns(2).unwrap();
        assert_eq!(t.iter().map(|t| t.content.as_str()).collect::<Vec<_>>(), ["msg 3", "msg 4"]);
    }

    #[test]
    fn lembretes_vencidos_saem_uma_vez() {
        let m = Memory::in_memory().unwrap();
        m.add_reminder(100, "beber água").unwrap();
        let id = m.add_reminder(200, "reunião").unwrap();
        m.add_reminder(300, "dormir").unwrap();
        assert_eq!(m.next_due().unwrap(), Some(100));
        assert!(m.cancel_reminder(id).unwrap());
        let due = m.take_due(250).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].text, "beber água");
        assert!(m.take_due(250).unwrap().is_empty());
        assert_eq!(m.next_due().unwrap(), Some(300));
    }

    #[test]
    fn fatos() {
        let m = Memory::in_memory().unwrap();
        let id = m.add_fact("Jogo CS2 à noite").unwrap();
        assert_eq!(m.facts().unwrap().len(), 1);
        assert!(m.delete_fact(id).unwrap());
        assert!(m.facts().unwrap().is_empty());
    }
}
