//! The dashboard's "Sync all": re-syncs every creator, one at a time, in a background task.
//!
//! The run lives in the server process, so closing the dashboard doesn't stop it and reopening
//! it shows the progress. A deploy or restart does stop it and forgets the progress, but each
//! creator is saved as it finishes: starting again and skipping the recently synced ones
//! carries on from there. Creators synced longest ago go first.

use axum::{extract::State, http::StatusCode, Json};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use super::{spawn_user_sync, RunningUserSyncs};
use crate::AppState;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatorOutcome {
    folder_name: String,
    success: bool,
    message: String,
    seconds: u64,
}

/// The current run, or the latest one once it's over.
#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkSyncStatus {
    running: bool,
    stop_requested: bool,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    total: usize,
    current: Option<String>,
    outcomes: Vec<CreatorOutcome>,
}

#[derive(Default)]
pub struct BulkUserSync(Mutex<BulkSyncStatus>);

impl BulkUserSync {
    fn status(&self) -> MutexGuard<'_, BulkSyncStatus> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// False while another run is going.
    fn begin(&self, total: usize) -> bool {
        let mut status = self.status();
        if status.running {
            return false;
        }
        *status = BulkSyncStatus {
            running: true,
            started_at: Some(Utc::now()),
            total,
            ..Default::default()
        };
        true
    }

    /// False once a stop was asked for, so the run ends before this creator.
    fn begin_creator(&self, folder: &str) -> bool {
        let mut status = self.status();
        if status.stop_requested {
            return false;
        }
        status.current = Some(folder.to_string());
        true
    }

    fn finish_creator(&self, outcome: CreatorOutcome) {
        let mut status = self.status();
        status.current = None;
        status.outcomes.push(outcome);
    }

    fn finish(&self) {
        let mut status = self.status();
        status.running = false;
        status.current = None;
        status.finished_at = Some(Utc::now());
    }

    /// False when there's no run to stop.
    fn request_stop(&self) -> bool {
        let mut status = self.status();
        if status.running {
            status.stop_requested = true;
        }
        status.running
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartBulkSyncRequest {
    /// Leaves out creators synced this recently, to carry on after a run that didn't finish.
    #[serde(default)]
    skip_synced_within_hours: u16,
}

#[derive(Serialize)]
pub struct BulkSyncReply {
    success: bool,
    message: String,
}

fn reply(status: StatusCode, success: bool, message: String) -> (StatusCode, Json<BulkSyncReply>) {
    (status, Json(BulkSyncReply { success, message }))
}

pub async fn start_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<StartBulkSyncRequest>,
) -> (StatusCode, Json<BulkSyncReply>) {
    let folders = sqlx::query_scalar::<_, String>(
        "SELECT folder_name FROM users
         WHERE last_synced_at IS NULL OR last_synced_at < NOW() - make_interval(hours => $1)
         ORDER BY last_synced_at ASC NULLS FIRST, folder_name",
    )
    .bind(i32::from(payload.skip_synced_within_hours))
    .fetch_all(&state.db)
    .await;

    let folders = match folders {
        Ok(folders) => folders,
        Err(e) => {
            tracing::error!("Failed to list creators to sync: {:?}", e);
            return reply(StatusCode::INTERNAL_SERVER_ERROR, false, format!("Database error: {}", e));
        }
    };
    if folders.is_empty() {
        return reply(StatusCode::OK, true, "No creators to sync".to_string());
    }
    if !state.bulk_user_sync.begin(folders.len()) {
        return reply(
            StatusCode::CONFLICT,
            false,
            "A sync of all creators is already running".to_string(),
        );
    }

    let message = format!("Syncing {} creators", folders.len());
    tokio::spawn(run(state, folders));
    reply(StatusCode::ACCEPTED, true, message)
}

pub async fn status_handler(State(state): State<Arc<AppState>>) -> Json<BulkSyncStatus> {
    Json(state.bulk_user_sync.status().clone())
}

pub async fn stop_handler(State(state): State<Arc<AppState>>) -> (StatusCode, Json<BulkSyncReply>) {
    if state.bulk_user_sync.request_stop() {
        reply(StatusCode::OK, true, "Stopping after the current creator".to_string())
    } else {
        reply(StatusCode::CONFLICT, false, "No sync of all creators is running".to_string())
    }
}

async fn run(state: Arc<AppState>, folders: Vec<String>) {
    let total = folders.len();
    tracing::info!("Sync of all creators started: {} creators", total);

    for folder in folders {
        if !state.bulk_user_sync.begin_creator(&folder) {
            break;
        }
        let started = Instant::now();
        let result = sync_creator(&state, &folder).await;
        let success = result.is_ok();
        let message = result.unwrap_or_else(|reason| reason);
        if !success {
            tracing::warn!("Sync of all creators: {} failed: {}", folder, message);
        }
        state.bulk_user_sync.finish_creator(CreatorOutcome {
            folder_name: folder,
            success,
            message,
            seconds: started.elapsed().as_secs(),
        });
    }

    state.bulk_user_sync.finish();
    let done = state.bulk_user_sync.status().outcomes.len();
    tracing::info!("Sync of all creators ended: {} of {} creators", done, total);
}

/// Says what happened to the creator, as an error if their catalog didn't get synced.
async fn sync_creator(state: &Arc<AppState>, folder: &str) -> Result<String, String> {
    // Read per creator: a run takes hours, and the admin can edit or delete creators meanwhile.
    // Syncing one deleted since the run started would bring them back.
    let user_id = sqlx::query_scalar::<_, String>("SELECT seven_tv_id FROM users WHERE folder_name = $1")
        .bind(folder)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| format!("Database error: {}", e))?;
    let Some(user_id) = user_id else {
        return Ok("Skipped: deleted during this run".to_string());
    };

    let guard = RunningUserSyncs::begin(&state.user_syncs, folder)
        .ok_or("Skipped: a sync of this creator was already running")?;
    // No image: the sync keeps the one the creator has.
    let (_, Json(response)) =
        spawn_user_sync(Arc::clone(state), guard, user_id, folder.to_string(), None).await;

    let message = response.message.unwrap_or_default();
    if response.success {
        Ok(message)
    } else {
        Err(message)
    }
}

#[cfg(test)]
mod tests {
    use super::{BulkUserSync, CreatorOutcome};

    fn outcome(folder: &str) -> CreatorOutcome {
        CreatorOutcome {
            folder_name: folder.to_string(),
            success: true,
            message: String::new(),
            seconds: 1,
        }
    }

    #[test]
    fn only_one_run_goes_at_a_time() {
        let sync = BulkUserSync::default();

        assert!(sync.begin(2));
        assert!(!sync.begin(2));

        sync.finish();
        assert!(sync.begin(2));
    }

    #[test]
    fn a_stop_ends_the_run_before_the_next_creator() {
        let sync = BulkUserSync::default();
        sync.begin(2);

        assert!(sync.begin_creator("xQc"));
        assert!(sync.request_stop());
        sync.finish_creator(outcome("xQc"));

        assert!(!sync.begin_creator("Rubius"));
    }

    #[test]
    fn there_is_nothing_to_stop_without_a_run() {
        let sync = BulkUserSync::default();
        assert!(!sync.request_stop());

        sync.begin(1);
        sync.finish();
        assert!(!sync.request_stop());
        assert!(!sync.status().stop_requested);
    }

    #[test]
    fn a_stopped_run_still_reads_as_stopped_afterwards() {
        let sync = BulkUserSync::default();
        sync.begin(1);
        sync.request_stop();
        sync.finish();

        assert!(!sync.request_stop());
        assert!(sync.status().stop_requested);
    }

    #[test]
    fn a_new_run_forgets_the_last_one() {
        let sync = BulkUserSync::default();
        sync.begin(1);
        sync.request_stop();
        sync.finish_creator(outcome("xQc"));
        sync.finish();

        sync.begin(3);

        let status = sync.status();
        assert!(status.running && !status.stop_requested);
        assert!(status.outcomes.is_empty() && status.finished_at.is_none());
        assert_eq!(status.total, 3);
    }
}
