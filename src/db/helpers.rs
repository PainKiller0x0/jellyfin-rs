use sea_orm::{DbBackend, Statement, Value};

pub fn portable_statement(backend: DbBackend, sql: &str, values: Vec<Value>) -> Statement {
    if backend == DbBackend::Postgres {
        let mut converted = String::with_capacity(sql.len());
        let mut n = 1u32;
        for ch in sql.chars() {
            if ch == '?' {
                use std::fmt::Write;
                write!(converted, "${n}").unwrap();
                n += 1;
            } else {
                converted.push(ch);
            }
        }
        Statement::from_sql_and_values(backend, &converted, values)
    } else {
        Statement::from_sql_and_values(backend, sql, values)
    }
}
