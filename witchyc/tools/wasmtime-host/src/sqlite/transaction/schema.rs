use std::cell::Cell;
use std::collections::BTreeSet;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr::null_mut;

use libsqlite3_sys as sql;

use super::{Db, Step, prepare};

struct Creation<'a> {
    allowed: &'a BTreeSet<String>,
    table: Cell<Option<&'a str>>,
}

unsafe extern "C" fn authorize(
    state: *mut c_void, action: c_int, target: *const c_char, detail: *const c_char,
    database: *const c_char, source: *const c_char,
) -> c_int {
    if target.is_null() || database.is_null() || !source.is_null()
        || unsafe { CStr::from_ptr(database) } != c"main"
    {
        return sql::SQLITE_DENY;
    }
    let creation = unsafe { &*state.cast::<Creation<'_>>() };
    let Ok(target) = unsafe { CStr::from_ptr(target) }.to_str() else { return sql::SQLITE_DENY; };
    if action == sql::SQLITE_CREATE_TABLE && creation.table.get().is_none() {
        if let Some(allowed) = creation.allowed.get(target) {
            creation.table.set(Some(allowed));
            return sql::SQLITE_OK;
        }
        return sql::SQLITE_DENY;
    }
    // SQLite inserts a schema row before its CREATE_TABLE notification. No
    // prepared statement is stepped unless it produced that notification.
    if target == "sqlite_master" && action == sql::SQLITE_INSERT {
        return sql::SQLITE_OK;
    }
    let Some(table) = creation.table.get() else { return sql::SQLITE_DENY; };
    let allowed = match action {
        sql::SQLITE_READ => target == "sqlite_master" || target == table,
        sql::SQLITE_UPDATE => target == "sqlite_master",
        sql::SQLITE_CREATE_INDEX if !detail.is_null() => {
            let owner = unsafe { CStr::from_ptr(detail) }.to_bytes();
            owner == table.as_bytes() && implicit_index(target, table)
        }
        _ => false,
    };
    if allowed { sql::SQLITE_OK } else { sql::SQLITE_DENY }
}

fn implicit_index(index: &str, table: &str) -> bool {
    let number = index.strip_prefix("sqlite_autoindex_")
        .and_then(|tail| tail.strip_prefix(table))
        .and_then(|tail| tail.strip_prefix('_'));
    number.is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

pub(super) fn create(db: &Db, tables: &BTreeSet<String>, steps: &[Step<'_>]) -> Option<()> {
    let creation = Creation { allowed: tables, table: Cell::new(None) };
    let raw = db.0.as_ptr();
    let context = (&creation as *const Creation<'_>).cast_mut().cast();
    if unsafe { sql::sqlite3_set_authorizer(raw, Some(authorize), context) } != sql::SQLITE_OK {
        return None;
    }
    let result = steps.iter().try_for_each(|step| {
        creation.table.set(None);
        let statement = prepare(db, step.sql)?;
        if creation.table.get().is_none() { return None; }
        let statement = statement.0.as_ptr();
        let finished = unsafe { sql::sqlite3_bind_parameter_count(statement) == 0
            && sql::sqlite3_column_count(statement) == 0
            && sql::sqlite3_step(statement) == sql::SQLITE_DONE };
        finished.then_some(())
    });
    if unsafe { sql::sqlite3_set_authorizer(raw, None, null_mut()) } != sql::SQLITE_OK {
        return None;
    }
    result
}
