use sea_orm::{DbBackend, Statement, Value};

pub fn pg_statement(sql: &str, values: Vec<Value>) -> Statement {
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
    Statement::from_sql_and_values(DbBackend::Postgres, &converted, values)
}
