use polymer_library::jobs::{JobError, JobKind, JobSpec, JobState, JobStore};
use serde_json::json;

#[tokio::test]
async fn cancellation_and_claim_keep_the_dedupe_key_until_work_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let store = JobStore::open(directory.path().join("jobs.db"))
        .await
        .unwrap();
    for persistent in [false, true] {
        for claim_first in [false, true] {
            let spec = JobSpec {
                kind: JobKind::Import,
                parent: None,
                title: "Game".to_owned(),
                subject: Default::default(),
                params: json!({}),
                dedupe: Some(format!("{persistent}-{claim_first}")),
                steps: Vec::new(),
                persistent,
            };
            let (job, created) = store.enqueue(spec.clone()).await.unwrap();
            assert!(created);
            let (cancelled, claimed) = if claim_first {
                let (claimed, cancelled) =
                    tokio::join!(biased; store.claim(job.id), store.cancel(job.id));
                (cancelled, claimed)
            } else {
                tokio::join!(biased; store.cancel(job.id), store.claim(job.id))
            };
            let claimed = claimed.unwrap();
            match claimed {
                Some(handle) => {
                    assert!(matches!(
                        cancelled,
                        Err(JobError::InvalidState(_, JobState::Running))
                    ));
                    let (duplicate, created) = store.enqueue(spec.clone()).await.unwrap();
                    assert!(!created);
                    assert_eq!(duplicate.id, job.id);
                    let (completed, failed) = tokio::join!(
                        handle.finish(JobState::Completed, None),
                        handle.finish(JobState::Failed, Some("late failure".to_owned()))
                    );
                    assert_ne!(completed.is_ok(), failed.is_ok());
                    let final_state = completed.or(failed).unwrap().state;
                    drop(handle);
                    assert_eq!(store.get(job.id).await.unwrap().unwrap().state, final_state);
                }
                None => assert_eq!(cancelled.unwrap().state, JobState::Cancelled),
            }
            let (replacement, created) = store.enqueue(spec).await.unwrap();
            assert!(created);
            assert_ne!(replacement.id, job.id);
            store.cancel(replacement.id).await.unwrap();
        }
    }
}
