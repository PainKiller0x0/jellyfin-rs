use anyhow::Context;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde::Serialize;
use serde_json::Value;

const JOB_LEASE_SECONDS: i64 = 10 * 60;
const MAX_JOB_ATTEMPTS: i64 = 4;

#[derive(Serialize)]
pub struct RefreshJobPayload {
    pub library_id: String,
    pub path: String,
    pub payload: Value,
}

pub struct ClaimedRefreshJob {
    pub id: i64,
    pub payload: String,
}

#[derive(Default)]
pub struct RefreshQueueStats {
    pub pending: i64,
    pub running: i64,
    pub completed: i64,
    pub failed: i64,
}

pub async fn enqueue_batch(
    db: &DatabaseConnection,
    scan_id: &str,
    jobs: &[RefreshJobPayload],
) -> anyhow::Result<usize> {
    if jobs.is_empty() {
        return Ok(0);
    }
    let payload = serde_json::to_string(jobs).context("failed to encode refresh job batch")?;
    let now = crate::util::now_unix();
    let result = db
        .execute_raw(crate::db::helpers::pg_statement(
            r#"INSERT INTO library_refresh_jobs
                   (scan_id, library_id, path, payload, status, attempts, available_at, created_at, updated_at)
               SELECT ?, job.library_id, job.path, job.payload, 'pending', 0, ?, ?, ?
                 FROM jsonb_to_recordset(?::jsonb)
                   AS job(library_id TEXT, path TEXT, payload JSONB)
               ON CONFLICT (path) DO UPDATE
                   SET scan_id = EXCLUDED.scan_id,
                       library_id = EXCLUDED.library_id,
                       payload = EXCLUDED.payload,
                       status = 'pending',
                       attempts = 0,
                       available_at = EXCLUDED.available_at,
                       lease_until = NULL,
                       last_error = NULL,
                       updated_at = EXCLUDED.updated_at"#,
            vec![
                scan_id.to_string().into(),
                now.into(),
                now.into(),
                now.into(),
                payload.into(),
            ],
        ))
        .await
        .context("failed to stage refresh job batch")?;
    Ok(result.rows_affected() as usize)
}

pub async fn claim_next(
    db: &DatabaseConnection,
    preferred_scan_id: &str,
) -> anyhow::Result<Option<ClaimedRefreshJob>> {
    let now = crate::util::now_unix();
    let lease_until = now + JOB_LEASE_SECONDS;
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"WITH candidate AS (
                   SELECT id
                     FROM library_refresh_jobs
                    WHERE available_at <= ?
                      AND ((status = 'pending' AND attempts < ?)
                           OR (status = 'running' AND lease_until < ?))
                    ORDER BY (scan_id = ?) DESC, id
                    FOR UPDATE SKIP LOCKED
                    LIMIT 1
               )
               UPDATE library_refresh_jobs AS job
                  SET status = 'running',
                      attempts = job.attempts + 1,
                      lease_until = ?,
                      updated_at = ?
                 FROM candidate
                WHERE job.id = candidate.id
               RETURNING job.id, job.payload::text AS payload"#,
            vec![
                now.into(),
                MAX_JOB_ATTEMPTS.into(),
                now.into(),
                preferred_scan_id.to_string().into(),
                lease_until.into(),
                now.into(),
            ],
        ))
        .await
        .context("failed to claim refresh job")?;
    row.map(|row| {
        Ok(ClaimedRefreshJob {
            id: row.try_get("", "id")?,
            payload: row.try_get("", "payload")?,
        })
    })
    .transpose()
}

pub async fn complete(db: &DatabaseConnection, job_id: i64) -> anyhow::Result<()> {
    set_terminal_status(db, job_id, "completed", None).await
}

pub async fn fail(db: &DatabaseConnection, job_id: i64, error: &str) -> anyhow::Result<()> {
    let now = crate::util::now_unix();
    db.execute_raw(crate::db::helpers::pg_statement(
        r#"UPDATE library_refresh_jobs
              SET status = CASE WHEN attempts >= ? THEN 'failed' ELSE 'pending' END,
                  available_at = CASE WHEN attempts >= ? THEN available_at ELSE ? END,
                  lease_until = NULL,
                  last_error = LEFT(?, 4000),
                  updated_at = ?
            WHERE id = ?"#,
        vec![
            MAX_JOB_ATTEMPTS.into(),
            MAX_JOB_ATTEMPTS.into(),
            (now + 2).into(),
            error.to_string().into(),
            now.into(),
            job_id.into(),
        ],
    ))
    .await
    .context("failed to record refresh job failure")?;
    Ok(())
}

async fn set_terminal_status(
    db: &DatabaseConnection,
    job_id: i64,
    status: &str,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let now = crate::util::now_unix();
    db.execute_raw(crate::db::helpers::pg_statement(
        "UPDATE library_refresh_jobs SET status = ?, lease_until = NULL, last_error = ?, updated_at = ? WHERE id = ?",
        vec![
            status.to_string().into(),
            error.map(ToString::to_string).into(),
            now.into(),
            job_id.into(),
        ],
    ))
    .await
    .context("failed to complete refresh job")?;
    Ok(())
}

pub async fn stats(db: &DatabaseConnection, scan_id: &str) -> anyhow::Result<RefreshQueueStats> {
    let row = db
        .query_one_raw(crate::db::helpers::pg_statement(
            r#"SELECT COUNT(*) FILTER (WHERE status = 'pending')::bigint AS pending,
                      COUNT(*) FILTER (WHERE status = 'running')::bigint AS running,
                      COUNT(*) FILTER (WHERE status = 'completed')::bigint AS completed,
                      COUNT(*) FILTER (WHERE status = 'failed')::bigint AS failed
                 FROM library_refresh_jobs
                WHERE scan_id = ?"#,
            vec![scan_id.to_string().into()],
        ))
        .await
        .context("failed to read refresh queue stats")?;
    let Some(row) = row else {
        return Ok(RefreshQueueStats::default());
    };
    Ok(RefreshQueueStats {
        pending: row.try_get("", "pending")?,
        running: row.try_get("", "running")?,
        completed: row.try_get("", "completed")?,
        failed: row.try_get("", "failed")?,
    })
}

pub async fn delete_completed(db: &DatabaseConnection, scan_id: &str) -> anyhow::Result<()> {
    db.execute_raw(crate::db::helpers::pg_statement(
        "DELETE FROM library_refresh_jobs WHERE scan_id = ? AND status = 'completed'",
        vec![scan_id.to_string().into()],
    ))
    .await
    .context("failed to clean completed refresh jobs")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn postgres_queue_batches_claims_and_completes_jobs() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let scan_id = uuid::Uuid::new_v4().to_string();
        let library_id = format!("refresh-queue-{scan_id}");
        let now = crate::util::now_unix();
        db.execute_raw(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, 'movies', ?, ?)",
            vec![
                library_id.clone().into(),
                library_id.clone().into(),
                now.into(),
                now.into(),
            ],
        ))
        .await
        .unwrap();
        let payloads = [
            RefreshJobPayload {
                library_id: library_id.clone(),
                path: "/media/a.mkv".to_string(),
                payload: serde_json::json!({"item": "a"}),
            },
            RefreshJobPayload {
                library_id: library_id.clone(),
                path: "/media/b.mkv".to_string(),
                payload: serde_json::json!({"item": "b"}),
            },
        ];

        assert_eq!(enqueue_batch(&db, &scan_id, &payloads).await.unwrap(), 2);
        let first = claim_next(&db, &scan_id).await.unwrap().unwrap();
        assert!(
            first.payload.contains("\"item\": \"a\"") || first.payload.contains("\"item\":\"a\"")
        );
        complete(&db, first.id).await.unwrap();
        let second = claim_next(&db, &scan_id).await.unwrap().unwrap();
        complete(&db, second.id).await.unwrap();

        let stats = stats(&db, &scan_id).await.unwrap();
        assert_eq!(stats.completed, 2);
        assert_eq!(stats.pending + stats.running + stats.failed, 0);
        delete_completed(&db, &scan_id).await.unwrap();
        db.execute_raw(crate::db::helpers::pg_statement(
            "DELETE FROM libraries WHERE id = ?",
            vec![library_id.into()],
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn newer_scan_replaces_path_job_and_expired_lease_is_reclaimed() {
        let Some(db) = crate::db::test_db().await else {
            return;
        };
        let first_scan_id = uuid::Uuid::new_v4().to_string();
        let second_scan_id = uuid::Uuid::new_v4().to_string();
        let library_id = format!("refresh-queue-{first_scan_id}");
        let now = crate::util::now_unix();
        db.execute_raw(crate::db::helpers::pg_statement(
            "INSERT INTO libraries (id, name, collection_type, created_at, updated_at) VALUES (?, ?, 'movies', ?, ?)",
            vec![
                library_id.clone().into(),
                library_id.clone().into(),
                now.into(),
                now.into(),
            ],
        ))
        .await
        .unwrap();
        let path = "/media/replaced.mkv";
        enqueue_batch(
            &db,
            &first_scan_id,
            &[RefreshJobPayload {
                library_id: library_id.clone(),
                path: path.to_string(),
                payload: serde_json::json!({"generation": 1}),
            }],
        )
        .await
        .unwrap();
        enqueue_batch(
            &db,
            &second_scan_id,
            &[RefreshJobPayload {
                library_id: library_id.clone(),
                path: path.to_string(),
                payload: serde_json::json!({"generation": 2}),
            }],
        )
        .await
        .unwrap();

        assert_eq!(stats(&db, &first_scan_id).await.unwrap().pending, 0);
        assert_eq!(stats(&db, &second_scan_id).await.unwrap().pending, 1);
        let claimed = claim_next(&db, &second_scan_id).await.unwrap().unwrap();
        assert!(claimed.payload.contains("2"));
        db.execute_raw(crate::db::helpers::pg_statement(
            "UPDATE library_refresh_jobs SET lease_until = ? WHERE id = ?",
            vec![(now - 1).into(), claimed.id.into()],
        ))
        .await
        .unwrap();
        let reclaimed = claim_next(&db, &second_scan_id).await.unwrap().unwrap();
        assert_eq!(reclaimed.id, claimed.id);
        complete(&db, reclaimed.id).await.unwrap();

        delete_completed(&db, &second_scan_id).await.unwrap();
        db.execute_raw(crate::db::helpers::pg_statement(
            "DELETE FROM libraries WHERE id = ?",
            vec![library_id.into()],
        ))
        .await
        .unwrap();
    }
}
