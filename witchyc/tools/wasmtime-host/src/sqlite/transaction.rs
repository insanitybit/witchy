use libsqlite3_sys as sql;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr::{NonNull, null_mut};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
#[cfg(test)]
use std::time::Duration;

use super::{Grant, MAX_BLOB};
use super::grant::{Rights, check_files, name};

mod schema;

#[derive(Clone, Copy)]
enum Kind { Exec, Change, Expect, Result }

struct Step<'a> { kind: Kind, label: &'a str, sql: &'a str }

fn steps(plan: &[u8], provision: bool) -> Option<Vec<Step<'_>>> {
    let text = std::str::from_utf8(plan).ok()?;
    let mut result = Vec::new();
    for line in text.lines() {
        if result.len() == 16 || result.last().is_some_and(|last: &Step<'_>| matches!(last.kind, Kind::Result)) { return None; }
        let mut fields = line.splitn(3, '\t');
        let kind = match fields.next()? {
            "exec" => Kind::Exec, "change" => Kind::Change,
            "expect" => Kind::Expect, "result" => Kind::Result, _ => return None,
        };
        if provision && !matches!(kind, Kind::Exec) { return None; }
        let label = fields.next()?;
        let sql = fields.next()?;
        if label.len() > 160 || !label.bytes().all(|b| (32..127).contains(&b))
            || matches!(kind, Kind::Exec) != label.is_empty() || sql.is_empty() || sql.len() > 2048
            || sql.contains('\0')
        {
            return None;
        }
        result.push(Step { kind, label, sql });
    }
    let last = result.last()?;
    (provision || matches!(last.kind, Kind::Result)).then_some(result)
}

enum Value<'a> { Text(&'a str), Integer(i64) }

fn values(bindings: &[u8]) -> Option<BTreeMap<&str, Value<'_>>> {
    let mut values = BTreeMap::new();
    for line in std::str::from_utf8(bindings).ok()?.lines() {
        let mut fields = line.splitn(3, '\t');
        let key = fields.next()?;
        let kind = fields.next()?;
        let raw = fields.next()?;
        if !name(key) || matches!(key, "blob" | "now") || values.len() == 32 { return None; }
        let value = match kind {
            "text" => Value::Text(raw),
            "int" => Value::Integer(raw.parse().ok()?),
            _ => return None,
        };
        if values.insert(key, value).is_some() { return None; }
    }
    Some(values)
}

struct Db(NonNull<sql::sqlite3>);
impl Drop for Db {
    fn drop(&mut self) { unsafe { sql::sqlite3_close(self.0.as_ptr()) }; }
}

struct Statement(NonNull<sql::sqlite3_stmt>);
impl Drop for Statement {
    fn drop(&mut self) { unsafe { sql::sqlite3_finalize(self.0.as_ptr()) }; }
}

unsafe extern "C" fn authorize(
    context: *mut c_void, action: c_int, target: *const c_char, detail: *const c_char,
    database: *const c_char, source: *const c_char,
) -> c_int {
    if !source.is_null() { return sql::SQLITE_DENY; }
    if action == sql::SQLITE_SELECT { return sql::SQLITE_OK; }
    // SQLite supplies optional nul-terminated strings for this callback.
    let same = |p: *const c_char, expected: &[u8]| !p.is_null() && unsafe { CStr::from_ptr(p).to_bytes() == expected };
    if action == sql::SQLITE_FUNCTION && [b"json_extract".as_slice(), b"json_object", b"length", b"typeof"].iter().any(|n| same(detail, n)) {
        return sql::SQLITE_OK;
    }
    if !same(database, b"main") || target.is_null() { return sql::SQLITE_DENY; }
    let Ok(table) = unsafe { CStr::from_ptr(target) }.to_str() else { return sql::SQLITE_DENY; };
    let rights = unsafe { &*context.cast::<Rights>() };
    let allowed = match action {
        sql::SQLITE_READ => rights.read.contains(table),
        sql::SQLITE_INSERT => rights.insert.contains(table),
        sql::SQLITE_DELETE => rights.delete.contains(table),
        _ => false,
    };
    if allowed { sql::SQLITE_OK } else { sql::SQLITE_DENY }
}

unsafe extern "C" fn progress(context: *mut c_void) -> c_int {
    i32::from(Instant::now() >= unsafe { *context.cast::<Instant>() })
}

fn exec(db: &Db, query: &CStr) -> Option<()> {
    (unsafe { sql::sqlite3_exec(db.0.as_ptr(), query.as_ptr(), None, null_mut(), null_mut()) } == sql::SQLITE_OK).then_some(())
}

fn connect(grant: &Grant, deadline: Instant) -> Option<Db> {
    let path = &grant.path;
    check_files(Path::new(std::ffi::OsStr::from_bytes(path.to_bytes())), &grant.secret_objects)?;
    let readonly = grant.rights.read_only();
    let mode = if readonly { sql::SQLITE_OPEN_READONLY } else { sql::SQLITE_OPEN_READWRITE };
    let mut raw = null_mut();
    let status = unsafe { sql::sqlite3_open_v2(path.as_ptr(), &mut raw,
        mode | sql::SQLITE_OPEN_NOFOLLOW | sql::SQLITE_OPEN_PRIVATECACHE, null_mut()) };
    let db = Db(NonNull::new(raw)?);
    if status != sql::SQLITE_OK { return None; }
    let left = deadline.checked_duration_since(Instant::now())?.as_millis().min(5000) as i32;
    unsafe {
        if sql::sqlite3_db_config(raw, sql::SQLITE_DBCONFIG_DEFENSIVE, 1_i32, null_mut::<i32>()) != sql::SQLITE_OK {
            return None;
        }
        sql::sqlite3_busy_timeout(raw, left);
        sql::sqlite3_limit(raw, sql::SQLITE_LIMIT_ATTACHED, 0);
        sql::sqlite3_limit(raw, sql::SQLITE_LIMIT_VARIABLE_NUMBER, 32);
        sql::sqlite3_limit(raw, sql::SQLITE_LIMIT_LENGTH, (MAX_BLOB + 65536).max(grant.reply_bytes) as i32);
    }
    exec(&db, c"PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY")?;
    exec(&db, if readonly { c"BEGIN" } else { c"BEGIN IMMEDIATE" })?;
    Some(db)
}

fn prepare(db: &Db, query: &str) -> Option<Statement> {
    let query = CString::new(query).ok()?;
    let mut raw = null_mut();
    let mut tail = std::ptr::null();
    let status = unsafe { sql::sqlite3_prepare_v2(db.0.as_ptr(), query.as_ptr(), -1, &mut raw, &mut tail) };
    let statement = Statement(NonNull::new(raw)?);
    if status != sql::SQLITE_OK || tail.is_null() || !unsafe { CStr::from_ptr(tail) }.to_bytes().iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    Some(statement)
}

fn bind(statement: &Statement, bindings: &BTreeMap<&str, Value<'_>>, blob: &[u8], now: i64) -> Option<()> {
    let raw = statement.0.as_ptr();
    // SQLITE_TRANSIENT copies a guest value before any statement runs.
    unsafe {
        for i in 1..=sql::sqlite3_bind_parameter_count(raw) {
            let param = sql::sqlite3_bind_parameter_name(raw, i);
            if param.is_null() { return None; }
            let key = CStr::from_ptr(param).to_str().ok()?.strip_prefix(':')?;
            let status = match key {
                "now" => sql::sqlite3_bind_int64(raw, i, now),
                "blob" => sql::sqlite3_bind_blob(raw, i, blob.as_ptr().cast(), blob.len() as i32, sql::SQLITE_TRANSIENT()),
                _ => match bindings.get(key)? {
                    Value::Integer(value) => sql::sqlite3_bind_int64(raw, i, *value),
                    Value::Text(value) => sql::sqlite3_bind_text(raw, i, value.as_ptr().cast(), value.len() as i32, sql::SQLITE_TRANSIENT()),
                },
            };
            if status != sql::SQLITE_OK { return None; }
        }
    }
    Some(())
}

fn run_steps(db: &Db, steps: &[Step<'_>], values: &BTreeMap<&str, Value<'_>>, blob: &[u8], capacity: usize) -> Option<Vec<u8>> {
    let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    for step in steps {
        let prepared = prepare(db, step.sql)?;
        bind(&prepared, values, blob, now)?;
        let raw = prepared.0.as_ptr();
        let count = unsafe { sql::sqlite3_column_count(raw) };
        let status = unsafe { sql::sqlite3_step(raw) };
        if !matches!(status, sql::SQLITE_DONE | sql::SQLITE_ROW) { return None; }
        let ok = match (step.kind, count, status) {
            (Kind::Exec, 0, sql::SQLITE_DONE) => true,
            (Kind::Change, 0, sql::SQLITE_DONE) => unsafe { sql::sqlite3_changes(db.0.as_ptr()) == 1 },
            (Kind::Expect, 1, sql::SQLITE_ROW) => unsafe {
                sql::sqlite3_column_type(raw, 0) == sql::SQLITE_INTEGER && sql::sqlite3_column_int64(raw, 0) == 1
                    && sql::sqlite3_step(raw) == sql::SQLITE_DONE
            },
            (Kind::Result, 1, sql::SQLITE_ROW) if unsafe { sql::sqlite3_column_type(raw, 0) == sql::SQLITE_BLOB } => {
                let len = usize::try_from(unsafe { sql::sqlite3_column_bytes(raw, 0) }).ok()?;
                if len == 0 || len >= capacity { return None; }
                let ptr = NonNull::new(unsafe { sql::sqlite3_column_blob(raw, 0) }.cast_mut().cast::<u8>())?;
                let mut result = vec![0];
                result.extend_from_slice(unsafe { std::slice::from_raw_parts(ptr.as_ptr(), len) });
                return (unsafe { sql::sqlite3_step(raw) } == sql::SQLITE_DONE).then_some(result);
            }
            _ => false,
        };
        if !ok {
            if step.label.is_empty() || step.label.len() >= capacity { return None; }
            return Some([&[1][..], step.label.as_bytes()].concat());
        }
    }
    None
}

pub(super) fn transaction(grant: &Grant, plan: &[u8], bindings: &[u8], blob: &[u8], capacity: usize, mut deadline: Instant) -> Option<Vec<u8>> {
    let capacity = capacity.min(grant.reply_bytes);
    if capacity == 0 { return None; }
    let provision = grant.rights.provision();
    let steps = steps(plan, provision)?;
    if provision && (!bindings.is_empty() || !blob.is_empty() || capacity < 3) { return None; }
    let values = values(bindings)?;
    let db = connect(grant, deadline)?;
    let raw = db.0.as_ptr();
    // Both callback contexts outlive this synchronous statement evaluation.
    // A closed connection rolls back an unfinished transaction.
    unsafe {
        sql::sqlite3_progress_handler(raw, 1024, Some(progress), (&mut deadline as *mut Instant).cast());
    }
    let result = if provision {
        schema::create(&db, &grant.rights.create, &steps)?;
        b"\0ok".to_vec()
    } else {
        if unsafe { sql::sqlite3_set_authorizer(raw, Some(authorize), (&grant.rights as *const Rights).cast_mut().cast()) } != sql::SQLITE_OK {
            return None;
        }
        run_steps(&db, &steps, &values, blob, capacity)?
    };
    if result[0] == 0 {
        let left = deadline.checked_duration_since(Instant::now())?.as_millis().min(5000) as i32;
        unsafe {
            if sql::sqlite3_get_autocommit(raw) != 0
                || sql::sqlite3_set_authorizer(raw, None, null_mut()) != sql::SQLITE_OK
                || sql::sqlite3_busy_timeout(raw, left) != sql::SQLITE_OK
            { return None; }
        }
        exec(&db, c"COMMIT")?;
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use super::super::Rights;
    use std::os::unix::fs::DirBuilderExt;

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn denial_and_an_oversized_reply_roll_back_before_any_success() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = Fixture(std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-sqlite-{}-{stamp}", std::process::id())));
        fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
        let database = root.0.join("fixture.db");
        let path = CString::new(database.as_os_str().as_bytes()).unwrap();
        let mut raw = null_mut();
        assert_eq!(unsafe { sql::sqlite3_open(path.as_ptr(), &mut raw) }, sql::SQLITE_OK);
        let setup = Db(NonNull::new(raw).unwrap());
        exec(&setup, c"CREATE TABLE visible(value BLOB); CREATE TABLE private(value BLOB)").unwrap();
        drop(setup);

        let mut rights = Rights::default();
        rights.add("read", "visible").unwrap();
        rights.add("insert", "visible").unwrap();
        let journal = database.with_file_name("fixture.db-journal");
        std::os::unix::fs::symlink(&database, &journal).unwrap();
        assert!(check_files(&database, &[]).is_none());
        fs::remove_file(&journal).unwrap();
        use std::os::unix::fs::MetadataExt;
        let identity = |path: &Path| {
            let meta = fs::metadata(path).unwrap();
            (meta.dev(), meta.ino())
        };
        assert!(check_files(&database, &[identity(&database)]).is_none());
        fs::write(&journal, b"fixture key").unwrap();
        assert!(check_files(&database, &[identity(&journal)]).is_none());
        fs::remove_file(journal).unwrap();
        let grant = Grant::new(database.to_str().unwrap(), rights, &[]).unwrap();
        let execute = |last: &str, cap| transaction(
            &grant, format!("change\tinsert failed\tINSERT INTO visible VALUES (:blob)\n{last}\n").as_bytes(),
            b"", b"abc", cap, Instant::now() + Duration::from_secs(2),
        );
        for statement in ["SELECT value FROM private", "DELETE FROM visible", "COMMIT", "ATTACH ':memory:' AS other"] {
            assert!(execute(&format!("result\tdenied\t{statement}"), 64).is_none(), "{statement}");
        }
        assert!(execute("result\tshort\tSELECT value FROM visible", 3).is_none());
        assert_eq!(execute("result\tinvalid\tSELECT value FROM visible", 64).unwrap(), b"\0abc");
        let plan = b"result\tinvalid\tSELECT value FROM visible\n";
        assert_eq!(transaction(&grant, plan, b"", b"", 64, Instant::now() + Duration::from_secs(2)).unwrap(), b"\0abc");

        let big = vec![7; 20_000];
        let big_plan = b"change\tinsert failed\tINSERT INTO visible VALUES (:blob)\nresult\tinvalid\tSELECT value FROM visible WHERE value=:blob\n";
        let deadline = || Instant::now() + Duration::from_secs(2);
        assert!(transaction(&grant, big_plan, b"", &big, 20_001, deadline()).is_none());
        let grant = grant.with_reply_bytes(Some(20_001));
        assert!(transaction(&grant, big_plan, b"", &big, 20_000, deadline()).is_none());
        let reply = transaction(&grant, big_plan, b"", &big, 20_001, deadline()).unwrap();
        assert_eq!(&reply[1..], big);

        let lease = connect(&grant, deadline()).unwrap();
        let mut rights = Rights::default();
        rights.add("read", "visible").unwrap();
        let reader = Grant::new(database.to_str().unwrap(), rights, &[]).unwrap().with_reply_bytes(Some(4));
        let plan = b"result\tinvalid\tSELECT value FROM visible WHERE length(value)=3 AND typeof(value)='blob'\n";
        assert_eq!(transaction(&reader, plan, b"", b"", 64, deadline()).unwrap(), b"\0abc");
        let db = connect(&reader, deadline()).unwrap();
        assert_eq!(unsafe { sql::sqlite3_db_readonly(db.0.as_ptr(), c"main".as_ptr()) }, 1);
        drop(lease);
    }

    #[test]
    fn table_creation_is_scoped_and_atomic() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = Fixture(std::env::temp_dir().canonicalize().unwrap().join(format!("witchy-schema-{}-{stamp}", std::process::id())));
        fs::DirBuilder::new().mode(0o700).create(&root.0).unwrap();
        let database = root.0.join("fixture.db");
        fs::File::create(&database).unwrap();
        let filename = database.to_str().unwrap();
        let mut mixed = Rights::default();
        mixed.add("create", "items").unwrap();
        mixed.add("read", "items").unwrap();
        assert!(Grant::new(filename, mixed, &[]).is_err());

        let mut rights = Rights::default();
        rights.add("create", "items,attempts").unwrap();
        let grant = Grant::new(filename, rights, &[]).unwrap();
        let run = |plan: &str, capacity| transaction(
            &grant, plan.as_bytes(), b"", b"", capacity, Instant::now() + Duration::from_secs(2),
        );
        let items = "CREATE TABLE IF NOT EXISTS items(id TEXT PRIMARY KEY, value BLOB UNIQUE)";
        assert_eq!(run(&format!("exec\t\t{items}\n"), 3).unwrap(), b"\0ok");
        assert_eq!(run(&format!("exec\t\t{items}\n"), 3).unwrap(), b"\0ok");

        for statement in [
            "CREATE TABLE forbidden(value BLOB)",
            "CREATE TEMP TABLE attempts(value BLOB)",
            "CREATE TABLE forbidden AS SELECT value FROM items",
            "CREATE TABLE attempts AS SELECT value FROM items",
            "CREATE VIEW attempts AS SELECT value FROM items",
            "INSERT INTO items VALUES ('a', X'01')",
            "CREATE INDEX extra ON items(value)",
            "ALTER TABLE items ADD COLUMN bad INTEGER",
            "DROP TABLE items", "PRAGMA writable_schema=ON",
        ] {
            assert!(run(&format!("exec\t\t{statement}\n"), 64).is_none(), "{statement}");
        }
        let attempts = "exec\t\tCREATE TABLE attempts(id TEXT PRIMARY KEY REFERENCES items(id))\n";
        assert!(run(attempts, 2).is_none());
        assert!(run(&format!("{attempts}exec\t\tDROP TABLE items\n"), 64).is_none());
        assert_eq!(run(attempts, 64).unwrap(), b"\0ok");

        let mut data_rights = Rights::default();
        data_rights.add("read", "items").unwrap();
        let data = Grant::new(filename, data_rights, &[]).unwrap();
        assert!(transaction(&data, b"exec\t\tCREATE TABLE ordinary(value INTEGER)\nresult\tmissing\tSELECT CAST(1 AS BLOB)\n",
            b"", b"", 64, Instant::now() + Duration::from_secs(2)).is_none());
    }
}
