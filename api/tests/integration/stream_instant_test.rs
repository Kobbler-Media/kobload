use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::resolve_fixture::Fixture;
use common::stream_fixtures::hash;
use nats_jobs::types::StreamPriority;

#[tokio::test]
async fn test_warm_resolves_yield_to_a_user_resolve() {
    let f = Arc::new(Fixture::new(40, &[]).await);
    let warm_hashes: Vec<String> = ['1', '2', '3', '4'].map(hash).to_vec();
    let warming = {
        let f = f.clone();
        let hashes = warm_hashes.clone();
        tokio::spawn(async move {
            for h in &hashes {
                f.resolve_with(h, None, Some(StreamPriority::Warm))
                    .await
                    .unwrap();
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(120)).await;

    f.resolve(&hash('u')).await.unwrap();
    let user_finished = Instant::now();
    warming.await.unwrap();

    let calls = f.mock.calls();
    let user_started = calls
        .iter()
        .find(|c| c.hash == hash('u'))
        .map(|c| c.started)
        .unwrap();
    let grace = Duration::from_millis(15);
    let interleaved: Vec<_> = calls
        .iter()
        .filter(|c| warm_hashes.contains(&c.hash))
        .filter(|c| c.started > user_started + grace && c.started < user_finished)
        .collect();
    assert!(
        interleaved.is_empty(),
        "warm resolves ran during a user resolve: {interleaved:?}"
    );

    let before = f.mock.calls().len();
    f.resolve(&warm_hashes[0]).await.unwrap();
    assert_eq!(f.mock.calls().len(), before, "a pre-warmed tap is free");
    Arc::into_inner(f).unwrap().server.cleanup().await;
}

#[tokio::test]
async fn test_repeat_search_and_resolve_of_a_title_makes_no_provider_calls() {
    let f = Fixture::new(2, &[]).await;
    let pack = hash('p');
    let options = vec![
        f.file_option(&pack, 1, "Show.S01E02.1080p.mkv"),
        f.option(&hash('a')),
        f.option(&hash('b')),
    ];
    f.warm_options(options.clone()).await.unwrap();
    f.resolve_file(&pack, 1).await.unwrap();

    let before = f.mock.calls().len();
    f.warm_options(options).await.unwrap();
    f.resolve_file(&pack, 1).await.unwrap();
    let repeat: Vec<_> = f.mock.calls().into_iter().skip(before).collect();
    assert!(repeat.is_empty(), "{repeat:?}");
    f.server.cleanup().await;
}
