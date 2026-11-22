use nats_jobs::MediaWant;
use nats_jobs::types::StreamFile;
use usenet_proto::OutputInfo;

use crate::parsing::file::VIDEO_EXTENSIONS;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Episode { season: Option<u32>, episode: u32 },
    Largest,
}

impl Target {
    pub fn for_want(want: &MediaWant) -> Self {
        match want {
            MediaWant::TvEpisode {
                season, episode, ..
            } => match episode.trim().parse() {
                Ok(episode) => Self::Episode {
                    season: season.trim().parse().ok(),
                    episode,
                },
                Err(_) => Self::Largest,
            },
            _ => Self::Largest,
        }
    }

    pub fn from_title(title: &str) -> Self {
        let parsed = crate::parsing::parse_release(title, "tv");
        match parsed.media.as_tv() {
            Some(tv) if tv.episodes.len() == 1 => Self::Episode {
                season: tv.season,
                episode: tv.episodes[0],
            },
            _ => Self::Largest,
        }
    }

    fn matches(&self, output: &OutputInfo) -> bool {
        let Self::Episode { season, episode } = self else {
            return false;
        };
        let parsed = crate::parsing::parse_release(&stem(&output.path), "tv");
        parsed.media.as_tv().is_some_and(|tv| {
            tv.episodes.contains(episode)
                && (season.is_none() || tv.season.is_none() || tv.season == *season)
        })
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn stem(path: &str) -> String {
    let name = basename(path);
    name.rsplit_once('.')
        .map_or(name, |(stem, _)| stem)
        .to_string()
}

pub fn filename(output: &OutputInfo) -> String {
    basename(&output.path).to_string()
}

fn is_video(output: &OutputInfo) -> bool {
    basename(&output.path)
        .rsplit_once('.')
        .is_some_and(|(_, ext)| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}

fn is_sample(output: &OutputInfo) -> bool {
    output
        .path
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|t| t == "sample")
}

/// The output a request stands for: the given index, else the largest video that holds the
/// wanted episode, else the largest video that is not a sample.
pub fn choose<'a>(
    outputs: &'a [OutputInfo],
    file_id: Option<i32>,
    target: &Target,
) -> Option<&'a OutputInfo> {
    if let Some(id) = file_id {
        return outputs
            .iter()
            .find(|o| i64::try_from(o.index).ok() == Some(i64::from(id)));
    }
    let videos = || outputs.iter().filter(|o| is_video(o) && !is_sample(o));
    videos()
        .filter(|o| target.matches(o))
        .max_by_key(|o| o.size)
        .or_else(|| videos().max_by_key(|o| o.size))
}

/// Clients play the first listed file, so the one the search was for leads.
pub fn listed_files(outputs: &[OutputInfo], target: &Target) -> Vec<StreamFile> {
    let lead = choose(outputs, None, target).map(|o| o.index);
    let mut ordered: Vec<&OutputInfo> = outputs.iter().collect();
    ordered.sort_by_key(|o| (Some(o.index) != lead, o.index));
    ordered
        .into_iter()
        .filter_map(|o| {
            Some(StreamFile {
                id: i32::try_from(o.index).ok()?,
                path: o.path.clone(),
                size: i64::try_from(o.size).ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outputs(paths: &[(&str, u64)]) -> Vec<OutputInfo> {
        paths
            .iter()
            .enumerate()
            .map(|(index, (path, size))| OutputInfo {
                index,
                path: path.to_string(),
                size: *size,
            })
            .collect()
    }

    #[test]
    fn a_multi_episode_file_stands_for_each_of_its_episodes() {
        let pack = outputs(&[
            ("Show.S01E01.mkv", 10),
            ("Show.S01E02E03.mkv", 20),
            ("Show.S01E04.srt", 1),
            ("Show.S01E04.mkv", 10),
        ]);
        for (episode, index) in [(1, 0), (2, 1), (3, 1), (4, 3)] {
            let target = Target::Episode {
                season: Some(1),
                episode,
            };
            assert_eq!(
                choose(&pack, None, &target).unwrap().index,
                index,
                "E{episode}"
            );
        }
    }

    #[test]
    fn a_movie_is_its_largest_video_that_is_not_a_sample() {
        let movie = outputs(&[
            ("Movie.nfo", 100),
            ("Movie.sample.mkv", 900),
            ("Movie.mkv", 500),
            ("Extras/Featurette.mkv", 200),
        ]);
        assert_eq!(choose(&movie, None, &Target::Largest).unwrap().index, 2);
        assert_eq!(choose(&movie, Some(3), &Target::Largest).unwrap().index, 3);
        assert!(choose(&movie, Some(9), &Target::Largest).is_none());
    }

    #[test]
    fn the_wanted_file_is_listed_first_and_every_output_follows() {
        let pack = outputs(&[
            ("Show.S01E01.mkv", 10),
            ("Show.S01E02.mkv", 10),
            ("Show.S01E02.srt", 1),
        ]);
        let target = Target::Episode {
            season: Some(1),
            episode: 2,
        };
        let ids: Vec<i32> = listed_files(&pack, &target).iter().map(|f| f.id).collect();
        assert_eq!(ids, vec![1, 0, 2]);
    }
}
