//! Read-only projection of the model Pi will resume for one person.
//!
//! The launch catalog owns this read. It selects the same transcript that it
//! passes to Pi, walks only that transcript's active leaf ancestry, and sends a
//! typed provider/model fact to clients. No client receives or reopens a path.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use chiefd_core::runtime::launch_catalog::PersonModel;
use serde::Deserialize;
use serde_json::Value;

const MAX_TRANSCRIPT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;
const MAX_TRANSCRIPT_NODES: usize = 20_000;
const MAX_MODEL_COMPONENT_BYTES: usize = 256;
const MODEL_CACHE_ENTRIES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FileStamp {
    Missing,
    Present {
        path: PathBuf,
        device: String,
        inode: String,
        size: u64,
        mode: u32,
        modified: SystemTime,
        changed_seconds: i64,
        changed_nanoseconds: i64,
    },
}

struct PreparedFile {
    stamp: FileStamp,
    opened: Option<crate::files::ObservedFile>,
    limit: u64,
}

struct PreparedSources {
    transcript: Option<PreparedFile>,
    global: PreparedFile,
    project: PreparedFile,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    transcript: Option<FileStamp>,
    global: FileStamp,
    project: FileStamp,
}

#[derive(Default)]
struct ModelCache {
    values: BTreeMap<CacheKey, PersonModel>,
    order: VecDeque<CacheKey>,
}

fn model_cache() -> &'static Mutex<ModelCache> {
    static CACHE: OnceLock<Mutex<ModelCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(ModelCache::default()))
}

/// Trusted roots and selected transcript for one model projection.
pub struct PersonModelSources<'a> {
    /// The exact epoch/mtime-selected transcript passed to Pi.
    pub transcript: Option<&'a Path>,
    /// The directory that must contain the selected transcript after symlinks resolve.
    pub sessions_root: &'a Path,
    /// Pi's global settings for this person.
    pub global_settings: &'a Path,
    /// Trusted root containing the resolved global settings target.
    pub global_root: &'a Path,
    /// Pi's project settings for this person.
    pub project_settings: &'a Path,
    /// Trusted root containing the resolved project settings target.
    pub project_root: &'a Path,
}

#[derive(Debug)]
struct Entry {
    parent: Option<String>,
    value: Value,
}

#[derive(Debug)]
enum PairRead {
    Absent,
    Pair(String, String),
    Invalid(Option<String>, Option<String>),
}

#[derive(Debug)]
enum JsonRead<T> {
    Missing,
    Value(T),
    Invalid,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiSettings {
    default_provider: Option<String>,
    default_model: Option<String>,
}

fn nonblank(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.to_owned();
    (!value.trim().is_empty() && value.len() <= MAX_MODEL_COMPONENT_BYTES).then_some(value)
}

fn bounded_component(value: Option<String>) -> Result<Option<String>, Option<String>> {
    match value {
        Some(value) if value.trim().is_empty() || value.len() > MAX_MODEL_COMPONENT_BYTES => {
            Err(Some(value))
        }
        value => Ok(value),
    }
}

fn pair(provider: Option<&Value>, model: Option<&Value>) -> PairRead {
    if [provider, model]
        .into_iter()
        .flatten()
        .any(|value| value.as_str().is_some_and(|value| value.len() > MAX_MODEL_COMPONENT_BYTES))
    {
        return PairRead::Invalid(None, None);
    }
    let provider = nonblank(provider);
    let model = nonblank(model);
    match (provider, model) {
        (Some(provider), Some(model)) => PairRead::Pair(provider, model),
        (None, None) => PairRead::Absent,
        (provider, model) => PairRead::Invalid(provider, model),
    }
}

fn prepared_file(path: &Path, root: &Path, limit: u64) -> Result<PreparedFile, ()> {
    if matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(PreparedFile { stamp: FileStamp::Missing, opened: None, limit });
    }
    let opened = crate::files::ObservedFile::open_contained(path, root).map_err(|_| ())?;
    let stamp = opened.as_ref().map_or(FileStamp::Missing, |file| {
        let metadata = file.metadata();
        FileStamp::Present {
            path: path.to_owned(),
            device: metadata.device.clone(),
            inode: metadata.inode.clone(),
            size: metadata.size,
            mode: metadata.mode,
            modified: metadata.modified,
            changed_seconds: metadata.changed_seconds,
            changed_nanoseconds: metadata.changed_nanoseconds,
        }
    });
    Ok(PreparedFile { stamp, opened, limit })
}

impl PreparedFile {
    fn bytes(mut self) -> Result<Option<Vec<u8>>, ()> {
        let Some(mut opened) = self.opened.take() else { return Ok(None) };
        if opened.metadata().size > self.limit {
            return Err(());
        }
        #[cfg(test)]
        {
            let path = match &self.stamp {
                FileStamp::Present { path, .. } => path,
                FileStamp::Missing => return Ok(None),
            };
            if let Ok(mut watched) = watched_reads().lock() {
                if watched.as_ref().is_some_and(|(target, _)| target == path) {
                    if let Some((_, reads)) = watched.as_mut() {
                        *reads += 1;
                    }
                }
            }
        }
        let size = opened.metadata().size;
        opened.read_range(0, usize::try_from(size).map_err(|_| ())?).map(Some).map_err(|_| ())
    }
}

fn prepare(sources: &PersonModelSources<'_>) -> Result<PreparedSources, ()> {
    Ok(PreparedSources {
        transcript: sources
            .transcript
            .map(|path| prepared_file(path, sources.sessions_root, MAX_TRANSCRIPT_BYTES))
            .transpose()?,
        global: prepared_file(sources.global_settings, sources.global_root, MAX_SETTINGS_BYTES)?,
        project: prepared_file(sources.project_settings, sources.project_root, MAX_SETTINGS_BYTES)?,
    })
}

fn read_settings(source: PreparedFile) -> JsonRead<PiSettings> {
    match source.bytes() {
        Ok(None) => JsonRead::Missing,
        Err(()) => JsonRead::Invalid,
        Ok(Some(bytes)) => match serde_json::from_slice(&bytes).ok() {
            Some(settings) => JsonRead::Value(settings),
            None => JsonRead::Invalid,
        },
    }
}

fn transcript_pair(source: PreparedFile) -> PairRead {
    let Ok(Some(bytes)) = source.bytes() else {
        return PairRead::Invalid(None, None);
    };
    let mut entries = BTreeMap::new();
    let mut leaf = None;
    let mut saw_header = false;
    for raw in
        bytes.split(|byte| *byte == b'\n').filter(|line| !line.iter().all(u8::is_ascii_whitespace))
    {
        let Ok(value) = serde_json::from_slice::<Value>(raw) else {
            return PairRead::Invalid(None, None);
        };
        let Some(kind) = value.get("type").and_then(Value::as_str) else {
            return PairRead::Invalid(None, None);
        };
        if kind == "session" {
            if saw_header || !entries.is_empty() {
                return PairRead::Invalid(None, None);
            }
            saw_header = true;
            continue;
        }
        if !saw_header || entries.len() >= MAX_TRANSCRIPT_NODES {
            return PairRead::Invalid(None, None);
        }
        let Some(id) =
            value.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()).map(str::to_owned)
        else {
            return PairRead::Invalid(None, None);
        };
        let parent = match value.get("parentId") {
            Some(Value::Null) => None,
            Some(Value::String(parent)) if !parent.is_empty() => Some(parent.clone()),
            _ => return PairRead::Invalid(None, None),
        };
        if entries.insert(id.clone(), Entry { parent, value }).is_some() {
            return PairRead::Invalid(None, None);
        }
        leaf = Some(id);
    }
    if !saw_header {
        return PairRead::Invalid(None, None);
    }
    let Some(mut cursor) = leaf else {
        return PairRead::Absent;
    };
    let mut ancestry = Vec::new();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(cursor.clone()) || ancestry.len() >= MAX_TRANSCRIPT_NODES {
            return PairRead::Invalid(None, None);
        }
        let Some(entry) = entries.get(&cursor) else {
            return PairRead::Invalid(None, None);
        };
        ancestry.push(entry);
        let Some(parent) = entry.parent.as_ref() else { break };
        cursor = parent.clone();
    }
    let mut selected = PairRead::Absent;
    for entry in ancestry.into_iter().rev() {
        match entry.value.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                selected = pair(entry.value.get("provider"), entry.value.get("modelId"));
            }
            Some("message")
                if entry.value.pointer("/message/role").and_then(Value::as_str)
                    == Some("assistant") =>
            {
                selected = pair(
                    entry.value.pointer("/message/provider"),
                    entry.value.pointer("/message/model"),
                );
            }
            _ => {}
        }
        if matches!(selected, PairRead::Invalid(_, _)) {
            return selected;
        }
    }
    selected
}

fn settings_pair(global: PreparedFile, project: PreparedFile) -> PersonModel {
    let global = read_settings(global);
    let project = read_settings(project);
    if matches!(global, JsonRead::Invalid) || matches!(project, JsonRead::Invalid) {
        return PersonModel::unavailable(None, None);
    }
    let (mut provider, mut model) = match global {
        JsonRead::Value(settings) => (settings.default_provider, settings.default_model),
        JsonRead::Missing | JsonRead::Invalid => (None, None),
    };
    if let JsonRead::Value(settings) = project {
        if settings.default_provider.is_some() {
            provider = settings.default_provider;
        }
        if settings.default_model.is_some() {
            model = settings.default_model;
        }
    }
    let provider = match bounded_component(provider) {
        Ok(value) => value,
        Err(_) => return PersonModel::unavailable(None, None),
    };
    let model = match bounded_component(model) {
        Ok(value) => value,
        Err(_) => return PersonModel::unavailable(provider, None),
    };
    match (provider, model) {
        (Some(provider), Some(model)) => PersonModel::selected(provider, model),
        (None, None) => PersonModel::pi_default(),
        (provider, model) => PersonModel::unavailable(provider, model),
    }
}

/// Resolve one person's current model without changing any source inode.
#[must_use]
pub fn resolve_person_model(sources: &PersonModelSources<'_>) -> PersonModel {
    let Ok(prepared) = prepare(sources) else {
        return PersonModel::unavailable(None, None);
    };
    let key = CacheKey {
        transcript: prepared.transcript.as_ref().map(|source| source.stamp.clone()),
        global: prepared.global.stamp.clone(),
        project: prepared.project.stamp.clone(),
    };
    if let Some(cached) =
        model_cache().lock().ok().and_then(|cache| cache.values.get(&key).cloned())
    {
        return cached;
    }
    let resolved = match prepared.transcript.map(transcript_pair) {
        Some(PairRead::Pair(provider, model)) => PersonModel::selected(provider, model),
        Some(PairRead::Invalid(provider, model)) => PersonModel::unavailable(provider, model),
        Some(PairRead::Absent) | None => settings_pair(prepared.global, prepared.project),
    };
    if let Ok(mut cache) = model_cache().lock() {
        if !cache.values.contains_key(&key) {
            while cache.order.len() >= MODEL_CACHE_ENTRIES {
                if let Some(evicted) = cache.order.pop_front() {
                    cache.values.remove(&evicted);
                }
            }
            cache.order.push_back(key.clone());
            cache.values.insert(key, resolved.clone());
        }
    }
    resolved
}

#[cfg(test)]
fn watched_reads() -> &'static Mutex<Option<(PathBuf, usize)>> {
    static WATCHED: OnceLock<Mutex<Option<(PathBuf, usize)>>> = OnceLock::new();
    WATCHED.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{symlink, MetadataExt as _, PermissionsExt as _};

    use super::*;

    fn write(path: &Path, contents: &str) {
        crate::files::publish_atomically(path, contents, 0o644).expect("fixture write");
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        sessions: PathBuf,
        transcript: PathBuf,
        global_root: PathBuf,
        global: PathBuf,
        project_root: PathBuf,
        project: PathBuf,
    }

    impl Fixture {
        fn new(transcript: &str) -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            let sessions = dir.path().join("home/sessions");
            let global_root = dir.path().join("operator");
            let project_root = dir.path().join("home");
            fs::create_dir_all(&sessions).expect("sessions");
            fs::create_dir_all(&global_root).expect("global root");
            fs::create_dir_all(project_root.join(".pi")).expect("project root");
            let transcript_path = sessions.join("active.jsonl");
            write(&transcript_path, transcript);
            let global = global_root.join("settings.json");
            let project = project_root.join(".pi/settings.json");
            Self {
                _dir: dir,
                sessions,
                transcript: transcript_path,
                global_root,
                global,
                project_root,
                project,
            }
        }

        fn sources(&self) -> PersonModelSources<'_> {
            PersonModelSources {
                transcript: Some(&self.transcript),
                sessions_root: &self.sessions,
                global_settings: &self.global,
                global_root: &self.global_root,
                project_settings: &self.project,
                project_root: &self.project_root,
            }
        }
    }

    const HEADER: &str = r#"{"type":"session","version":3,"id":"s"}"#;

    #[test]
    fn active_leaf_applies_assistant_and_model_change_but_ignores_dead_branch() {
        let body = format!(
            "{HEADER}\n{}\n{}\n{}\n{}\n",
            r#"{"type":"message","id":"root","parentId":null,"message":{"role":"assistant","provider":"anthropic","model":"claude"}}"#,
            r#"{"type":"model_change","id":"dead","parentId":"root","provider":"dead-provider","modelId":"dead-model"}"#,
            r#"{"type":"message","id":"active","parentId":"root","message":{"role":"assistant","provider":"openai","model":"gpt-5"}}"#,
            r#"{"type":"model_change","id":"leaf","parentId":"active","provider":"openai","modelId":"gpt-5.6"}"#,
        );
        let fixture = Fixture::new(&body);
        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::selected("openai".to_owned(), "gpt-5.6".to_owned())
        );
    }

    #[test]
    fn a_well_formed_transcript_without_a_pair_merges_global_then_project_settings() {
        let fixture = Fixture::new(&format!(
            "{HEADER}\n{}\n",
            r#"{"type":"message","id":"u","parentId":null,"message":{"role":"user","content":"hi"}}"#
        ));
        write(&fixture.global, r#"{"defaultProvider":"anthropic","defaultModel":"old"}"#);
        write(&fixture.project, r#"{"defaultModel":"claude-sonnet"}"#);
        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::selected("anthropic".to_owned(), "claude-sonnet".to_owned())
        );
    }

    #[test]
    fn no_pair_is_pi_default_but_an_incomplete_pair_is_unavailable() {
        let fixture = Fixture::new(&format!("{HEADER}\n"));
        assert_eq!(resolve_person_model(&fixture.sources()), PersonModel::pi_default());
        write(&fixture.global, r#"{"defaultProvider":"openai"}"#);
        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::unavailable(Some("openai".to_owned()), None)
        );
    }

    #[test]
    fn malformed_missing_parent_cycle_and_over_budget_sources_are_unavailable() {
        let malformed = Fixture::new(&format!("{HEADER}\nnot-json\n"));
        assert_eq!(
            resolve_person_model(&malformed.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );

        let missing = Fixture::new(&format!(
            "{HEADER}\n{}\n",
            r#"{"type":"model_change","id":"leaf","parentId":"absent","provider":"p","modelId":"m"}"#
        ));
        assert_eq!(
            resolve_person_model(&missing.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );

        let cycle = Fixture::new(&format!(
            "{HEADER}\n{}\n{}\n",
            r#"{"type":"message","id":"a","parentId":"b","message":{"role":"user"}}"#,
            r#"{"type":"message","id":"b","parentId":"a","message":{"role":"user"}}"#
        ));
        assert_eq!(
            resolve_person_model(&cycle.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );

        let oversized =
            Fixture::new(&format!("{HEADER}\n{}", " ".repeat(MAX_TRANSCRIPT_BYTES as usize)));
        assert_eq!(
            resolve_person_model(&oversized.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );
    }

    #[test]
    fn transcript_and_settings_symlinks_must_resolve_inside_their_trusted_roots() {
        let fixture = Fixture::new(&format!("{HEADER}\n"));
        let outside = fixture._dir.path().join("outside.jsonl");
        write(
            &outside,
            &format!(
                "{HEADER}\n{}\n",
                r#"{"type":"model_change","id":"x","parentId":null,"provider":"p","modelId":"m"}"#
            ),
        );
        crate::files::remove_file_if_exists(&fixture.transcript)
            .expect("remove fixture transcript");
        symlink(&outside, &fixture.transcript).expect("outside link");
        assert_eq!(
            resolve_person_model(&fixture.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );

        let global_escape = Fixture::new(&format!("{HEADER}\n"));
        let outside_global = global_escape._dir.path().join("outside-global.json");
        write(&outside_global, r#"{"defaultProvider":"escaped","defaultModel":"escaped-model"}"#);
        symlink(&outside_global, &global_escape.global).expect("outside global settings link");
        assert_eq!(
            resolve_person_model(&global_escape.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );

        let project_escape = Fixture::new(&format!("{HEADER}\n"));
        let outside_project = project_escape._dir.path().join("outside-project.json");
        write(&outside_project, r#"{"defaultProvider":"escaped","defaultModel":"escaped-model"}"#);
        symlink(&outside_project, &project_escape.project).expect("outside project settings link");
        assert_eq!(
            resolve_person_model(&project_escape.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );
    }

    #[test]
    fn resolution_is_read_only_and_preserves_source_inodes() {
        let fixture = Fixture::new(&format!(
            "{HEADER}\n{}\n",
            r#"{"type":"model_change","id":"x","parentId":null,"provider":"openai","modelId":"gpt-5.6"}"#
        ));
        write(&fixture.global, r#"{"defaultProvider":"unused","defaultModel":"unused"}"#);
        let transcript_before = fs::metadata(&fixture.transcript).expect("transcript metadata");
        let settings_before = fs::metadata(&fixture.global).expect("settings metadata");
        fs::set_permissions(&fixture.transcript, fs::Permissions::from_mode(0o444))
            .expect("read-only transcript");
        fs::set_permissions(&fixture.global, fs::Permissions::from_mode(0o444))
            .expect("read-only settings");

        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::selected("openai".to_owned(), "gpt-5.6".to_owned())
        );

        let transcript_after = fs::metadata(&fixture.transcript).expect("transcript metadata");
        let settings_after = fs::metadata(&fixture.global).expect("settings metadata");
        assert_eq!(transcript_before.ino(), transcript_after.ino());
        assert_eq!(transcript_before.len(), transcript_after.len());
        assert_eq!(settings_before.ino(), settings_after.ino());
        assert_eq!(settings_before.len(), settings_after.len());
    }

    #[test]
    fn unchanged_catalog_rounds_parse_no_transcript_or_settings_bytes_twice() {
        let fixture = Fixture::new(&format!(
            "{HEADER}\n{}\n",
            r#"{"type":"model_change","id":"x","parentId":null,"provider":"zipbox/deepseek","modelId":"deepseek-v4-flash-0731"}"#
        ));
        *watched_reads().lock().expect("watch lock") =
            Some((fs::canonicalize(&fixture.transcript).expect("canonical transcript"), 0));
        let first = resolve_person_model(&fixture.sources());
        let after_first = watched_reads().lock().expect("watch lock").as_ref().expect("watch").1;
        let second = resolve_person_model(&fixture.sources());
        let after_second = watched_reads().lock().expect("watch lock").as_ref().expect("watch").1;
        assert_eq!(first, second);
        assert_eq!(after_first, 1, "the selected transcript is read once");
        assert_eq!(
            after_second, after_first,
            "an unchanged catalog round validates metadata but reads and parses zero source bytes"
        );
        write(
            &fixture.transcript,
            &format!(
                "{HEADER}\n{}\n",
                r#"{"type":"model_change","id":"y","parentId":null,"provider":"openai","modelId":"gpt-5.6"}"#
            ),
        );
        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::selected("openai".to_owned(), "gpt-5.6".to_owned())
        );
        assert_eq!(
            watched_reads().lock().expect("watch lock").as_ref().expect("watch").1,
            after_first + 1,
            "a replaced transcript invalidates the cache and is read once"
        );
    }

    #[test]
    fn same_size_in_place_change_with_restored_mtime_invalidates_the_cache() {
        let first = format!(
            "{HEADER}\n{}\n",
            r#"{"type":"model_change","id":"x","parentId":null,"provider":"openai","modelId":"gpt-5.6"}"#
        );
        let second = format!(
            "{HEADER}\n{}\n",
            r#"{"type":"model_change","id":"x","parentId":null,"provider":"zipbox","modelId":"llama31"}"#
        );
        assert_eq!(first.len(), second.len());
        let fixture = Fixture::new(&first);
        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::selected("openai".into(), "gpt-5.6".into())
        );
        let modified =
            fs::metadata(&fixture.transcript).expect("metadata").modified().expect("mtime");
        let changed = fs::metadata(&fixture.transcript).expect("metadata").ctime();
        #[allow(clippy::disallowed_methods)]
        fs::write(&fixture.transcript, second).expect("same-inode rewrite");
        #[allow(clippy::disallowed_types)]
        let file = fs::File::open(&fixture.transcript).expect("open for timestamp restore");
        file.set_times(std::fs::FileTimes::new().set_modified(modified)).expect("restore mtime");
        while std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("wall clock")
            .as_secs()
            <= u64::try_from(changed).expect("non-negative ctime")
        {
            std::thread::yield_now();
        }
        fs::set_permissions(&fixture.transcript, fs::Permissions::from_mode(0o600))
            .expect("change inode time");
        fs::set_permissions(&fixture.transcript, fs::Permissions::from_mode(0o644))
            .expect("restore mode");

        assert_eq!(
            resolve_person_model(&fixture.sources()),
            PersonModel::selected("zipbox".into(), "llama31".into()),
            "ctime identifies an in-place rewrite even when size and mtime are unchanged"
        );
    }

    #[test]
    fn provider_and_model_strings_are_bounded_before_the_card_argv() {
        let oversized = "x".repeat(MAX_MODEL_COMPONENT_BYTES + 1);
        let transcript = Fixture::new(&format!(
            "{HEADER}\n{}\n",
            serde_json::json!({
                "type": "model_change",
                "id": "x",
                "parentId": null,
                "provider": oversized,
                "modelId": "gpt-5.6"
            })
        ));
        assert_eq!(
            resolve_person_model(&transcript.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );

        let settings = Fixture::new(HEADER);
        write(
            &settings.global,
            &serde_json::json!({
                "defaultProvider": "openai",
                "defaultModel": "x".repeat(MAX_MODEL_COMPONENT_BYTES + 1)
            })
            .to_string(),
        );
        assert_eq!(
            resolve_person_model(&settings.sources()).state,
            chiefd_core::runtime::launch_catalog::PersonModelState::Unavailable
        );
    }
}
