//! The repositories tuicr has been opened in: one per-user list, most
//! recently opened first, behind the repo switcher (`<leader>r`, `:repos`).
//!
//! The list lives in `recent_repos.json` in tuicr's data directory, beside
//! `reviews/` (not inside it: that directory's `*.json` files are sessions
//! and a layout migration may rename it aside).
//!
//! There is no lock. Every change re-reads the file, applies one edit, and
//! replaces the file through a temp-file rename, so two running tuicr
//! instances can lose at most the edit that raced, never the file. A file
//! that is missing or unreadable is an empty list.
//!
//! Every function takes the file's path. Only `main.rs` asks for the real
//! one ([`default_path`], `None` under `cfg(test)`), so tests cannot reach
//! the user's list.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::storage::write_atomic;
use crate::error::Result;

/// How many repositories the list keeps. Older ones fall off the end.
pub const MAX_RECENT_REPOS: usize = 200;

const FILE_VERSION: &str = "1";

/// One opened repository: its canonical root and when it was last opened
/// (unix seconds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentRepo {
    pub path: PathBuf,
    #[serde(default)]
    pub last_opened: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RecentReposFile {
    #[serde(default)]
    version: String,
    /// Kept as raw values on the way in, so one entry this version cannot
    /// read does not cost the others.
    #[serde(default)]
    repos: Vec<serde_json::Value>,
}

/// Where the list lives for this user. `None` under `cfg(test)` and when
/// the platform has no data directory. Only `main.rs` calls this.
pub fn default_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    {
        directories::ProjectDirs::from("", "", "tuicr")
            .map(|dirs| dirs.data_dir().join("recent_repos.json"))
    }
}

/// The identity every comparison uses: the path with symlinks resolved, or
/// the path as given when it cannot be resolved (it no longer exists).
pub fn canonical(path: &Path) -> PathBuf {
    match fs::canonicalize(path) {
        Ok(resolved) => without_verbatim_prefix(resolved),
        Err(_) => path.to_path_buf(),
    }
}

/// On Windows `canonicalize` answers with a verbatim path
/// (`\\?\C:\code\repo`), which other programs take unevenly, `~` never
/// matches and nobody wants to read. A drive path is given back in its
/// usual spelling; anything else (a verbatim UNC share, every path on other
/// systems) is left alone.
fn without_verbatim_prefix(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|text| text.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path,
    }
}

/// The list, newest first, one entry per path. A missing, unreadable or
/// corrupt file is an empty list.
pub fn load(file: &Path) -> Vec<RecentRepo> {
    let Ok(text) = fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<RecentReposFile>(&text) else {
        return Vec::new();
    };
    let mut repos: Vec<RecentRepo> = parsed
        .repos
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect();
    normalize(&mut repos);
    repos
}

/// Note that `root` was opened at `now` (unix seconds): it moves to the
/// front, and the list is cut to [`MAX_RECENT_REPOS`].
pub fn record(file: &Path, root: &Path, now: i64) -> Result<()> {
    let root = canonical(root);
    let mut repos = load(file);
    repos.retain(|repo| repo.path != root);
    // A clock that stepped back must not file the repository just opened
    // below one opened earlier.
    let newest = repos.first().map_or(now, |repo| repo.last_opened);
    repos.insert(
        0,
        RecentRepo {
            path: root,
            last_opened: now.max(newest),
        },
    );
    normalize(&mut repos);
    write(file, &repos)
}

/// Drop `path` from the list. Stored paths are compared as they are, since
/// a path that has vanished cannot be resolved again.
pub fn remove(file: &Path, path: &Path) -> Result<()> {
    let resolved = canonical(path);
    let mut repos = load(file);
    let before = repos.len();
    repos.retain(|repo| repo.path != path && repo.path != resolved);
    if repos.len() == before {
        return Ok(());
    }
    write(file, &repos)
}

/// Newest first (ties keep their order), one entry per path, capped.
fn normalize(repos: &mut Vec<RecentRepo>) {
    repos.sort_by_key(|repo| std::cmp::Reverse(repo.last_opened));
    let mut seen = std::collections::HashSet::new();
    repos.retain(|repo| seen.insert(repo.path.clone()));
    repos.truncate(MAX_RECENT_REPOS);
}

fn write(file: &Path, repos: &[RecentRepo]) -> Result<()> {
    let contents = RecentReposFile {
        version: FILE_VERSION.to_string(),
        repos: repos
            .iter()
            // JSON has no way to hold a path that is not UTF-8.
            .filter(|repo| repo.path.to_str().is_some())
            .map(serde_json::to_value)
            .collect::<std::result::Result<_, _>>()?,
    };
    let bytes = serde_json::to_vec_pretty(&contents)?;
    write_atomic(file, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("state").join("recent_repos.json");
        (dir, file)
    }

    /// A directory that exists, so its canonical path is stable.
    fn repo_dir(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        fs::create_dir_all(&path).expect("create repo dir");
        canonical(&path)
    }

    fn paths(file: &Path) -> Vec<PathBuf> {
        load(file).into_iter().map(|repo| repo.path).collect()
    }

    #[test]
    fn should_return_an_empty_list_when_the_file_is_missing() {
        // given
        let (_dir, file) = store();

        // when
        let repos = load(&file);

        // then
        assert!(repos.is_empty());
        assert!(!file.exists(), "reading must not create the file");
    }

    #[test]
    fn should_treat_a_corrupt_file_as_empty_and_replace_it_on_the_next_record() {
        // given
        let (dir, file) = store();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{ this is not json").unwrap();
        let repo = repo_dir(dir.path(), "alpha");

        // when
        let before = load(&file);
        record(&file, &repo, 100).expect("record over a corrupt file");

        // then
        assert!(before.is_empty());
        assert_eq!(
            load(&file),
            vec![RecentRepo {
                path: repo,
                last_opened: 100
            }]
        );
    }

    #[test]
    fn should_list_the_most_recently_opened_repository_first() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let beta = repo_dir(dir.path(), "beta");
        let gamma = repo_dir(dir.path(), "gamma");

        // when
        record(&file, &alpha, 10).unwrap();
        record(&file, &beta, 20).unwrap();
        record(&file, &gamma, 30).unwrap();
        record(&file, &alpha, 40).unwrap();

        // then
        assert_eq!(paths(&file), vec![alpha, gamma, beta]);
        assert_eq!(load(&file)[0].last_opened, 40);
    }

    #[test]
    fn should_keep_the_repository_just_opened_first_when_two_share_a_second() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let beta = repo_dir(dir.path(), "beta");

        // when: both opened within the same second, then flipped back
        record(&file, &alpha, 50).unwrap();
        record(&file, &beta, 50).unwrap();
        let after_beta = paths(&file);
        record(&file, &alpha, 50).unwrap();

        // then
        assert_eq!(after_beta, vec![beta.clone(), alpha.clone()]);
        assert_eq!(paths(&file), vec![alpha, beta]);
    }

    #[test]
    fn should_keep_the_repository_just_opened_first_when_the_clock_steps_back() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let beta = repo_dir(dir.path(), "beta");
        record(&file, &alpha, 1_000).unwrap();

        // when
        record(&file, &beta, 900).unwrap();

        // then
        assert_eq!(paths(&file), vec![beta, alpha]);
    }

    #[test]
    fn should_store_one_entry_per_canonical_path() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let with_slash = PathBuf::from(format!("{}/", alpha.display()));
        let with_dot = alpha.join(".");

        // when
        record(&file, &alpha, 10).unwrap();
        record(&file, &with_slash, 20).unwrap();
        record(&file, &with_dot, 30).unwrap();

        // then
        assert_eq!(
            load(&file),
            vec![RecentRepo {
                path: alpha,
                last_opened: 30
            }]
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_store_one_entry_for_a_repository_reached_through_a_symlink() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let link = dir.path().join("link-to-alpha");
        std::os::unix::fs::symlink(&alpha, &link).expect("symlink");

        // when
        record(&file, &alpha, 10).unwrap();
        record(&file, &link, 20).unwrap();

        // then
        assert_eq!(paths(&file), vec![alpha]);
    }

    #[test]
    fn should_cap_the_list_and_drop_the_oldest() {
        // given
        let (dir, file) = store();
        let total = MAX_RECENT_REPOS + 5;

        // when
        for i in 0..total {
            // Paths that do not exist are stored as given, which keeps
            // this test from creating two hundred directories.
            record(&file, &dir.path().join(format!("repo-{i}")), i as i64).unwrap();
        }

        // then
        let repos = load(&file);
        assert_eq!(repos.len(), MAX_RECENT_REPOS);
        assert_eq!(
            repos[0].path,
            dir.path().join(format!("repo-{}", total - 1))
        );
        assert_eq!(
            repos.last().unwrap().path,
            dir.path().join("repo-5"),
            "the five oldest fell off"
        );
    }

    #[test]
    fn should_keep_entries_another_instance_added_since_the_last_read() {
        // given: this instance read the list, then another one wrote to it
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let beta = repo_dir(dir.path(), "beta");
        let gamma = repo_dir(dir.path(), "gamma");
        record(&file, &alpha, 10).unwrap();
        let stale_view = load(&file);
        record(&file, &beta, 20).unwrap();

        // when
        record(&file, &gamma, 30).unwrap();

        // then
        assert_eq!(stale_view.len(), 1);
        assert_eq!(paths(&file), vec![gamma, beta, alpha]);
    }

    #[test]
    fn should_remove_an_entry_and_leave_the_rest_in_order() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let beta = repo_dir(dir.path(), "beta");
        let gamma = repo_dir(dir.path(), "gamma");
        record(&file, &alpha, 10).unwrap();
        record(&file, &beta, 20).unwrap();
        record(&file, &gamma, 30).unwrap();

        // when
        remove(&file, &beta).unwrap();

        // then
        assert_eq!(paths(&file), vec![gamma, alpha]);
    }

    #[test]
    fn should_remove_an_entry_whose_directory_is_gone() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let gone = repo_dir(dir.path(), "gone");
        record(&file, &alpha, 10).unwrap();
        record(&file, &gone, 20).unwrap();
        fs::remove_dir_all(&gone).unwrap();

        // when
        remove(&file, &gone).unwrap();

        // then
        assert_eq!(paths(&file), vec![alpha]);
    }

    #[test]
    fn should_not_create_the_file_when_there_is_nothing_to_remove() {
        // given
        let (dir, file) = store();

        // when
        remove(&file, &dir.path().join("never-opened")).unwrap();

        // then
        assert!(!file.exists());
    }

    #[test]
    fn should_leave_no_temp_file_behind() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");
        let beta = repo_dir(dir.path(), "beta");

        // when
        record(&file, &alpha, 10).unwrap();
        record(&file, &beta, 20).unwrap();
        remove(&file, &alpha).unwrap();

        // then
        let names: Vec<String> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["recent_repos.json".to_string()]);
    }

    #[test]
    fn should_tolerate_unknown_fields_a_missing_version_and_unreadable_entries() {
        // given: a file a newer or older tuicr might have written
        let (_dir, file) = store();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            r#"{
                "future_field": true,
                "repos": [
                    { "path": "/work/alpha", "last_opened": 5, "pinned": true },
                    { "last_opened": 9 },
                    "not an object",
                    { "path": "/work/beta", "last_opened": 7 },
                    { "path": "/work/alpha", "last_opened": 1 }
                ]
            }"#,
        )
        .unwrap();

        // when
        let repos = load(&file);

        // then: readable entries survive, newest first, one per path
        assert_eq!(
            repos,
            vec![
                RecentRepo {
                    path: PathBuf::from("/work/beta"),
                    last_opened: 7
                },
                RecentRepo {
                    path: PathBuf::from("/work/alpha"),
                    last_opened: 5
                },
            ]
        );
    }

    #[test]
    fn should_write_the_documented_format() {
        // given
        let (dir, file) = store();
        let alpha = repo_dir(dir.path(), "alpha");

        // when
        record(&file, &alpha, 1_731_600_000).unwrap();

        // then
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(value["version"], "1");
        assert_eq!(value["repos"][0]["path"], alpha.to_str().unwrap());
        assert_eq!(value["repos"][0]["last_opened"], 1_731_600_000_i64);
    }

    #[test]
    fn should_spell_a_verbatim_drive_path_the_usual_way() {
        let plain = |path: &str| without_verbatim_prefix(PathBuf::from(path));
        assert_eq!(plain(r"\\?\C:\code\repo"), PathBuf::from(r"C:\code\repo"));
        // not a drive path: left alone
        assert_eq!(
            plain(r"\\?\UNC\server\share\repo"),
            PathBuf::from(r"\\?\UNC\server\share\repo")
        );
        assert_eq!(plain("/home/me/repo"), PathBuf::from("/home/me/repo"));
        assert_eq!(plain(r"C:\code\repo"), PathBuf::from(r"C:\code\repo"));
    }

    #[test]
    fn should_have_no_real_store_under_test() {
        assert_eq!(default_path(), None);
    }
}
