use sea_orm::{DbErr, QueryResult};

pub trait QueryResultExt {
    fn get_str(&self, column: &str) -> Result<String, DbErr>;
    fn get_opt_str(&self, column: &str) -> Result<Option<String>, DbErr>;
    fn get_i64(&self, column: &str) -> Result<i64, DbErr>;
    fn get_opt_i64(&self, column: &str) -> Result<Option<i64>, DbErr>;
    fn get_f64(&self, column: &str) -> Result<Option<f64>, DbErr>;
    fn get_bool_from_i64(&self, column: &str) -> Result<bool, DbErr>;
}

impl QueryResultExt for QueryResult {
    fn get_str(&self, column: &str) -> Result<String, DbErr> {
        self.try_get::<String>("", column)
    }

    fn get_opt_str(&self, column: &str) -> Result<Option<String>, DbErr> {
        self.try_get::<Option<String>>("", column)
    }

    fn get_i64(&self, column: &str) -> Result<i64, DbErr> {
        self.try_get::<i64>("", column)
    }

    fn get_opt_i64(&self, column: &str) -> Result<Option<i64>, DbErr> {
        self.try_get::<Option<i64>>("", column)
    }

    fn get_f64(&self, column: &str) -> Result<Option<f64>, DbErr> {
        self.try_get::<Option<f64>>("", column)
    }

    fn get_bool_from_i64(&self, column: &str) -> Result<bool, DbErr> {
        let val: i64 = self.try_get("", column)?;
        Ok(val != 0)
    }
}
