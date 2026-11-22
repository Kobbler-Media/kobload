//! Every path that finishes with a SABnzbd job — import confirmed, import
//! rejected, content rejected, SAB-side failure, cancel, clear — must leave
//! neither the SAB history entry nor the job folder behind. SAB's own
//! `del_files` never removes completed jobs' files, so the mock does not either.

use crate::common;

use serde_json::json;
use uuid::Uuid;

use common::sab::{SabFixture as Fixture, snapshot};
use kobload_api::services::import_outcome;

struct Layout {
    case: &'static str,
    files: &'static [&'static str],
    name: &'static str,
    storage: &'static str,
    gone: &'static [&'static str],
    kept: &'static [&'static str],
}

const LAYOUTS: &[Layout] = &[
    Layout {
        case: "single file",
        files: &["complete/Job/Job.mkv"],
        name: "Job",
        storage: "complete/Job/Job.mkv",
        gone: &["complete/Job"],
        kept: &["complete"],
    },
    Layout {
        case: "multi-file directory",
        files: &["complete/Job/a.mkv", "complete/Job/a.nfo"],
        name: "Job",
        storage: "complete/Job",
        gone: &["complete/Job"],
        kept: &["complete"],
    },
    Layout {
        case: "recursive unpack into a subfolder",
        files: &["complete/Job/DISC/disc.iso"],
        name: "Job",
        storage: "complete/Job/DISC/disc.iso",
        gone: &["complete/Job"],
        kept: &["complete"],
    },
    Layout {
        case: "same-named inner folder",
        files: &["complete/Job/Job/Job.mkv"],
        name: "Job",
        storage: "complete/Job/Job/Job.mkv",
        gone: &["complete/Job"],
        kept: &["complete"],
    },
    Layout {
        case: ".1 duplicate folder",
        files: &["complete/Job/Job.mkv", "complete/Job.1/Job.mkv"],
        name: "Job",
        storage: "complete/Job.1/Job.mkv",
        gone: &["complete/Job.1"],
        kept: &["complete/Job/Job.mkv"],
    },
    Layout {
        case: "_FAILED_ folder",
        files: &["complete/_FAILED_Job/Job.mkv"],
        name: "Job",
        storage: "complete/_FAILED_Job",
        gone: &["complete/_FAILED_Job"],
        kept: &["complete"],
    },
    Layout {
        case: "file loose in the complete root",
        files: &["complete/Loose.mkv", "complete/Other/Other.mkv"],
        name: "Loose",
        storage: "complete/Loose.mkv",
        gone: &[],
        kept: &["complete/Other/Other.mkv"],
    },
];

#[tokio::test]
async fn import_confirmation_deletes_the_whole_sab_job_folder() {
    let fx = Fixture::start(true).await;
    let mut failures = Vec::new();
    for (i, layout) in LAYOUTS.iter().enumerate() {
        for stale_snapshot in [false, true] {
            let base = format!("{i}-{stale_snapshot}");
            let at = |rel: &str| format!("{base}/{rel}");
            let nzo = format!("nzo-{base}");
            for file in layout.files {
                fx.touch(&at(file));
            }
            fx.sab_job(&nzo, layout.name, "Completed", &at(layout.storage));
            let stored = if stale_snapshot {
                String::new()
            } else {
                fx.path(&at(layout.storage)).to_string_lossy().into_owned()
            };
            let id = fx.download(&nzo, "completed", snapshot(&stored)).await;

            let resp = fx
                .server
                .admin_client()
                .post(&format!("/api/v1/downloads/{id}/imported"))
                .await;
            assert_eq!(resp.status, 204, "{}", layout.case);

            let case = format!("{} (stale snapshot: {stale_snapshot})", layout.case);
            let gone: Vec<String> = layout.gone.iter().map(|r| at(r)).collect();
            let kept: Vec<String> = layout.kept.iter().map(|r| at(r)).collect();
            failures.extend(fx.release_failures(
                &case,
                &[nzo.as_str()],
                &gone.iter().map(String::as_str).collect::<Vec<_>>(),
                &kept.iter().map(String::as_str).collect::<Vec<_>>(),
            ));
        }
    }
    fx.cleanup().await;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn import_confirmation_deletes_every_sub_job_of_a_split_nzb() {
    let fx = Fixture::start(true).await;
    fx.touch("complete/Job.E01/E01.mkv");
    fx.touch("complete/Job.E02/E02.mkv");
    fx.sab_job("nzo_a", "Job.E01", "Completed", "complete/Job.E01/E01.mkv");
    fx.sab_job("nzo_b", "Job.E02", "Completed", "complete/Job.E02/E02.mkv");
    let id = fx
        .download(
            "nzo_a,nzo_b",
            "completed",
            json!({ "nzo_ids": "nzo_a,nzo_b" }),
        )
        .await;

    let resp = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/downloads/{id}/imported"))
        .await;
    assert_eq!(resp.status, 204);

    let failures = fx.release_failures(
        "split nzb",
        &["nzo_a", "nzo_b"],
        &["complete/Job.E01", "complete/Job.E02"],
        &["complete"],
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    fx.cleanup().await;
}

#[derive(Clone, Copy, Debug)]
enum Exit {
    ImportConfirmedNats,
    ImportFailedHttp,
    ImportFailedNats,
    ContentRejected,
    SabFailed,
    CancelDelete,
    ClearWithCancel,
}

impl Exit {
    const ALL: [Self; 7] = [
        Self::ImportConfirmedNats,
        Self::ImportFailedHttp,
        Self::ImportFailedNats,
        Self::ContentRejected,
        Self::SabFailed,
        Self::CancelDelete,
        Self::ClearWithCancel,
    ];
}

async fn finish(fx: &Fixture, exit: Exit, id: Uuid) {
    let state = &fx.server.app_state;
    match exit {
        Exit::ImportConfirmedNats => {
            assert!(import_outcome::confirm_import(state, id).await.unwrap());
        }
        Exit::ImportFailedNats => import_outcome::fail_import(state, id, Some("rejected")).await,
        Exit::ImportFailedHttp => {
            let resp = fx
                .server
                .admin_client()
                .post_json(
                    &format!("/api/v1/downloads/{id}/import-failed"),
                    &json!({ "error": "rejected" }),
                )
                .await;
            assert_eq!(resp.status, 204);
        }
        Exit::ContentRejected => {
            use kobload_api::jobs::handlers::MetadataExtractionHandler;
            use kobload_api::jobs::worker::JobHandler;
            let handler = MetadataExtractionHandler::new(
                state.pool.clone(),
                state.nats.clone(),
                state.http_client.clone(),
                state.download_events.clone(),
                state.download_clients.clone(),
            );
            let out = handler
                .execute(
                    Uuid::now_v7(),
                    json!({ "download_id": id.to_string() }),
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(out["status"], "failed_no_content");
        }
        Exit::SabFailed => kobload_api::polling::poll_once(state).await,
        Exit::CancelDelete => {
            let resp = fx
                .server
                .admin_client()
                .delete(&format!("/api/v1/downloads/{id}?cancel=true"))
                .await;
            assert_eq!(resp.status, 204);
        }
        Exit::ClearWithCancel => {
            let resp = fx
                .server
                .admin_client()
                .delete("/api/v1/downloads?cancel_protocols=usenet")
                .await;
            assert_eq!(resp.status, 200);
        }
    }
}

async fn seed_for(fx: &Fixture, exit: Exit) -> (Uuid, &'static str) {
    match exit {
        Exit::SabFailed => {
            fx.touch("incomplete/Job/Job.mkv.part");
            fx.sab_job("nzo", "Job", "Failed", "incomplete/Job");
            let id = fx.download("nzo", "downloading", json!({})).await;
            (id, "incomplete/Job")
        }
        Exit::ContentRejected => {
            fx.touch("complete/Job/Job.exe");
            fx.sab_job("nzo", "Job", "Completed", "complete/Job/Job.exe");
            let exe = fx.path("complete/Job/Job.exe");
            let id = fx
                .download("nzo", "completed", snapshot(&exe.to_string_lossy()))
                .await;
            let files = json!([{
                "id": Uuid::now_v7(),
                "absolute_path": exe.to_string_lossy(),
                "relative_path": "Job.exe",
                "size": 1,
                "extension": "exe",
            }]);
            sqlx::query("UPDATE downloads SET files_json = $1 WHERE id = $2")
                .bind(files)
                .bind(id)
                .execute(&fx.server.pool)
                .await
                .unwrap();
            (id, "complete/Job")
        }
        _ => {
            fx.touch("complete/Job/Job.mkv");
            fx.sab_job("nzo", "Job", "Completed", "complete/Job/Job.mkv");
            let storage = fx.path("complete/Job/Job.mkv");
            let id = fx
                .download("nzo", "completed", snapshot(&storage.to_string_lossy()))
                .await;
            (id, "complete/Job")
        }
    }
}

#[tokio::test]
async fn every_terminal_path_releases_the_sab_job() {
    let mut failures = Vec::new();
    for exit in Exit::ALL {
        let fx = Fixture::start(true).await;
        let (id, job_dir) = seed_for(&fx, exit).await;
        finish(&fx, exit, id).await;
        failures.extend(fx.release_failures(&format!("{exit:?}"), &["nzo"], &[job_dir], &[]));
        fx.cleanup().await;
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn delete_after_import_off_keeps_imported_and_failed_jobs() {
    for exit in [
        Exit::ImportFailedHttp,
        Exit::ContentRejected,
        Exit::SabFailed,
    ] {
        let fx = Fixture::start(false).await;
        let (id, job_dir) = seed_for(&fx, exit).await;
        finish(&fx, exit, id).await;
        assert!(fx.path(job_dir).exists(), "{exit:?}: job folder must stay");
        assert!(
            fx.sab.deleted.lock().unwrap().is_empty(),
            "{exit:?}: SAB entry must stay"
        );
        fx.cleanup().await;
    }

    let fx = Fixture::start(false).await;
    let (id, job_dir) = seed_for(&fx, Exit::ImportFailedHttp).await;
    let resp = fx
        .server
        .admin_client()
        .post(&format!("/api/v1/downloads/{id}/imported"))
        .await;
    assert_eq!(resp.status, 204);
    assert!(fx.path(job_dir).exists(), "imported: job folder must stay");
    fx.cleanup().await;
}
