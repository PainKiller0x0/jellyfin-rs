use sea_orm::{
    sea_query::{Alias, Expr, Query},
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement, Value,
};

pub fn insert_into(table: &str) -> sea_orm::sea_query::InsertStatement {
    Query::insert().into_table(Alias::new(table)).to_owned()
}

pub fn table_col(table: &str, column: &str) -> Expr {
    Expr::col((Alias::new(table), Alias::new(column)))
}

pub fn alias_col(column: &str) -> Expr {
    Expr::col(Alias::new(column))
}

pub async fn exec_stmt(db: &DatabaseConnection, backend: DbBackend, sql: String) -> Result<(), DbErr> {
    db.execute(Statement::from_string(backend, sql)).await.map(|_| ())
}

pub fn db_backend(url: &str) -> DbBackend {
    if url.starts_with("sqlite:") {
        DbBackend::Sqlite
    } else {
        DbBackend::Postgres
    }
}

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
