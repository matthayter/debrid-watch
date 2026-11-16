//! Works out where each file of a magnet should be downloaded to, by
//! recognising TV episodes and movies from file and folder names.
//!
//! - TV:     `{prefix}/tv/{Show}/Season_{NN}/{file}`
//! - Movie:  `{prefix}/movies/{Title (Year)}/{file}`
//! - Other:  `{prefix}/other/{original path}`
//!
//! For TV and movies, files that aren't video or subtitles are excluded.

use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::alldebrid::MagnetFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tv,
    Movie,
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Tv => "TV",
            Kind::Movie => "Movie",
            Kind::Other => "Other",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Include { dest_dir: String, file_name: String },
    Exclude { reason: &'static str },
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub file: MagnetFile,
    pub decision: Decision,
}

#[derive(Debug, Clone)]
pub struct Classification {
    pub kind: Kind,
    /// Show name (TV) or movie title (movie) worked out from the names.
    /// `None` for other.
    pub detected_name: Option<String>,
    pub files: Vec<PlannedFile>,
}

/// Works out destinations for a magnet's files. `name_override` replaces
/// the detected show name or movie title; it's ignored for other files.
pub fn classify(
    magnet_name: &str,
    files: &[MagnetFile],
    prefix: &str,
    name_override: Option<&str>,
) -> Classification {
    let infos: Vec<FileInfo> = files.iter().map(FileInfo::new).collect();

    if infos
        .iter()
        .any(|i| i.file_type == FileType::Video && !i.sample && i.episode.is_some())
    {
        classify_tv(magnet_name, &infos, prefix, name_override)
    } else if infos
        .iter()
        .any(|i| i.file_type == FileType::Video && !i.sample)
    {
        classify_movie(magnet_name, &infos, prefix, name_override)
    } else {
        classify_other(&infos, prefix)
    }
}

/// Cleans a show name or movie title typed by the user, so it's safe to
/// use as one folder name. Returns `None` if nothing usable is left.
pub fn clean_name_override(input: &str) -> Option<String> {
    let name: String = input
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| if c == '/' || c == '\\' { '-' } else { c })
        .collect();
    // Collapse spaces and stop it being a hidden folder or `..`.
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = name.trim_start_matches('.').trim();
    (!name.is_empty()).then(|| name.to_string())
}

// ---------- Per-file facts ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileType {
    Video,
    Subtitle,
    Other,
}

const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "m4v", "mov", "wmv", "mpg", "mpeg", "ts", "m2ts", "webm",
];
const SUBTITLE_EXTS: &[&str] = &["srt", "ass", "ssa", "sub", "idx", "vtt", "sup"];

struct FileInfo<'a> {
    file: &'a MagnetFile,
    /// Folders above the file, outermost first.
    dirs: Vec<&'a str>,
    name: &'a str,
    file_type: FileType,
    sample: bool,
    episode: Option<Episode>,
}

struct Episode {
    season: u32,
    /// Show name taken from the same name the episode number was found in.
    show: Option<String>,
    /// Set when the episode number came from a folder rather than the file
    /// name; the folder name is then added to the file name.
    from_dir: Option<String>,
}

impl<'a> FileInfo<'a> {
    fn new(file: &'a MagnetFile) -> Self {
        let mut parts: Vec<&str> = file.path.split('/').collect();
        let name = parts.pop().unwrap_or_default();
        let (stem, ext) = split_ext(name);
        let file_type = if VIDEO_EXTS.contains(&ext.as_str()) {
            FileType::Video
        } else if SUBTITLE_EXTS.contains(&ext.as_str()) {
            FileType::Subtitle
        } else {
            FileType::Other
        };
        let sample = file_type == FileType::Video
            && (SAMPLE_RE.is_match(stem) || parts.iter().any(|d| d.eq_ignore_ascii_case("sample")));

        // Look for an episode number in the file name, then in each folder
        // from the innermost out.
        let episode = find_episode(stem)
            .map(|(season, before)| Episode {
                season,
                show: clean_show_name(before),
                from_dir: None,
            })
            .or_else(|| {
                parts.iter().rev().find_map(|dir| {
                    find_episode(dir).map(|(season, before)| Episode {
                        season,
                        show: clean_show_name(before),
                        from_dir: Some(dir.to_string()),
                    })
                })
            });

        Self {
            file,
            dirs: parts,
            name,
            file_type,
            sample,
            episode,
        }
    }
}

/// Splits `name.ext` into the stem and the lowercased extension.
fn split_ext(name: &str) -> (&str, String) {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, ext.to_ascii_lowercase()),
        _ => (name, String::new()),
    }
}

// ---------- Name parsing ----------

/// `S01E02`, `S01 E02`, `s1e2` and so on.
static SXXEXX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,2})\s?E\d{1,3}").unwrap());
/// `1x02`.
static NXNN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(\d{1,2})x\d{2,3}\b").unwrap());
/// A season with no episode, e.g. the `S02` in a season-pack folder name.
static SEASON_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:S\d{1,2}|Season[ ._]?\d{1,2})\b").unwrap());
static SAMPLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bsample\b").unwrap());
/// Leading `[Group]` tags and `www.site.org - ` prefixes.
static LEADING_TAGS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:(?:\[[^\]]*\]|www\.\S+\s+-)\s*)+").unwrap());
static TRAILING_YEAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.*?)[\s-]*\(?((?:19|20)\d{2})\)?$").unwrap());
static YEAR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(?\b((?:19|20)\d{2})\b\)?").unwrap());
/// Release details that follow a title; everything from here on is dropped.
static RELEASE_INFO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)[\[(]?\b(?:2160p|1080p|720p|576p|480p|4k|uhd|blu-?ray|bdrip|brrip|web-?dl|web-?rip|hdtv|dvdrip|hdrip|x26[45]|h\.?26[45]|hevc|remux|10bit)\b",
    )
    .unwrap()
});

/// Finds an episode number in `text`, returning the season and the text before it.
fn find_episode(text: &str) -> Option<(u32, &str)> {
    let caps = SXXEXX_RE.captures(text).or_else(|| NXNN_RE.captures(text))?;
    let season = caps[1].parse().ok()?;
    let start = caps.get(0)?.start();
    Some((season, &text[..start]))
}

/// Gets a show name from a folder or magnet name such as
/// `[Trix] Show Name S02 [1080p]` or `Show.Name.S01E01.720p`.
fn show_name_from(text: &str) -> Option<String> {
    let before = find_episode(text)
        .map(|(_, before)| before)
        .or_else(|| SEASON_RE.find(text).map(|m| &text[..m.start()]))?;
    clean_show_name(before)
}

/// Tidies the raw text before an episode number into a show name:
/// `[Group] Doctor.Who.2005.` becomes `Doctor Who (2005)`.
fn clean_show_name(raw: &str) -> Option<String> {
    let name = tidy(raw);
    let name = match TRAILING_YEAR_RE.captures(&name) {
        Some(c) if !c[1].is_empty() => format!("{} ({})", &c[1], &c[2]),
        _ => name,
    };
    (!name.is_empty()).then_some(name)
}

/// Drops leading `[tags]`, turns `.` and `_` into spaces, collapses spaces
/// and trims separators off the ends.
fn tidy(raw: &str) -> String {
    let raw = LEADING_TAGS_RE.replace(raw, "");
    let spaced: String = raw
        .chars()
        .map(|c| if c == '.' || c == '_' { ' ' } else { c })
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim_start_matches(|c: char| c.is_whitespace() || c == '-')
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '[' | '(' | '{'))
        .to_string()
}

/// Gets a movie folder name such as `Moana (2016)` from a release name such
/// as `Moana.2016.1080p.BluRay.x264-JYK.mkv`.
fn movie_title(release: &str) -> Option<String> {
    let (stem, ext) = split_ext(release);
    let release = if VIDEO_EXTS.contains(&ext.as_str()) {
        stem
    } else {
        release
    };
    let release = LEADING_TAGS_RE.replace(release, "");
    let cut = match RELEASE_INFO_RE.find(&release) {
        Some(m) => &release[..m.start()],
        None => &release[..],
    };

    // Use the last year, so `2001 A Space Odyssey 1968` keeps its title.
    if let Some(m) = YEAR_RE.captures_iter(cut).last() {
        let whole = m.get(0).unwrap();
        let title = tidy(&cut[..whole.start()]);
        if !title.is_empty() {
            return Some(format!("{title} ({})", &m[1]));
        }
    }
    let title = tidy(cut);
    (!title.is_empty()).then_some(title)
}

// ---------- Classifiers ----------

fn classify_tv(
    magnet_name: &str,
    infos: &[FileInfo],
    prefix: &str,
    name_override: Option<&str>,
) -> Classification {
    let episodes = || {
        infos
            .iter()
            .filter(|i| i.file_type == FileType::Video && !i.sample)
            .filter_map(|i| i.episode.as_ref().map(|e| (i, e)))
    };
    let seasons: BTreeSet<u32> = episodes().map(|(_, e)| e.season).collect();
    // The show name of the first episode. Also used when a file's own name
    // doesn't include the show name.
    let detected_show = episodes()
        .find_map(|(i, e)| e.show.clone().or_else(|| show_from_dirs(&i.dirs)))
        .or_else(|| show_name_from(magnet_name))
        .unwrap_or_else(|| tidy(magnet_name));
    let fallback_show = name_override.unwrap_or(&detected_show).to_string();

    // An override puts every episode under the one show name.
    let season_dir = |show: &str, season: u32| {
        let show = name_override.unwrap_or(show);
        format!("{prefix}/tv/{show}/Season_{season:02}")
    };

    let files = infos
        .iter()
        .map(|info| {
            let decision = match (info.file_type, &info.episode) {
                (FileType::Video, _) if info.sample => Decision::Exclude { reason: "sample" },
                (FileType::Video | FileType::Subtitle, Some(ep)) => {
                    let show = ep
                        .show
                        .clone()
                        .or_else(|| show_from_dirs(&info.dirs))
                        .unwrap_or_else(|| fallback_show.clone());
                    let file_name = match &ep.from_dir {
                        Some(dir) => format!("{dir}.{}", info.name),
                        None => info.name.to_string(),
                    };
                    Decision::Include {
                        dest_dir: season_dir(&show, ep.season),
                        file_name,
                    }
                }
                (FileType::Video, None) => Decision::Exclude {
                    reason: "not recognised as an episode",
                },
                (FileType::Subtitle, None) => match seasons.iter().next() {
                    Some(&season) if seasons.len() == 1 => Decision::Include {
                        dest_dir: season_dir(&fallback_show, season),
                        file_name: info.name.to_string(),
                    },
                    _ => Decision::Exclude {
                        reason: "subtitle not matched to an episode",
                    },
                },
                (FileType::Other, _) => Decision::Exclude {
                    reason: "not video or subtitle",
                },
            };
            planned(info, decision)
        })
        .collect();

    Classification {
        kind: Kind::Tv,
        detected_name: Some(detected_show),
        files,
    }
}

fn show_from_dirs(dirs: &[&str]) -> Option<String> {
    dirs.iter().rev().find_map(|d| show_name_from(d))
}

fn classify_movie(
    magnet_name: &str,
    infos: &[FileInfo],
    prefix: &str,
    name_override: Option<&str>,
) -> Classification {
    let largest_video = infos
        .iter()
        .filter(|i| i.file_type == FileType::Video && !i.sample)
        .max_by_key(|i| i.file.size);
    let detected_title = movie_title(magnet_name)
        .or_else(|| largest_video.and_then(|i| movie_title(i.name)))
        .unwrap_or_else(|| "Unknown movie".to_string());
    let title = name_override.unwrap_or(&detected_title);
    let dest_dir = format!("{prefix}/movies/{title}");

    let mut used_names = HashSet::new();
    let files = infos
        .iter()
        .map(|info| {
            let decision = match info.file_type {
                FileType::Video if info.sample => Decision::Exclude { reason: "sample" },
                FileType::Video | FileType::Subtitle => {
                    // Files are flattened into one folder, so avoid name clashes
                    // (e.g. `Subs/A/English.srt` and `Subs/B/English.srt`).
                    let mut file_name = info.name.to_string();
                    if used_names.contains(&file_name) {
                        if let Some(parent) = info.dirs.last() {
                            file_name = format!("{parent}.{file_name}");
                        }
                    }
                    used_names.insert(file_name.clone());
                    Decision::Include {
                        dest_dir: dest_dir.clone(),
                        file_name,
                    }
                }
                FileType::Other => Decision::Exclude {
                    reason: "not video or subtitle",
                },
            };
            planned(info, decision)
        })
        .collect();

    Classification {
        kind: Kind::Movie,
        detected_name: Some(detected_title),
        files,
    }
}

fn classify_other(infos: &[FileInfo], prefix: &str) -> Classification {
    let files = infos
        .iter()
        .map(|info| {
            let mut dest_dir = format!("{prefix}/other");
            for dir in &info.dirs {
                dest_dir.push('/');
                dest_dir.push_str(dir);
            }
            planned(
                info,
                Decision::Include {
                    dest_dir,
                    file_name: info.name.to_string(),
                },
            )
        })
        .collect();

    Classification {
        kind: Kind::Other,
        detected_name: None,
        files,
    }
}

fn planned(info: &FileInfo, decision: Decision) -> PlannedFile {
    PlannedFile {
        file: info.file.clone(),
        decision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "/downloads";

    fn files(paths: &[&str]) -> Vec<MagnetFile> {
        paths
            .iter()
            .enumerate()
            .map(|(i, p)| MagnetFile {
                path: p.to_string(),
                size: 1000 + i as u64,
                link: String::new(),
            })
            .collect()
    }

    /// Classifies with no name override.
    fn plan(magnet_name: &str, files: &[MagnetFile], prefix: &str) -> Classification {
        classify(magnet_name, files, prefix, None)
    }

    #[test]
    fn tv_name_override_applies_to_every_episode() {
        let paths = [
            "pack/Colin.from.Accounts.S01E01.mkv",
            "pack/Colin.From.Accounts.S01E02.mkv",
            "pack/English.srt",
        ];
        let c = classify("pack", &files(&paths), PREFIX, Some("Colin from Accounts"));
        assert_eq!(c.detected_name.as_deref(), Some("Colin from Accounts"));
        let dir = "/downloads/tv/Colin from Accounts/Season_01";
        assert_eq!(
            dests(&c),
            [
                format!("{dir}/Colin.from.Accounts.S01E01.mkv"),
                format!("{dir}/Colin.From.Accounts.S01E02.mkv"),
                format!("{dir}/English.srt"),
            ]
        );
    }

    #[test]
    fn movie_name_override() {
        let name = "www.example.org - Odd.Name.2026.1080p.mkv";
        let c = classify(name, &files(&[name]), PREFIX, Some("Proper Title (2026)"));
        assert_eq!(c.detected_name.as_deref(), Some("Odd Name (2026)"));
        assert_eq!(dests(&c), [format!("/downloads/movies/Proper Title (2026)/{name}")]);
    }

    #[test]
    fn other_ignores_name_override() {
        let c = classify("archive.zip", &files(&["archive.zip"]), PREFIX, Some("Ignored"));
        assert_eq!(c.detected_name, None);
        assert_eq!(dests(&c), ["/downloads/other/archive.zip"]);
    }

    #[test]
    fn cleans_name_overrides() {
        assert_eq!(clean_name_override("  Doctor   Who (2005) ").as_deref(), Some("Doctor Who (2005)"));
        assert_eq!(clean_name_override("AC/DC Live").as_deref(), Some("AC-DC Live"));
        assert_eq!(clean_name_override("../etc").as_deref(), Some("-etc"));
        assert_eq!(clean_name_override(".."), None);
        assert_eq!(clean_name_override("   "), None);
    }

    /// Each file as `dest_dir/file_name`, or `EXCLUDED: reason`.
    fn dests(c: &Classification) -> Vec<String> {
        c.files
            .iter()
            .map(|f| match &f.decision {
                Decision::Include {
                    dest_dir,
                    file_name,
                } => format!("{dest_dir}/{file_name}"),
                Decision::Exclude { reason } => format!("EXCLUDED: {reason}"),
            })
            .collect()
    }

    #[test]
    fn single_episode() {
        let name = "Get.Jiro.S01E02.Meat.Is.Murder.1080p.HEVC.x265-MeGusta[EZTVx.to].mkv";
        let c = plan(name, &files(&[name]), PREFIX);
        assert_eq!(c.kind, Kind::Tv);
        assert_eq!(dests(&c), [format!("/downloads/tv/Get Jiro/Season_01/{name}")]);
    }

    #[test]
    fn anime_season_pack_with_group_tag() {
        let folder = "[Trix] You and I Are Polar Opposites S02 v2 [WEBRip 1080p AV1] (Dual Audio, Multi Subs)";
        let ep1 = "[Trix] You and I Are Polar Opposites S02E01 v2 [WEBRip 1080p AV1] (Dual Audio, Multi Subs).mkv";
        let ep2 = "[Trix] You and I Are Polar Opposites S02E02 v2 [WEBRip 1080p AV1] (Dual Audio, Multi Subs).mkv";
        let c = plan(
            folder,
            &files(&[&format!("{folder}/{ep1}"), &format!("{folder}/{ep2}")]),
            PREFIX,
        );
        assert_eq!(c.kind, Kind::Tv);
        let dir = "/downloads/tv/You and I Are Polar Opposites/Season_02";
        assert_eq!(dests(&c), [format!("{dir}/{ep1}"), format!("{dir}/{ep2}")]);
    }

    #[test]
    fn season_pack_with_extras() {
        let c = plan(
            "Where the Mask Ends [1080p]",
            &files(&[
                "Where the Mask Ends [1080p]/Where the Mask Ends S01E01.mkv",
                "Where the Mask Ends [1080p]/Where the Mask Ends S01E02.mkv",
                "Where the Mask Ends [1080p]/English.srt",
                "Where the Mask Ends [1080p]/Behind the Scenes.mkv",
                "Where the Mask Ends [1080p]/Sample/sample.mkv",
                "Where the Mask Ends [1080p]/info.nfo",
            ]),
            PREFIX,
        );
        assert_eq!(c.kind, Kind::Tv);
        let dir = "/downloads/tv/Where the Mask Ends/Season_01";
        assert_eq!(
            dests(&c),
            [
                format!("{dir}/Where the Mask Ends S01E01.mkv"),
                format!("{dir}/Where the Mask Ends S01E02.mkv"),
                format!("{dir}/English.srt"),
                "EXCLUDED: not recognised as an episode".to_string(),
                "EXCLUDED: sample".to_string(),
                "EXCLUDED: not video or subtitle".to_string(),
            ]
        );
    }

    #[test]
    fn multi_season_pack() {
        let c = plan(
            "The.Office.US.S01-S02.720p",
            &files(&[
                "The.Office.US.S01-S02.720p/Season 1/The.Office.US.S01E01.720p.mkv",
                "The.Office.US.S01-S02.720p/Season 2/The.Office.US.S02E01.720p.mkv",
                "The.Office.US.S01-S02.720p/extra.srt",
            ]),
            PREFIX,
        );
        assert_eq!(
            dests(&c),
            [
                "/downloads/tv/The Office US/Season_01/The.Office.US.S01E01.720p.mkv",
                "/downloads/tv/The Office US/Season_02/The.Office.US.S02E01.720p.mkv",
                "EXCLUDED: subtitle not matched to an episode",
            ]
        );
    }

    #[test]
    fn show_with_year() {
        let name = "Doctor.Who.2005.S01E01.Rose.mkv";
        let c = plan(name, &files(&[name]), PREFIX);
        assert_eq!(dests(&c), [format!("/downloads/tv/Doctor Who (2005)/Season_01/{name}")]);
    }

    #[test]
    fn nxnn_format() {
        let name = "Firefly - 1x02 - The Train Job.avi";
        let c = plan(name, &files(&[name]), PREFIX);
        assert_eq!(dests(&c), [format!("/downloads/tv/Firefly/Season_01/{name}")]);
    }

    #[test]
    fn subtitles_in_per_episode_folders() {
        let c = plan(
            "Show.S01.1080p",
            &files(&[
                "Show.S01.1080p/Show.S01E01.1080p.mkv",
                "Show.S01.1080p/Show.S01E02.1080p.mkv",
                "Show.S01.1080p/Subs/Show.S01E01.1080p/2_English.srt",
                "Show.S01.1080p/Subs/Show.S01E02.1080p/2_English.srt",
            ]),
            PREFIX,
        );
        let dir = "/downloads/tv/Show/Season_01";
        assert_eq!(
            dests(&c),
            [
                format!("{dir}/Show.S01E01.1080p.mkv"),
                format!("{dir}/Show.S01E02.1080p.mkv"),
                format!("{dir}/Show.S01E01.1080p.2_English.srt"),
                format!("{dir}/Show.S01E02.1080p.2_English.srt"),
            ]
        );
    }

    #[test]
    fn episode_without_show_in_file_name() {
        let c = plan(
            "Some Show Season 3",
            &files(&["Some Show Season 3/S03E01.mkv"]),
            PREFIX,
        );
        assert_eq!(dests(&c), ["/downloads/tv/Some Show/Season_03/S03E01.mkv"]);
    }

    #[test]
    fn movie_with_year_sample_and_extras() {
        let c = plan(
            "Moana 2016 1080p BluRay x264 DTS-JYK",
            &files(&[
                "Moana 2016 1080p BluRay x264 DTS-JYK/Moana 2016 1080p BluRay x264 DTS-JYK.mkv",
                "Moana 2016 1080p BluRay x264 DTS-JYK/Moana 2016 1080p BluRay x264 DTS-JYK.mkv.jpg",
                "Moana 2016 1080p BluRay x264 DTS-JYK/Subs/English(SDH).srt",
                "Moana 2016 1080p BluRay x264 DTS-JYK/Subs/English.srt",
                "Moana 2016 1080p BluRay x264 DTS-JYK/moana.sample.mkv",
            ]),
            PREFIX,
        );
        assert_eq!(c.kind, Kind::Movie);
        let dir = "/downloads/movies/Moana (2016)";
        assert_eq!(
            dests(&c),
            [
                format!("{dir}/Moana 2016 1080p BluRay x264 DTS-JYK.mkv"),
                "EXCLUDED: not video or subtitle".to_string(),
                format!("{dir}/English(SDH).srt"),
                format!("{dir}/English.srt"),
                "EXCLUDED: sample".to_string(),
            ]
        );
    }

    #[test]
    fn movie_subtitle_name_clash() {
        let c = plan(
            "Film.2020.1080p",
            &files(&[
                "Film.2020.1080p/Film.2020.1080p.mkv",
                "Film.2020.1080p/Subs/Forced/English.srt",
                "Film.2020.1080p/Subs/Full/English.srt",
            ]),
            PREFIX,
        );
        let dir = "/downloads/movies/Film (2020)";
        assert_eq!(
            dests(&c),
            [
                format!("{dir}/Film.2020.1080p.mkv"),
                format!("{dir}/English.srt"),
                format!("{dir}/Full.English.srt"),
            ]
        );
    }

    #[test]
    fn movie_titles() {
        assert_eq!(movie_title("Moana 2016 1080p BluRay x264 DTS-JYK").unwrap(), "Moana (2016)");
        assert_eq!(movie_title("Blade.Runner.2049.2017.2160p.mkv").unwrap(), "Blade Runner 2049 (2017)");
        assert_eq!(movie_title("2001 A Space Odyssey (1968) [1080p]").unwrap(), "2001 A Space Odyssey (1968)");
        assert_eq!(movie_title("Charlotte's Web 2006 1080p").unwrap(), "Charlotte's Web (2006)");
        assert_eq!(movie_title("Some.Movie.1080p.WEB-DL.mkv").unwrap(), "Some Movie");
        assert_eq!(movie_title("1917.1080p.mkv").unwrap(), "1917");
        assert_eq!(
            movie_title("www.UIndex.org    -    Coyote vs Acme 2026 1080p AMZN WEB-DL").unwrap(),
            "Coyote vs Acme (2026)"
        );
        assert_eq!(
            show_name_from("www.UIndex.org    -    Peppa Pig-2004-S08 1080p").unwrap(),
            "Peppa Pig (2004)"
        );
        assert_eq!(movie_title("(500) Days of Summer (2009) 720p").unwrap(), "(500) Days of Summer (2009)");
    }

    #[test]
    fn non_media_goes_to_other_keeping_folders() {
        let c = plan(
            "ubuntu-24.10",
            &files(&["ubuntu-24.10/ubuntu-24.10-desktop-amd64.iso", "ubuntu-24.10/docs/README.txt"]),
            PREFIX,
        );
        assert_eq!(c.kind, Kind::Other);
        assert_eq!(
            dests(&c),
            [
                "/downloads/other/ubuntu-24.10/ubuntu-24.10-desktop-amd64.iso",
                "/downloads/other/ubuntu-24.10/docs/README.txt",
            ]
        );
    }

    #[test]
    fn single_file_other() {
        let c = plan("archive.zip", &files(&["archive.zip"]), PREFIX);
        assert_eq!(dests(&c), ["/downloads/other/archive.zip"]);
    }

    #[test]
    fn resolution_is_not_an_episode() {
        let name = "Clip.1920x1080.mp4";
        let c = plan(name, &files(&[name]), PREFIX);
        assert_eq!(c.kind, Kind::Movie);
    }
}
