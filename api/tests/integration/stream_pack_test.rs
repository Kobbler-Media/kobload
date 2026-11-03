use crate::common;

use std::time::Duration;

use common::debrid_mock::{MockDebrid, MockRelease};
use common::resolve_fixture::Fixture;
use common::stream_fixtures::hash;

const PACK: &[&str] = &[
    "Show.S01/Show.S01E01.1080p.mkv",
    "Show.S01/Show.S01E02.1080p.mkv",
    "Show.S01/Show.S01E03.1080p.mkv",
    "Show.S01/Sample/show.s01e01.sample.mkv",
    "Show.S01/Show.S01.nfo",
];

async fn pack_fixture(
    setup: impl FnOnce(std::sync::Arc<MockDebrid>) -> std::sync::Arc<MockDebrid>,
) -> (Fixture, String) {
    let p = hash('p');
    let mock = setup(MockDebrid::new(Duration::from_millis(2)).with_pack(&p, PACK));
    (Fixture::with_mock(mock).await, p)
}

#[tokio::test]
async fn test_playing_a_pack_episode_selects_every_episode_and_streams_that_one() {
    let (f, p) = pack_fixture(|m| m).await;
    let resolved = f.resolve_file(&p, 1).await.unwrap();
    assert!(resolved.cached);
    assert_eq!(resolved.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(f.mock.selections(&p), vec![vec![1, 2, 3]]);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_next_episode_plays_from_the_same_pack_torrent() {
    let (f, p) = pack_fixture(|m| m).await;
    f.resolve_file(&p, 0).await.unwrap();
    let next = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(next.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(f.mock.count("add_magnet", Some(&p)), 1);
    assert_eq!(f.mock.count("select_files", Some(&p)), 1);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_pack_on_the_account_without_the_wanted_file_is_added_again() {
    let (f, p) = pack_fixture(|m| {
        let p = hash('p');
        m.with_torrent(&p, &[1])
    })
    .await;
    let resolved = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(resolved.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(
        f.mock.selections(&p),
        vec![vec![1], vec![1, 2, 3]],
        "the existing torrent is someone's and stays as it was"
    );
    let again = f.resolve_file(&p, 2).await.unwrap();
    assert_eq!(again.filename, "Show.S01E03.1080p.mkv");
    assert_eq!(f.mock.count("add_magnet", Some(&p)), 1);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_partly_cached_pack_falls_back_to_just_the_wanted_file() {
    let (f, p) = pack_fixture(|m| {
        let p = hash('p');
        m.with_uncached_files(&p, &[3])
    })
    .await;
    let resolved = f.resolve_file(&p, 1).await.unwrap();
    assert!(resolved.cached, "{resolved:?}");
    assert_eq!(resolved.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(f.mock.selections(&p), vec![vec![2]]);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_warmed_pack_episode_resolves_to_that_episode_without_calls() {
    let (f, p) = pack_fixture(|m| m).await;
    f.warm_options(vec![f.file_option(&p, 1, "Show.S01E02.1080p.mkv")])
        .await
        .unwrap();
    let before = f.mock.calls().len();
    let resolved = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(resolved.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(f.mock.calls().len(), before, "{:?}", f.mock.calls());
    assert_eq!(f.mock.selections(&p), vec![vec![1, 2, 3]]);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_the_copy_that_holds_the_wanted_file_is_reused_whichever_it_is() {
    let (f, p) = pack_fixture(|m| {
        let p = hash('p');
        m.with_releases(&[(&p, MockRelease::Refused)])
            .with_torrent(&p, &[2])
            .with_torrent(&p, &[3])
    })
    .await;
    let resolved = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(resolved.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(
        f.mock.count("add_magnet", Some(&p)),
        0,
        "the provider refuses this hash, so only the older copy can serve E02"
    );
    f.server.cleanup().await;
}

/// Every episode of a partly cached pack used to cost two adds: the whole pack, found
/// partial and deleted, then the single file. Once that is known the next goes straight
/// to its single file.
#[tokio::test]
async fn test_binging_a_partly_cached_pack_skips_the_doomed_pack_add() {
    let (f, p) = pack_fixture(|m| {
        let p = hash('p');
        m.with_uncached_files(&p, &[3])
    })
    .await;
    f.resolve_file(&p, 0).await.unwrap();
    assert_eq!(f.mock.count("add_magnet", Some(&p)), 2);

    let next = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(next.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(
        f.mock.count("add_magnet", Some(&p)),
        3,
        "{:?}",
        f.mock.selections(&p)
    );
    assert_eq!(f.mock.selections(&p), vec![vec![1], vec![2]]);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warming_learns_a_partly_cached_pack_too() {
    let (f, p) = pack_fixture(|m| {
        let p = hash('p');
        m.with_uncached_files(&p, &[3])
    })
    .await;
    f.warm_options(vec![f.file_option(&p, 0, "Show.S01E01.1080p.mkv")])
        .await
        .unwrap();
    assert_eq!(f.mock.count("add_magnet", Some(&p)), 2);
    f.warm_options(vec![f.file_option(&p, 1, "Show.S01E02.1080p.mkv")])
        .await
        .unwrap();
    assert_eq!(
        f.mock.count("add_magnet", Some(&p)),
        3,
        "{:?}",
        f.mock.selections(&p)
    );
    f.server.cleanup().await;
}

/// What a whole-pack success taught holds after the copy is gone: the pack is selected
/// again, so the episodes after it are ready in the same torrent.
#[tokio::test]
async fn test_a_known_cached_pack_is_selected_whole_again() {
    let (f, p) = pack_fixture(|m| m).await;
    f.resolve_file(&p, 0).await.unwrap();
    f.mock.forget_torrents();
    let next = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(next.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(f.mock.selections(&p), vec![vec![1, 2, 3]]);
    f.server.cleanup().await;
}

/// RD can serve a multi-file selection as one archive; Torrentio then adds the torrent
/// again with just the wanted file, which streams as itself.
#[tokio::test]
async fn test_a_pack_served_as_an_archive_is_added_again_for_the_one_file() {
    let (f, p) = pack_fixture(|m| m).await;
    f.mock.archive_multi_selections(&p);
    let resolved = f.resolve_file(&p, 1).await.unwrap();
    assert_eq!(resolved.filename, "Show.S01E02.1080p.mkv");
    assert_eq!(f.mock.selections(&p), vec![vec![2]]);
    f.server.cleanup().await;
}
