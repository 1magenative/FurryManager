use rusqlite::Connection;
use std::path::Path;

/// Открывает (или создаёт) базу библиотеки и применяет схему.
pub fn open(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    let _ = conn.pragma_update(None, "journal_mode", "WAL");
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Ждём до 5с при параллельной записи (например, из watcher-потока), а не падаем сразу.
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS files (
            id          INTEGER PRIMARY KEY,
            sha256      TEXT UNIQUE NOT NULL,
            phash       INTEGER,            -- dHash 64 бита (NULL для видео)
            ext         TEXT NOT NULL,
            size        INTEGER NOT NULL DEFAULT 0,
            width       INTEGER NOT NULL DEFAULT 0,
            height      INTEGER NOT NULL DEFAULT 0,
            kind        TEXT NOT NULL,      -- image | animation | video
            rel_path    TEXT NOT NULL,      -- store/ab/<sha256>.<ext>
            thumb_rel   TEXT,               -- thumbs/ab/<sha256>.jpg (NULL если нет)
            imported_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sources (
            id      INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            url     TEXT,
            site    TEXT,
            artist  TEXT,
            title   TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_sources_file ON sources(file_id);

        CREATE TABLE IF NOT EXISTS tags (
            id        INTEGER PRIMARY KEY,
            namespace TEXT NOT NULL DEFAULT '',
            name      TEXT NOT NULL,
            UNIQUE(namespace, name)
        );
        CREATE INDEX IF NOT EXISTS idx_tags_name ON tags(name);

        CREATE TABLE IF NOT EXISTS file_tags (
            file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            tag_id  INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            origin  TEXT NOT NULL DEFAULT 'manual',
            PRIMARY KEY (file_id, tag_id)
        );
        CREATE INDEX IF NOT EXISTS idx_file_tags_tag ON file_tags(tag_id);

        CREATE TABLE IF NOT EXISTS collections (
            id         INTEGER PRIMARY KEY,
            name       TEXT UNIQUE NOT NULL,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS collection_files (
            collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
            file_id       INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            added_at      INTEGER NOT NULL,
            PRIMARY KEY (collection_id, file_id)
        );
        CREATE INDEX IF NOT EXISTS idx_cf_file ON collection_files(file_id);

        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT
        );

        CREATE TABLE IF NOT EXISTS clip_vectors (
            file_id   INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
            embedding BLOB NOT NULL  -- f32 LE; пустой BLOB = не удалось обработать
        );

        -- «Надгробия»: хеши удалённых артов. Watcher их не реимпортирует
        -- (файл может по-прежнему лежать в отслеживаемой папке).
        CREATE TABLE IF NOT EXISTS deleted_hashes (
            sha256     TEXT PRIMARY KEY,
            deleted_at INTEGER NOT NULL
        );

        -- Группы артов: комиксы (упорядоченные страницы) и вариации одного арта.
        CREATE TABLE IF NOT EXISTS groups (
            id         INTEGER PRIMARY KEY,
            name       TEXT NOT NULL DEFAULT '',
            kind       TEXT NOT NULL DEFAULT 'variants', -- comic | variants
            created_at INTEGER NOT NULL
        );

        -- Связи тегов: implies (parent подразумевает child) и alias (синонимы).
        CREATE TABLE IF NOT EXISTS tag_relations (
            id        INTEGER PRIMARY KEY,
            parent_id INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            child_id  INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            kind      TEXT NOT NULL,   -- implies | alias
            UNIQUE(parent_id, child_id, kind)
        );
        "#,
    )?;
    // Миграции для существующих баз (ошибки «колонка уже есть» игнорируем).
    let _ = conn.execute("ALTER TABLE file_tags ADD COLUMN confidence REAL", []);
    // nsfw: NULL = авто по rating-тегам, 0 = принудительно SFW, 1 = принудительно NSFW
    let _ = conn.execute("ALTER TABLE files ADD COLUMN nsfw INTEGER", []);
    // порядок коллекций в панели
    let _ = conn.execute("ALTER TABLE collections ADD COLUMN position INTEGER", []);
    // способ хранения файла: 'store' (копия в библиотеке) | 'inplace' (оригинал на месте)
    let _ = conn.execute("ALTER TABLE files ADD COLUMN storage TEXT DEFAULT 'store'", []);
    // история просмотров
    let _ = conn.execute("ALTER TABLE files ADD COLUMN viewed_at INTEGER", []);
    let _ = conn.execute("ALTER TABLE files ADD COLUMN view_count INTEGER DEFAULT 0", []);
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_files_viewed ON files(viewed_at)", []);
    // видео прошло мультикадровую индексацию (3 кадра), а не по одному превью
    let _ = conn.execute("ALTER TABLE files ADD COLUMN vframes INTEGER", []);
    // членство в группе (комикс/вариации) и позиция внутри неё
    let _ = conn.execute("ALTER TABLE files ADD COLUMN group_id INTEGER", []);
    let _ = conn.execute("ALTER TABLE files ADD COLUMN group_pos INTEGER", []);
    let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_files_group ON files(group_id)", []);
    // Одноразовый сброс счётчика дубликатов: watcher-пересканы раздували его
    // мусорными значениями до фикса.
    let already: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'dupes_reset_v1'", [], |r| r.get(0))
        .ok();
    if already.is_none() {
        let _ = conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES('dupes_skipped', '0')", []);
        let _ = conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES('dupes_reset_v1', '1')", []);
    }
    Ok(())
}
