use rusqlite::{params, Connection};

use crate::error::Result;

#[derive(Debug, Clone)]
pub struct Event<'a> {
    pub project_root: Option<&'a str>,
    pub layer: &'a str,
    pub feature: &'a str,
    pub agent_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub command_pattern: Option<&'a str>,
    pub original_tokens: i64,
    pub compressed_tokens: i64,
    pub exec_time_ms: Option<i64>,
    pub quality_preserved: bool,
    pub detail: Option<&'a str>,
}

impl<'a> Event<'a> {
    pub fn savings(&self) -> i64 {
        self.original_tokens - self.compressed_tokens
    }
}

pub fn record(conn: &Connection, e: &Event<'_>) -> Result<i64> {
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        r#"INSERT INTO telemetry
           (project_root, layer, feature, agent_id, session_id, command_pattern,
            original_tokens, compressed_tokens, savings, exec_time_ms,
            quality_preserved, detail, created_at_epoch)
           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        params![
            e.project_root,
            e.layer,
            e.feature,
            e.agent_id,
            e.session_id,
            e.command_pattern,
            e.original_tokens,
            e.compressed_tokens,
            e.savings(),
            e.exec_time_ms,
            if e.quality_preserved { 1 } else { 0 },
            e.detail,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

#[derive(Debug, Clone)]
pub struct LayerStat {
    pub layer: String,
    pub events: i64,
    pub original_tokens: i64,
    pub compressed_tokens: i64,
    pub savings: i64,
}

pub fn stats_by_layer(conn: &Connection, project_root: Option<&str>) -> Result<Vec<LayerStat>> {
    let mut sql = String::from(
        r#"SELECT layer,
                  COUNT(*) AS events,
                  COALESCE(SUM(original_tokens), 0)   AS original_tokens,
                  COALESCE(SUM(compressed_tokens), 0) AS compressed_tokens,
                  COALESCE(SUM(savings), 0)           AS savings
           FROM telemetry"#,
    );
    if project_root.is_some() {
        sql.push_str(" WHERE project_root = ?");
    }
    sql.push_str(" GROUP BY layer ORDER BY layer");

    let mut stmt = conn.prepare(&sql)?;
    let rows = if let Some(pr) = project_root {
        stmt.query_map(params![pr], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        stmt.query_map([], map_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    Ok(rows)
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LayerStat> {
    Ok(LayerStat {
        layer: row.get(0)?,
        events: row.get(1)?,
        original_tokens: row.get(2)?,
        compressed_tokens: row.get(3)?,
        savings: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_aggregate() {
        let conn = crate::db::open_in_memory().unwrap();
        record(
            &conn,
            &Event {
                project_root: Some("/tmp/proj"),
                layer: "l4",
                feature: "read_cache",
                agent_id: None,
                session_id: None,
                command_pattern: None,
                original_tokens: 1000,
                compressed_tokens: 200,
                exec_time_ms: Some(3),
                quality_preserved: true,
                detail: None,
            },
        )
        .unwrap();

        let stats = stats_by_layer(&conn, Some("/tmp/proj")).unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].layer, "l4");
        assert_eq!(stats[0].events, 1);
        assert_eq!(stats[0].savings, 800);
    }
}
