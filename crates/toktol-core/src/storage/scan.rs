//! 扫描簿记与转录索引：`scanned_files` 增量游标、转录条目存储与索引推进。
//! 写入只发生在扫描与转录重建路径上，读侧供明细页与会话页分页。

use rusqlite::{OptionalExtension, params};

use super::*;

impl Storage {
    /// 登记一个扫描到的日志文件；已存在（按规范化路径）则更新簿记并清除"消失"标记。
    ///
    /// 返回该文件的行 id。`parsed_bytes` 是增量游标——文件被轮转/重写后由扫描层
    /// 重置为 0 并整体重解析，去重交给 [`Storage::insert_usage_record`]。
    pub fn upsert_scanned_file(
        &self,
        path: &str,
        tool: &str,
        content_hash: &[u8],
        size: i64,
        parsed_bytes: i64,
        now: i64,
    ) -> Result<i64> {
        self.conn.query_row(
            "INSERT INTO scanned_files (path, tool, content_hash, size, parsed_bytes, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                 tool = excluded.tool,
                 content_hash = excluded.content_hash,
                 size = excluded.size,
                 parsed_bytes = excluded.parsed_bytes,
                 missing_since = NULL,
                 updated_at = excluded.updated_at
             RETURNING id",
            params![path, tool, content_hash, size, parsed_bytes, now],
            |row| row.get(0),
        )
        .map_err(|err| err.into())
    }

    /// 标记（或清除）文件消失。扫描层发现文件不存在时置时间戳；**用量数据保留
    /// 不动**——用户自行删日志是真实发生的历史，与"经 Toktol 删除会话"是两条
    /// 语义。
    ///
    /// 置 missing 的同时清掉该文件的转录索引：索引条目是源文件里的**字节区间**，
    /// 源没了就永远无法解引用，而转录读取路径在解引用之前就会报
    /// `core.source_gone`，留着只是死重。索引是纯缓存：文件若失而复得，状态行
    /// 已不在，读取路径自然按未建站从头重建。对没建过索引的文件是空操作。
    pub fn set_file_missing(&self, file_id: i64, missing_since: Option<i64>) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE scanned_files SET missing_since = ?2 WHERE id = ?1",
            params![file_id, missing_since],
        )?;
        if missing_since.is_some() {
            tx.execute(
                "DELETE FROM transcript_entries WHERE file_id = ?1",
                params![file_id],
            )?;
            tx.execute(
                "DELETE FROM transcript_index_state WHERE file_id = ?1",
                params![file_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 读取扫描簿记（游标与内容指纹）；从未见过该文件则 `None`。
    pub fn scanned_file_state(&self, path: &str) -> Result<Option<ScannedFileState>> {
        self.conn
            .query_row(
                "SELECT id, parsed_bytes, content_hash FROM scanned_files WHERE path = ?1",
                params![path],
                |row| {
                    Ok(ScannedFileState {
                        id: row.get(0)?,
                        parsed_bytes: row.get(1)?,
                        content_hash: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 某工具全部已登记文件的 (id, 路径)，供扫描层做"本轮没见到 → 标记消失"的对账。
    pub fn scanned_file_paths(&self, tool: &str) -> Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path FROM scanned_files WHERE tool = ?1")?;
        let rows = stmt
            .query_map(params![tool], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// 转录索引的建站状态；从未建过则 `None`。
    pub fn transcript_index_state(&self, file_id: i64) -> Result<Option<TranscriptIndexState>> {
        self.conn
            .query_row(
                "SELECT built_offset, size, mtime_ms, complete,
                        baseline_offset, baseline_len,
                        pending_offset, pending_len, pending_ts, pending_model
                 FROM transcript_index_state WHERE file_id = ?1",
                params![file_id],
                |row| {
                    Ok(TranscriptIndexState {
                        built_offset: row.get(0)?,
                        size: row.get(1)?,
                        mtime_ms: row.get(2)?,
                        complete: row.get::<_, i64>(3)? != 0,
                        baseline_offset: row.get(4)?,
                        baseline_len: row.get(5)?,
                        pending_offset: row.get(6)?,
                        pending_len: row.get(7)?,
                        pending_ts: row.get(8)?,
                        pending_model: row.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// 清空某源文件的转录索引（全新建站 / 指纹失效重建前调用）。
    pub fn reset_transcript_index(&self, file_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transcript_entries WHERE file_id = ?1",
            params![file_id],
        )?;
        self.conn.execute(
            "DELETE FROM transcript_index_state WHERE file_id = ?1",
            params![file_id],
        )?;
        Ok(())
    }

    /// 撤掉一行已提交的转录条目（增量续建时撤销"建完时物化的末轮响应"用，
    /// 见 zcode index 的 append 续建）。
    pub fn delete_transcript_entry(&self, file_id: i64, seq: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transcript_entries WHERE file_id = ?1 AND seq = ?2",
            params![file_id, seq],
        )?;
        Ok(())
    }

    /// 提交一次建站检查点：新增条目行 + 建站状态，单事务落库。
    /// 已提交的行保留（续建从 built_offset 继续），条目 seq 必须全局递增。
    pub fn commit_transcript_index(
        &self,
        file_id: i64,
        entries: &[NewTranscriptEntry],
        state: &TranscriptIndexState,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO transcript_entries
                     (file_id, seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for entry in entries {
                stmt.execute(params![
                    file_id,
                    entry.seq,
                    entry.role,
                    entry.ts_ms,
                    entry.model,
                    entry.kind,
                    entry.frag_offset,
                    entry.frag_len,
                    entry.skip_blocks,
                    entry.frag_meta,
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO transcript_index_state
                 (file_id, built_offset, size, mtime_ms, complete,
                  baseline_offset, baseline_len, pending_offset, pending_len,
                  pending_ts, pending_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(file_id) DO UPDATE SET
                 built_offset = excluded.built_offset,
                 size = excluded.size,
                 mtime_ms = excluded.mtime_ms,
                 complete = excluded.complete,
                 baseline_offset = excluded.baseline_offset,
                 baseline_len = excluded.baseline_len,
                 pending_offset = excluded.pending_offset,
                 pending_len = excluded.pending_len,
                 pending_ts = excluded.pending_ts,
                 pending_model = excluded.pending_model",
            params![
                file_id,
                state.built_offset,
                state.size,
                state.mtime_ms,
                state.complete as i64,
                state.baseline_offset,
                state.baseline_len,
                state.pending_offset,
                state.pending_len,
                state.pending_ts,
                state.pending_model,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 按序取一页转录条目：`seq > after_seq` 的前 `limit` 行（升序）。
    pub fn transcript_entries_page(
        &self,
        file_id: i64,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<TranscriptEntryRow>> {
        self.transcript_entries_page_impl(file_id, after_seq, limit, false)
    }

    /// 按序取一页转录条目：`seq < before_seq` 的**末** `limit` 行（升序返回），
    /// 供窗口向前加载。
    pub fn transcript_entries_before(
        &self,
        file_id: i64,
        before_seq: i64,
        limit: i64,
    ) -> Result<Vec<TranscriptEntryRow>> {
        self.transcript_entries_page_impl(file_id, before_seq, limit, true)
    }

    fn transcript_entries_page_impl(
        &self,
        file_id: i64,
        anchor: i64,
        limit: i64,
        before: bool,
    ) -> Result<Vec<TranscriptEntryRow>> {
        let sql = if before {
            "SELECT seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta
             FROM transcript_entries WHERE file_id = ?1 AND seq < ?2
             ORDER BY seq DESC LIMIT ?3"
        } else {
            "SELECT seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta
             FROM transcript_entries WHERE file_id = ?1 AND seq > ?2
             ORDER BY seq ASC LIMIT ?3"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt
            .query_map(params![file_id, anchor, limit], |row| {
                Ok(TranscriptEntryRow {
                    seq: row.get(0)?,
                    role: row.get(1)?,
                    ts_ms: row.get(2)?,
                    model: row.get(3)?,
                    kind: row.get(4)?,
                    frag_offset: row.get(5)?,
                    frag_len: row.get(6)?,
                    skip_blocks: row.get(7)?,
                    frag_meta: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if before {
            rows.reverse();
        }
        Ok(rows)
    }

    /// 转录条目总数（seq 可能因读取时空块过滤出现空洞，总数以行数为准）。
    pub fn transcript_entry_count(&self, file_id: i64) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM transcript_entries WHERE file_id = ?1",
                params![file_id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// 目录（轮次列表）的候选行：user 角色的消息片段（压缩摘要行的判定
    /// 需要解析片段内容，在调用方做）。
    pub fn transcript_turn_candidates(&self, file_id: i64) -> Result<Vec<TranscriptEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, role, ts_ms, model, kind, frag_offset, frag_len, skip_blocks, frag_meta
             FROM transcript_entries WHERE file_id = ?1 AND role = 'user' AND kind = 0
             ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![file_id], |row| {
                Ok(TranscriptEntryRow {
                    seq: row.get(0)?,
                    role: row.get(1)?,
                    ts_ms: row.get(2)?,
                    model: row.get(3)?,
                    kind: row.get(4)?,
                    frag_offset: row.get(5)?,
                    frag_len: row.get(6)?,
                    skip_blocks: row.get(7)?,
                    frag_meta: row.get(8)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
