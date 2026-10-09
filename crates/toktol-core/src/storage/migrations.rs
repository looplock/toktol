//! 有序内嵌迁移：[`MIGRATIONS`] 的下标 i 把库从 `user_version = i` 升到 `i + 1`。
//! 已发布版本的 SQL 不可修改，改表在数组末尾追加。
//! 阶段 4 的 `gateway_requests` 走 v2 迁移追加，不在 v1 提前建表。

/// 迁移 SQL 序列：`MIGRATIONS[i]` 把库从版本 i 升到 i+1。
pub(super) const MIGRATIONS: &[&str] = &[
    // v1：阶段 2 的六张表 + 索引 + 总分视图。
    "
    CREATE TABLE scanned_files (
        id            INTEGER PRIMARY KEY,
        path          TEXT    NOT NULL UNIQUE,
        tool          TEXT    NOT NULL,
        content_hash  BLOB    NOT NULL,
        size          INTEGER NOT NULL,
        parsed_bytes  INTEGER NOT NULL,
        missing_since INTEGER,
        updated_at    INTEGER NOT NULL
    );

    CREATE TABLE sessions (
        id              INTEGER PRIMARY KEY,
        tool            TEXT    NOT NULL,
        external_id     TEXT    NOT NULL,
        external_id_is_derived INTEGER NOT NULL DEFAULT 0,
        title           TEXT,
        project_dir     TEXT,
        source_file_id  INTEGER NOT NULL REFERENCES scanned_files(id),
        first_seen_at   INTEGER NOT NULL,
        last_activity_at INTEGER,
        UNIQUE (tool, external_id)
    );

    CREATE TABLE usage_records (
        id                      INTEGER PRIMARY KEY,
        session_id              INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
        tool                    TEXT    NOT NULL,
        model_raw               TEXT    NOT NULL,
        model                   TEXT    NOT NULL,
        ts                      INTEGER NOT NULL,
        input_tokens            INTEGER NOT NULL DEFAULT 0,
        output_tokens           INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens       INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens      INTEGER NOT NULL DEFAULT 0,
        reasoning_tokens        INTEGER,
        input_cost_micros       INTEGER,
        output_cost_micros      INTEGER,
        cache_read_cost_micros  INTEGER,
        cache_write_cost_micros INTEGER,
        dedup_key               BLOB    NOT NULL UNIQUE
    );

    CREATE TABLE models (
        id            TEXT PRIMARY KEY,
        display_name  TEXT NOT NULL,
        first_seen_at INTEGER
    );

    CREATE TABLE model_mappings (
        tool      TEXT NOT NULL,
        raw_model TEXT NOT NULL,
        model_id  TEXT NOT NULL REFERENCES models(id),
        source    TEXT NOT NULL CHECK (source IN ('auto','user')),
        updated_at INTEGER NOT NULL,
        UNIQUE (tool, raw_model)
    );

    CREATE TABLE model_prices (
        model_id TEXT NOT NULL REFERENCES models(id),
        input_per_mtok_micros        INTEGER,
        output_per_mtok_micros       INTEGER,
        cache_read_per_mtok_micros   INTEGER,
        cache_write_per_mtok_micros  INTEGER,
        currency TEXT NOT NULL DEFAULT 'USD',
        PRIMARY KEY (model_id)
    );

    CREATE INDEX idx_usage_ts         ON usage_records (ts);
    CREATE INDEX idx_usage_tool_model ON usage_records (tool, model, ts);
    CREATE INDEX idx_sessions_recent  ON sessions (tool, last_activity_at);

    -- 总分视图：聚合查询的唯一总分口径；任一有量桶未定价 → 该行总分 NULL。
    -- 设计依据与 gateway_requests 的同构视图；总分口径见下方视图定义。
    CREATE VIEW v_usage_cost AS
    SELECT
        r.*,
        CASE WHEN (r.input_tokens > 0 AND r.input_cost_micros IS NULL)
               OR (r.output_tokens + COALESCE(r.reasoning_tokens, 0) > 0
                   AND r.output_cost_micros IS NULL)
               OR (r.cache_read_tokens  > 0 AND r.cache_read_cost_micros  IS NULL)
               OR (r.cache_write_tokens > 0 AND r.cache_write_cost_micros IS NULL)
             THEN NULL
             ELSE COALESCE(r.input_cost_micros, 0) + COALESCE(r.output_cost_micros, 0)
                + COALESCE(r.cache_read_cost_micros, 0)
                + COALESCE(r.cache_write_cost_micros, 0)
        END AS cost_micros
    FROM usage_records AS r;
    ",
    // v2：阶段 4 的网关计量事实表；
    // 总分视图照抄 v_usage_cost 的 CASE 口径（只换 FROM）。
    "
    CREATE TABLE gateway_requests (
        id            INTEGER PRIMARY KEY,
        ts            INTEGER NOT NULL,
        tool          TEXT,
        model_raw     TEXT    NOT NULL,
        model         TEXT    NOT NULL,
        input_tokens  INTEGER NOT NULL DEFAULT 0,
        output_tokens INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        reasoning_tokens   INTEGER,
        input_cost_micros        INTEGER,
        output_cost_micros       INTEGER,
        cache_read_cost_micros   INTEGER,
        cache_write_cost_micros  INTEGER,
        status_code   INTEGER,
        latency_ms    INTEGER,
        upstream      TEXT,
        token_hash    BLOB
    );

    CREATE INDEX idx_gateway_ts    ON gateway_requests (ts);
    CREATE INDEX idx_gateway_model ON gateway_requests (model, ts);

    CREATE VIEW v_gateway_cost AS
    SELECT
        r.*,
        CASE WHEN (r.input_tokens > 0 AND r.input_cost_micros IS NULL)
               OR (r.output_tokens + COALESCE(r.reasoning_tokens, 0) > 0
                   AND r.output_cost_micros IS NULL)
               OR (r.cache_read_tokens  > 0 AND r.cache_read_cost_micros  IS NULL)
               OR (r.cache_write_tokens > 0 AND r.cache_write_cost_micros IS NULL)
             THEN NULL
             ELSE COALESCE(r.input_cost_micros, 0) + COALESCE(r.output_cost_micros, 0)
                + COALESCE(r.cache_read_cost_micros, 0)
                + COALESCE(r.cache_write_cost_micros, 0)
        END AS cost_micros
    FROM gateway_requests AS r;
    ",
    // v3：明细页的请求耗时（毫秒）。可空：日志不带计时的工具与老记录恒 NULL，
    // 前端显示为无数据（–），不参与"0 耗时"的统计口径。
    "
    ALTER TABLE usage_records ADD COLUMN duration_ms INTEGER;
    ",
    // v4：zcode/workbuddy 适配器开始提供项目目录（此前恒 NULL）。只把这两类
    // 来源的游标归零触发重扫：重复入账由 dedup_key 拦住，已存在的会话只补
    // NULL 的 project_dir，其余不动。
    "
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool IN ('zcode', 'workbuddy');
    ",
    // v5：定价页基础。三件事：
    // ① model_mappings 改全局粒度——一个原始变种名只指向一个标准模型，
    //    不再按工具区分（产品决策）。同一 raw 名多工具冲突时 user 映射优先、
    //    其余取最近更新，落表即收敛成一行。
    // ② model_prices 加 source（seed|catalog|user）：models.dev 目录同步只补缺
    //    和刷新 catalog 行，用户改过的价（user）永不覆盖。
    // ③ catalog_entries 存目录快照（models.dev，USD/百万 token → 微美元），
    //    用户价与目录价分离，目录刷新不触碰事实表。
    "
    CREATE TABLE catalog_entries (
        provider     TEXT NOT NULL,
        model_id     TEXT NOT NULL,
        display_name TEXT,
        status       TEXT,
        input_per_mtok_micros        INTEGER,
        output_per_mtok_micros       INTEGER,
        cache_read_per_mtok_micros   INTEGER,
        cache_write_per_mtok_micros  INTEGER,
        reasoning_per_mtok_micros    INTEGER,
        fetched_at   INTEGER NOT NULL,
        PRIMARY KEY (provider, model_id)
    );

    ALTER TABLE model_prices ADD COLUMN source TEXT NOT NULL DEFAULT 'seed';

    CREATE TABLE model_mappings_global (
        raw_model  TEXT PRIMARY KEY,
        model_id   TEXT NOT NULL REFERENCES models(id),
        source     TEXT NOT NULL CHECK (source IN ('auto','user')),
        updated_at INTEGER NOT NULL
    );
    INSERT INTO model_mappings_global (raw_model, model_id, source, updated_at)
    SELECT raw_model, model_id, source, updated_at FROM (
        SELECT raw_model, model_id, source, updated_at,
               ROW_NUMBER() OVER (PARTITION BY raw_model
                                  ORDER BY source = 'user' DESC, updated_at DESC) AS rn
        FROM model_mappings
    ) WHERE rn = 1;
    DROP TABLE model_mappings;
    ALTER TABLE model_mappings_global RENAME TO model_mappings;
    ",
    // v6：映射自动处理。两件事：
    // ① catalog_entries(model_id) 索引——入库 miss 分支要按归一名与启发候选
    //    反查目录，没索引就是每次入库全表扫。
    // ② model_mappings.source 扩 'suggested'：启发式合并（剥日期/修饰尾缀）的
    //    映射与确定性归一（auto）区分开。两者页面都显示"已默认"，但 suggested
    //    是猜出来的，用户确认前不算是稳的。
    "
    CREATE INDEX idx_catalog_model ON catalog_entries (model_id);

    CREATE TABLE model_mappings_v6 (
        raw_model  TEXT PRIMARY KEY,
        model_id   TEXT NOT NULL REFERENCES models(id),
        source     TEXT NOT NULL CHECK (source IN ('auto','suggested','user')),
        updated_at INTEGER NOT NULL
    );
    INSERT INTO model_mappings_v6 (raw_model, model_id, source, updated_at)
    SELECT raw_model, model_id, source, updated_at FROM model_mappings;
    DROP TABLE model_mappings;
    ALTER TABLE model_mappings_v6 RENAME TO model_mappings;
    ",
    // v7：zcode/workbuddy 的标题曾取到 harness 注入的 <system-reminder> 上下文
    // 文本。适配器已改为取工具自身的标题来源（zcode 应用库 session.title、
    // workbuddy 的 ai-title 行与 <user_query> 提问）：污染标题置 NULL，游标归零
    // 触发重扫回填——COALESCE 只补空，不碰本来就正确的标题；重复入账由
    // dedup_key 拦住。
    "
    UPDATE sessions SET title = NULL
     WHERE tool IN ('zcode', 'workbuddy')
       AND (title LIKE '<system-reminder%' OR title LIKE '<teammate-message%');
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool IN ('zcode', 'workbuddy');
    ",
    // v8：claude-code 适配器曾把工具自合成的占位"模型"（`<synthetic>`，出错时的
    // 提示行，usage 全零、不是真实请求）当作模型名入账并自动建映射。适配器已在
    // 源头过滤（模型名以 '<' 开头即拒），这里只清历史脏数据；不做游标归零——
    // 重扫不会再写入这些行，直接删即可。
    "
    DELETE FROM usage_records  WHERE model_raw LIKE '<%';
    DELETE FROM model_mappings WHERE raw_model LIKE '<%';
    DELETE FROM model_prices   WHERE model_id   LIKE '<%';
    DELETE FROM models         WHERE id         LIKE '<%';
    ",
    // v9：zcode/codex 适配器曾把缓存读计两次——日志是 OpenAI 语义（cached ⊆
    // input），但输入桶没有先拆掉缓存读。适配器已修，但 dedup_key 是整行内容
    // 哈希，旧行不删则重扫写不进修正值：删掉两工具的用量记录、游标归零重扫
    // 重建（sessions 行保留，重扫回填）；成本分项按现价重算，属设计内路径。
    "
    DELETE FROM usage_records WHERE tool IN ('zcode', 'codex');
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool IN ('zcode', 'codex');
    ",
    // v10：会话转录的字节区间索引——多 GB 日志的详情页从"全量现读"变为
    // "按索引 seek 读片段"。只存元数据（角色/时间/片段在源文件里的字节区间），
    // 消息正文绝不落库（红线）。状态表管
    // 增量续建（检查点 + diff 基线 + 末轮响应悬挂）与指纹失效判定；两表都以
    // scanned_files.id 为锚，文件行删除时级联清理。
    "
    CREATE TABLE transcript_entries (
        id          INTEGER PRIMARY KEY,
        file_id     INTEGER NOT NULL REFERENCES scanned_files(id) ON DELETE CASCADE,
        seq         INTEGER NOT NULL,
        role        TEXT    NOT NULL,
        ts_ms       INTEGER,
        model       TEXT,
        kind        INTEGER NOT NULL,
        frag_offset INTEGER NOT NULL,
        frag_len    INTEGER NOT NULL,
        skip_blocks INTEGER,
        frag_meta   TEXT
    );

    CREATE INDEX idx_transcript_entries_seq ON transcript_entries (file_id, seq);

    CREATE TABLE transcript_index_state (
        file_id         INTEGER PRIMARY KEY REFERENCES scanned_files(id) ON DELETE CASCADE,
        built_offset    INTEGER NOT NULL,
        size            INTEGER NOT NULL,
        mtime_ms        INTEGER NOT NULL,
        complete        INTEGER NOT NULL,
        baseline_offset INTEGER,
        baseline_len    INTEGER,
        pending_offset  INTEGER,
        pending_len     INTEGER,
        pending_ts      INTEGER,
        pending_model   TEXT
    );
    ",
    // v11：zcode 用量源切换——从 rollout 日志改为应用库 db.sqlite 的 model_usage
    // 表。默认配置下工具滚动清理 rollout 文件（只留最近几个），日志源既不全也
    // 不稳，还记不到 subagent / compact / session_title 请求；应用库的行不受轮
    // 转影响，与工具官方用量页同源。旧行删掉、由库源全量重建（sessions 行保留，
    // 重扫回填）；成本分项按现价重算，属设计内路径（同 v9）。rollout 文件的游
    // 标不必归零：解析层已改为只产会话簿记事实（标题/目录），不再产用量。
    // db 源的缓存指纹一并失效——这是切换的组成部分而非修数据：指纹只含表水位、
    // 不随代码变化，若某次扫描在旧事实源口径下登记过 db 路径，不清指纹的话
    // "指纹未变→跳过"分支会挡住首次重建。对全新用户该行尚不存在，空操作。
    "
    DELETE FROM usage_records WHERE tool = 'zcode';
    UPDATE scanned_files SET content_hash = x'' WHERE tool = 'zcode' AND path LIKE '%db.sqlite';
    ",
    // v12：opencode 的失败/中止请求会在工具库里留下 tokens 全零的 assistant
    // 行，适配器曾照常入账（界面上 0 token、$0.00 的空记录）。适配器已在源头
    // 过滤；这里删掉历史全零行，指纹失效由 v13 补做。全零行成本本为零，删除
    // 不影响任何汇总，无需重算。
    "
    DELETE FROM usage_records
     WHERE tool = 'opencode'
       AND input_tokens = 0 AND output_tokens = 0
       AND cache_read_tokens = 0 AND cache_write_tokens = 0
       AND COALESCE(reasoning_tokens, 0) = 0;
    ",
    // v13：v12 删掉 opencode 全零行时漏了失效库源指纹——指纹只含表水位，源库
    // 不变就永远不走重扫分支，被删的行无法用源库当前值重建（本地实测踩坑：
    // 会话详情缺行，直到源库因继续使用而变化才自愈）。置空强制下次全量重读；
    // 未删的行由 dedup_key 拦住，只有被删行会以终值重建。
    "
    UPDATE scanned_files SET content_hash = x'' WHERE tool = 'opencode';
    ",
    // v14：zcode 转录解析改为真实日志形态（request.messages + messageOffset
    // 压缩块），索引状态里 baseline 列的语义从"基线数组字节区间"换成"回放
    // 锚点行"，旧行无法判别版本，一并清掉——索引按需重建（用户点开详情）。
    "
    DELETE FROM transcript_entries;
    DELETE FROM transcript_index_state;
    ",
    // v15：内置种子价下线（models.dev 目录全覆盖，硬编码表只会过期）。旧库里的
    // `source='seed'` 行改标 `catalog`：价格数值先保留（历史成本口径不突变），
    // 下次目录同步即按 catalog 语义刷新成 models.dev 现价；user 行不动。
    "
    UPDATE model_prices SET source = 'catalog' WHERE source = 'seed';
    ",
    // v16：codebuddy 的纯文本扫描曾把统计行行尾的右括号吃进模型名
    // （`…, modelId=hy4-preview-f)` → raw `hy4-preview-f)`）。解析终止符已补
    // `)`，但 dedup 按行拦住重扫，存量脏 raw 无法自愈，这里只清已实锤的形态：
    // 尾部一个 `)` 且清完非空。映射侧净名同指的脏行删除、净名空缺的就地改名、
    // 指向分歧的保守保留（真实数据不存在，只为 PK 安全兜底）；事实表的 model
    // 列不动——它由入库时映射解析而来，保留即计费口径零变化、无需重算。
    "
    DELETE FROM model_mappings
     WHERE length(raw_model) > 1
       AND raw_model LIKE '%)'
       AND EXISTS (SELECT 1 FROM model_mappings AS clean
                   WHERE clean.raw_model = substr(model_mappings.raw_model, 1,
                                                  length(model_mappings.raw_model) - 1)
                     AND clean.model_id = model_mappings.model_id);

    UPDATE model_mappings
       SET raw_model = substr(raw_model, 1, length(raw_model) - 1)
     WHERE length(raw_model) > 1
       AND raw_model LIKE '%)'
       AND substr(raw_model, 1, length(raw_model) - 1)
           NOT IN (SELECT raw_model FROM model_mappings);

    UPDATE usage_records
       SET model_raw = substr(model_raw, 1, length(model_raw) - 1)
     WHERE length(model_raw) > 1
       AND model_raw LIKE '%)';

    UPDATE gateway_requests
       SET model_raw = substr(model_raw, 1, length(model_raw) - 1)
     WHERE length(model_raw) > 1
       AND model_raw LIKE '%)';

    DELETE FROM models
     WHERE id NOT IN (SELECT model_id FROM model_mappings)
       AND id NOT IN (SELECT model_id FROM model_prices)
       AND id NOT IN (SELECT model FROM usage_records)
       AND id NOT IN (SELECT model FROM gateway_requests);
    ",
    // v17：workbuddy 适配器开始推导请求耗时（日志不带原生计时，口径为 usage 行
    // 时间戳 − 最近边界事件时间戳，见适配器模块文档）。dedup_key 是整行内容哈希，
    // 旧行不删则重扫写不进耗时：删掉 workbuddy 的用量记录、游标归零重扫重建
    // （sessions 行保留，重扫回填；成本分项按现价重算，属设计内路径，同 v9）。
    "
    DELETE FROM usage_records WHERE tool = 'workbuddy';
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool = 'workbuddy';
    ",
    // v18：已删会话的墓碑。共享日志（codebuddy 扩展日志等）轮转/重写时整文件
    // 重解析，get_or_create_session 在 dedup 之前跑——没有墓碑，已删会话会以
    // "0 用量、无标题"的空壳复活。扫描层建行前查此表。IF NOT EXISTS：v13 起的
    // 迁移测试靠"回拨版本重跑后段"验证，后段迁移必须可重入。
    "
    CREATE TABLE IF NOT EXISTS deleted_sessions (
        tool        TEXT NOT NULL,
        external_id TEXT NOT NULL,
        deleted_at  INTEGER NOT NULL,
        PRIMARY KEY (tool, external_id)
    );
    ",
    // v19：请求耗时回填。claude-code 适配器改为 message.id 分组的双时间戳推导
    // （新版工具日志已停写 assistant 行的顶层 durationMs，存量 100% NULL）；
    // codebuddy 的 metrics 配对算法此前修正，存量 NULL 同样被 dedup_key 锁死
    // ——旧行不删则重扫写不进新值（同 v9/v17）：删掉两工具在册会话的用量行、
    // 游标归零重扫重建。session_id IS NOT NULL：孤儿行是"删除会话统计保留"的
    // 载体（v18 墓碑会拦住其重建，删了即真丢），必须留在库中。
    "
    DELETE FROM usage_records
     WHERE tool IN ('claude-code', 'codebuddy') AND session_id IS NOT NULL;
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool IN ('claude-code', 'codebuddy');
    ",
    // v20：grok 适配器开始从会话目录名解码项目目录（URL 编码的 cwd，见适配
    // 器文档）。游标归零触发重扫回填：重复入账由 dedup_key 拦住，已存在的
    // 会话只补 NULL 的 project_dir（同 v4 口径）。
    "
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool = 'grok';
    ",
    // v21：pi/codex 适配器开始推导请求耗时（pi：外层行时间戳 − 内层
    // message.timestamp 请求发起时刻；codex：请求边界触发行 → token_usage_record
    // 收尾行）。旧行不删则重扫写不进新值（同 v9/v17/v19）：删掉两工具在册会话的
    // 用量行、游标归零重扫重建。session_id IS NOT NULL：孤儿行是"删除会话统计
    // 保留"的载体，必须留下（同 v19）。
    "
    DELETE FROM usage_records WHERE tool IN ('pi', 'codex') AND session_id IS NOT NULL;
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool IN ('pi', 'codex');
    ",
    // v22：请求数落到行级（request_count）。grok 一次轮次拆成多条请求级行，
    // 每行代表一次真实网络请求（token 按比例分摊）；其余工具恒 1。ALTER TABLE
    // 无法重入（v13 起的迁移测试回拨版本重跑后段，见 v18 注），故整表重建：
    // 建带新列的副本 → 按列名拷贝（存量一律 1）→ 换名 → 重建索引与视图。
    // v_usage_cost 引用本表，必须先 DROP（悬空视图会让换名的 schema 重解析
    // 报 no such table），换名后按 v1 原文重建。重入安全：换名后表名复原，
    // 再次执行时两侧列名依然对齐；重跑会把存量 request_count 重置为 1，真实
    // 库版本单调不会发生，测试数据全部是默认 1 无损。
    // grok 行重建：一轮多行的拆分改变了行的形状，旧行不删则重扫写不进新形状
    // （同 v9/v17/v19/v21）：删掉 grok 在册会话的用量行、游标归零重扫重建。
    // session_id IS NOT NULL：孤儿行是"删除会话统计保留"的载体，必须留下
    // （同 v19/v21）。
    "
    CREATE TABLE usage_records_v22 (
        id                      INTEGER PRIMARY KEY,
        session_id              INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
        tool                    TEXT    NOT NULL,
        model_raw               TEXT    NOT NULL,
        model                   TEXT    NOT NULL,
        ts                      INTEGER NOT NULL,
        input_tokens            INTEGER NOT NULL DEFAULT 0,
        output_tokens           INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens       INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens      INTEGER NOT NULL DEFAULT 0,
        reasoning_tokens        INTEGER,
        input_cost_micros       INTEGER,
        output_cost_micros      INTEGER,
        cache_read_cost_micros  INTEGER,
        cache_write_cost_micros INTEGER,
        dedup_key               BLOB    NOT NULL UNIQUE,
        duration_ms             INTEGER,
        request_count           INTEGER NOT NULL DEFAULT 1
    );

    INSERT INTO usage_records_v22
        SELECT r.id, r.session_id, r.tool, r.model_raw, r.model, r.ts,
               r.input_tokens, r.output_tokens, r.cache_read_tokens,
               r.cache_write_tokens, r.reasoning_tokens, r.input_cost_micros,
               r.output_cost_micros, r.cache_read_cost_micros,
               r.cache_write_cost_micros, r.dedup_key, r.duration_ms, 1
        FROM usage_records AS r;

    DROP VIEW IF EXISTS v_usage_cost;
    DROP TABLE usage_records;
    ALTER TABLE usage_records_v22 RENAME TO usage_records;

    CREATE INDEX idx_usage_ts         ON usage_records (ts);
    CREATE INDEX idx_usage_tool_model ON usage_records (tool, model, ts);

    CREATE VIEW v_usage_cost AS
    SELECT
        r.*,
        CASE WHEN (r.input_tokens > 0 AND r.input_cost_micros IS NULL)
               OR (r.output_tokens + COALESCE(r.reasoning_tokens, 0) > 0
                   AND r.output_cost_micros IS NULL)
               OR (r.cache_read_tokens  > 0 AND r.cache_read_cost_micros  IS NULL)
               OR (r.cache_write_tokens > 0 AND r.cache_write_cost_micros IS NULL)
             THEN NULL
             ELSE COALESCE(r.input_cost_micros, 0) + COALESCE(r.output_cost_micros, 0)
                + COALESCE(r.cache_read_cost_micros, 0)
                + COALESCE(r.cache_write_cost_micros, 0)
        END AS cost_micros
    FROM usage_records AS r;

    DELETE FROM usage_records WHERE tool = 'grok' AND session_id IS NOT NULL;
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool = 'grok';
    ",
    // v23：grok 请求级行回填请求耗时。拆分（v22）当时未推导耗时，现改为从
    // updates.jsonl 的事件锚点推导（工具执行时间剔除的真实区间）。dedup 键
    // 不变，旧行被锁死：删掉 grok 在册会话的用量行、游标归零重扫重建
    // （同 v22）。session_id IS NOT NULL：孤儿行是"删除会话统计保留"的载体，
    // 必须留下（同 v19/v21/v22）。可重入：幂等 DELETE + 游标归零。
    "
    DELETE FROM usage_records WHERE tool = 'grok' AND session_id IS NOT NULL;
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool = 'grok';
    ",
    // v24：workbuddy 子智能体日志（projects/<编码项目>/<会话id>/subagents/
    // agent-*.jsonl）此前解码不出项目目录——适配器只取直接父目录名，对子代理
    // 日志拿到的是 `subagents`。适配器已改为沿父目录向上找编码项目目录；游标
    // 归零触发重扫回填：重复入账由 dedup_key 拦住，已存在的会话只补 NULL 的
    // project_dir（同 v4 口径）。
    "
    UPDATE scanned_files SET parsed_bytes = 0 WHERE tool = 'workbuddy';
    ",
];
