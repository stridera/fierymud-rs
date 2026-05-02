//! Player feedback writer: in-game `idea` / `bug` / `typo` commands
//! drop a row into the `reports` table for staff to triage. The
//! schema also tracks status, resolver, and resolution; this module
//! only writes — read-side admin tooling lives in Muditor.

use sqlx::PgPool;

use crate::enums::ReportType;

/// Record a player report. `reporter_id` is the `Characters.id` UUID
/// (string) when known; `reporter_name` is the live in-game Named
/// component (mob commanders included). The room key is captured at
/// submission time so a `bug` from a moving mob still tells staff
/// where the issue was observed.
pub async fn submit(
    pool: &PgPool,
    kind: ReportType,
    reporter_name: &str,
    reporter_id: Option<&str>,
    room_zone_id: Option<i32>,
    room_id: Option<i32>,
    message: &str,
) -> sqlx::Result<i32> {
    // Cast the enum text literal explicitly to "ReportType". Without
    // quotes Postgres lowercases the type name and sqlx's auto-cast
    // emits `reporttype` which doesn't match this schema's
    // PascalCase enum type. Other enum-bearing tables in this DB
    // generally avoid the issue by using sqlx::Type derive +
    // RenameAll which produces the right shape, but `reports` here
    // pre-existed and the type name is case-sensitive.
    let kind_str = match kind {
        ReportType::Bug => "BUG",
        ReportType::Idea => "IDEA",
        ReportType::Typo => "TYPO",
    };
    let row = sqlx::query!(
        r#"
        INSERT INTO reports
            (report_type, reporter_name, reporter_id,
             room_zone_id, room_id, message, updated_at)
        VALUES ($1::text::"ReportType", $2, $3, $4, $5, $6, CURRENT_TIMESTAMP)
        RETURNING id
        "#,
        kind_str,
        reporter_name,
        reporter_id,
        room_zone_id,
        room_id,
        message,
    )
    .fetch_one(pool)
    .await?;
    Ok(row.id)
}
