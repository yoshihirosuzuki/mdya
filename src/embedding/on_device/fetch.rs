//! Fetching the files of a model from Hugging Face into the embedding model
//! cache directory.
//!
//! The files of a model at a revision go to
//! `<cache>/<owner>/<name>/<revision>/<file>`, where `<file>` may include a
//! subdirectory of the repository. A file that is already there is used
//! without asking Hugging Face. A file being fetched is written to a partial
//! file next to it (`.<file name>.<random>.part`), and moved to its name
//! only when it is complete and only if no file is there yet, so that a reader
//! never sees half a file. Processes that share the cache and fetch the same
//! file at once each fetch it; the content at a revision is the same, so
//! whichever finishes first stays.
//!
//! A partial file is removed when its fetch fails, panics, or is abandoned.
//! One left by a process that was killed is removed by a later fetch into the
//! same directory. A fetch holds a lock on its partial file while it runs,
//! which the system releases when the process ends, so a partial file is
//! removed only when no fetch holds it and nothing has written to it for an
//! hour.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::StatusCode;
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;

use crate::types::BoxError;

/// Where models are fetched from.
pub(super) const HUGGING_FACE: &str = "https://huggingface.co";

/// The end of the name of a partial file.
const PARTIAL_SUFFIX: &str = ".part";

/// How long nothing must have written to an unlocked partial file before it is
/// removed. A new partial file is unlocked for a moment between its creation
/// and its lock, and the age keeps it through that moment.
const STALE_PARTIAL_AGE: Duration = Duration::from_secs(60 * 60);

/// How much of the body of an error answer is read, in bytes.
const ERROR_BODY_MAX_BYTES: usize = 16 * 1024;

/// Fetches the files of models from Hugging Face.
pub(super) struct Hub {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Hub {
    /// A hub at `base` (Hugging Face itself, or a stand-in in tests). `token`
    /// is sent with every request, for models whose use needs permission.
    pub(super) fn connect(base: &str, token: Option<String>) -> Result<Self, BoxError> {
        // The proxy is not taken from environment variables: mdya reads no
        // environment variable for where to connect. Redirects are followed,
        // because Hugging Face answers with a redirect to where the file is
        // served; reqwest drops the token on a redirect to another host.
        let client = reqwest::Client::builder().no_proxy().build()?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_owned(),
            token,
        })
    }

    /// Makes sure that `files` of `repository` (`owner/name`) at `revision`
    /// are in `cache_dir`, fetching the ones that are not, and returns the
    /// directory they are in. A file may be in a subdirectory of the
    /// repository (`2_Dense/model.safetensors`), and goes in the same
    /// subdirectory. Creates the directories it needs.
    pub(super) async fn fetch(
        &self,
        cache_dir: &Path,
        repository: &str,
        revision: &str,
        files: &[&str],
    ) -> Result<PathBuf, BoxError> {
        let mut dir = cache_dir.to_owned();
        dir.extend(repository.split('/'));
        dir.push(revision);
        let paths: Vec<PathBuf> = files.iter().map(|file| dir.join(file)).collect();
        let mut dirs: Vec<PathBuf> = paths
            .iter()
            .map(|path| place_of(path).0.to_owned())
            .chain([dir.clone()])
            .collect();
        dirs.sort();
        dirs.dedup();
        for dir in &dirs {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|error| cache_error(dir, error))?;
        }
        // Only a failure to hand the work to a thread (the runtime shutting
        // down) can be returned, and the cleanup is not needed for the fetch.
        let _ = blocking(move || dirs.iter().for_each(|dir| remove_stale_partials(dir))).await;
        for (file, path) in files.iter().zip(&paths) {
            if tokio::fs::try_exists(path)
                .await
                .map_err(|error| cache_error(path, error))?
            {
                continue;
            }
            let url = format!("{}/{repository}/resolve/{revision}/{file}", self.base);
            self.download(&url, path).await?;
        }
        Ok(dir)
    }

    /// Downloads `url` to a partial file next to `path`, then moves it to
    /// `path` unless a file is already there.
    async fn download(&self, url: &str, path: &Path) -> Result<(), BoxError> {
        let mut response = self.request(url).await?;
        let expected = response.content_length();
        let (dir, name) = place_of(path);
        let (out, partial) = new_partial(dir, name).await?;
        let mut out = tokio::fs::File::from_std(out);
        let mut written: u64 = 0;
        while let Some(piece) = response.chunk().await? {
            out.write_all(&piece)
                .await
                .map_err(|error| cache_error(&partial, error))?;
            written += piece.len() as u64;
        }
        out.flush()
            .await
            .map_err(|error| cache_error(&partial, error))?;
        let out = out.into_std().await;
        if let Some(expected) = expected
            && written != expected
        {
            return Err(Box::new(IncompleteFile {
                url: url.to_owned(),
                expected,
                written,
            }));
        }
        persist(out, partial, path.to_owned()).await
    }

    async fn request(&self, url: &str) -> Result<reqwest::Response, BoxError> {
        let mut request = self.client.get(url);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(error_answer(url, response).await);
        }
        Ok(response)
    }
}

/// The directory of `path`, a file of a model, and the name of the file.
#[expect(clippy::expect_used)]
fn place_of(path: &Path) -> (&Path, &str) {
    let dir = path
        .parent()
        .expect("a file of a model is in the model's directory");
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .expect("the files of the models have names in UTF-8");
    (dir, name)
}

/// Creates a partial file for `file` in `dir`, locked for as long as the
/// returned file is open, so that no other fetch removes it. The returned path
/// removes the file when it is dropped without being persisted: when the
/// download fails, panics, or is abandoned.
async fn new_partial(dir: &Path, file: &str) -> Result<(std::fs::File, TempPath), BoxError> {
    let dir = dir.to_owned();
    let prefix = format!(".{file}.");
    blocking(move || {
        let (out, partial) = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(PARTIAL_SUFFIX)
            .tempfile_in(&dir)
            .map_err(|error| cache_error(&dir, error))?
            .into_parts();
        // The download does not need the lock. On a file system without
        // locks, this fails, and the cleanup cannot lock the file either, so
        // it keeps it.
        let _ = out.lock();
        Ok((out, partial))
    })
    .await?
}

/// Moves `partial` to `path` unless a file is already there, then closes
/// `out`, the open partial file. A file there was fetched by another process
/// at the same revision, so its content is the same; it stays, and `partial`
/// is removed.
async fn persist(out: std::fs::File, partial: TempPath, path: PathBuf) -> Result<(), BoxError> {
    blocking(move || {
        // Moved while still open and locked, so that no cleanup can take it
        // for a stopped fetch's file in between. tempfile opens the file with
        // std's default sharing, which on Windows includes FILE_SHARE_DELETE,
        // so that it can be moved while open.
        let persisted = match partial.persist_noclobber(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(cache_error(&path, error.error)),
        };
        drop(out);
        persisted
    })
    .await?
}

/// Runs `work` on tokio's blocking threads, so that file operations do not
/// hold up other asynchronous work, and waits for it. A panic in `work`
/// reaches the caller as a panic.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, BoxError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => Ok(value),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(Box::new(error)),
    }
}

/// Removes the partial files in `dir` of fetches that stopped: no fetch holds
/// them, and nothing has written to them for `STALE_PARTIAL_AGE`. A failure
/// to read or remove one is ignored: it only takes room, and no fetch uses it.
fn remove_stale_partials(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !(name.starts_with('.') && name.ends_with(PARTIAL_SUFFIX)) {
            continue;
        }
        remove_if_stale(&entry.path());
    }
}

/// Removes the partial file at `path` if no fetch holds it and nothing has
/// written to it for `STALE_PARTIAL_AGE`. The lock is taken first and held
/// until the file is removed, so that no fetch can take it over in between.
/// When the file cannot be locked (another fetch holds it, or the file system
/// has no locks), it is kept.
fn remove_if_stale(path: &Path) {
    // Opened for writing: whether a file opened only for reading can be locked
    // is unspecified.
    let Ok(file) = std::fs::File::options().write(true).open(path) else {
        return;
    };
    if file.try_lock().is_err() {
        return;
    }
    let stale = file
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= STALE_PARTIAL_AGE);
    if stale {
        let _ = std::fs::remove_file(path);
    }
    // The lock is released here, when `file` is closed.
}

/// Hugging Face answered with an error status. Its message tells why, for
/// example that the model needs permission and a token.
#[derive(Debug, thiserror::Error)]
#[error("Hugging Face answered {status} for {url}: {message}")]
struct ErrorAnswer {
    url: String,
    status: StatusCode,
    message: String,
}

#[derive(Debug, thiserror::Error)]
#[error("the download of {url} ended after {written} of {expected} bytes")]
struct IncompleteFile {
    url: String,
    expected: u64,
    written: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot write {path} in the embedding model cache directory")]
struct CacheError {
    path: PathBuf,
    source: std::io::Error,
}

fn cache_error(path: &Path, source: std::io::Error) -> BoxError {
    Box::new(CacheError {
        path: path.to_owned(),
        source,
    })
}

/// The error for an error answer. Hugging Face puts its message in the
/// `x-error-message` header; without it, the beginning of the body is used.
async fn error_answer(url: &str, mut response: reqwest::Response) -> BoxError {
    let status = response.status();
    let header = response
        .headers()
        .get("x-error-message")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let message = match header {
        Some(message) => message,
        None => {
            let mut body = Vec::new();
            while body.len() < ERROR_BODY_MAX_BYTES {
                let Ok(Some(piece)) = response.chunk().await else {
                    break;
                };
                let room = ERROR_BODY_MAX_BYTES - body.len();
                body.extend_from_slice(&piece[..piece.len().min(room)]);
            }
            String::from_utf8_lossy(&body).into_owned()
        }
    };
    Box::new(ErrorAnswer {
        url: url.to_owned(),
        status,
        message,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use std::io::Write as _;
    use std::time::SystemTime;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    /// A request the stand-in received: its path and its `authorization`
    /// header.
    #[derive(Clone, Debug, PartialEq)]
    struct Received {
        path: String,
        authorization: Option<String>,
    }

    /// Stands in for Hugging Face on a local port: answers each request with
    /// the whole HTTP response planned for its path, and records the
    /// requests. It takes `batch` requests before answering any of them, so
    /// that a test can make requests overlap.
    struct StandIn {
        base: String,
        received: Arc<Mutex<Vec<Received>>>,
    }

    impl StandIn {
        async fn start(answers: HashMap<String, String>) -> Self {
            Self::start_in_batches(answers, 1).await
        }

        async fn start_in_batches(answers: HashMap<String, String>, batch: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let received = Arc::new(Mutex::new(Vec::new()));
            let record = received.clone();
            tokio::spawn(async move {
                loop {
                    let mut waiting = Vec::new();
                    while waiting.len() < batch {
                        let (mut socket, _) = listener.accept().await.unwrap();
                        let request = read_head(&mut socket).await;
                        record.lock().unwrap().push(request.clone());
                        waiting.push((socket, request.path));
                    }
                    for (mut socket, path) in waiting {
                        let answer = answers
                            .get(&path)
                            .cloned()
                            .unwrap_or_else(|| answer(404, &[], "not planned"));
                        socket.write_all(answer.as_bytes()).await.unwrap();
                        socket.shutdown().await.unwrap();
                    }
                }
            });
            Self { base, received }
        }

        fn received(&self) -> Vec<Received> {
            self.received.lock().unwrap().clone()
        }
    }

    /// Reads the head of a request (GET requests have no body).
    async fn read_head(socket: &mut TcpStream) -> Received {
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        let head = String::from_utf8_lossy(&request).into_owned();
        let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
        let authorization = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        });
        Received {
            path,
            authorization,
        }
    }

    /// A whole HTTP response with `status`, extra `headers`, and `body`.
    fn answer(status: u16, headers: &[(&str, &str)], body: &str) -> String {
        let extra: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        format!(
            "HTTP/1.1 {status} Planned\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    const REVISION: &str = "0123abcd";

    fn path_of(file: &str) -> String {
        format!("/owner/model/resolve/{REVISION}/{file}")
    }

    fn files_answered(files: &[(&str, &str)]) -> HashMap<String, String> {
        files
            .iter()
            .map(|(file, body)| (path_of(file), answer(200, &[], body)))
            .collect()
    }

    fn model_dir(cache: &Path) -> PathBuf {
        cache.join("owner").join("model").join(REVISION)
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Writes partial files in `dir`; the `stale` ones were last written to
    /// longer ago than `STALE_PARTIAL_AGE`.
    fn place_partials(dir: &Path, stale: &[&str], running: &[&str]) {
        std::fs::create_dir_all(dir).unwrap();
        let long_ago = SystemTime::now() - STALE_PARTIAL_AGE - Duration::from_secs(60);
        for name in stale.iter().chain(running) {
            std::fs::write(dir.join(name), "partial").unwrap();
        }
        for name in stale {
            std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(long_ago)
                .unwrap();
        }
    }

    #[tokio::test]
    async fn fetches_the_files_into_a_directory_per_model_and_revision_creating_it() {
        let stand_in = StandIn::start(files_answered(&[
            ("config.json", "{}"),
            ("model.safetensors", "weights"),
        ]))
        .await;
        let cache = tempfile::tempdir().unwrap();
        let cache_dir = cache.path().join("not-yet-there");
        let hub = Hub::connect(&stand_in.base, None).unwrap();

        let dir = hub
            .fetch(
                &cache_dir,
                "owner/model",
                REVISION,
                &["config.json", "model.safetensors"],
            )
            .await
            .unwrap();

        assert_eq!(dir, model_dir(&cache_dir));
        assert_eq!(
            std::fs::read_to_string(dir.join("config.json")).unwrap(),
            "{}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("model.safetensors")).unwrap(),
            "weights"
        );
        assert_eq!(entries(&dir), ["config.json", "model.safetensors"]);
    }

    #[tokio::test]
    async fn uses_the_files_in_the_cache_without_asking_hugging_face() {
        let stand_in = StandIn::start(files_answered(&[("config.json", "{}")])).await;
        let cache = tempfile::tempdir().unwrap();
        let hub = Hub::connect(&stand_in.base, None).unwrap();
        hub.fetch(cache.path(), "owner/model", REVISION, &["config.json"])
            .await
            .unwrap();

        // Nothing listens here, so asking would fail.
        let offline = Hub::connect("http://127.0.0.1:9", None).unwrap();
        let dir = offline
            .fetch(cache.path(), "owner/model", REVISION, &["config.json"])
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("config.json")).unwrap(),
            "{}"
        );
        assert_eq!(stand_in.received().len(), 1);
    }

    #[tokio::test]
    async fn sends_the_token_when_it_has_one() {
        let stand_in = StandIn::start(files_answered(&[("a", "1"), ("b", "2")])).await;
        let cache = tempfile::tempdir().unwrap();

        Hub::connect(&stand_in.base, Some("hf_secret".to_owned()))
            .unwrap()
            .fetch(cache.path(), "owner/model", REVISION, &["a"])
            .await
            .unwrap();
        Hub::connect(&stand_in.base, None)
            .unwrap()
            .fetch(cache.path(), "owner/model", REVISION, &["b"])
            .await
            .unwrap();

        assert_eq!(
            stand_in.received(),
            [
                Received {
                    path: path_of("a"),
                    authorization: Some("Bearer hf_secret".to_owned()),
                },
                Received {
                    path: path_of("b"),
                    authorization: None,
                },
            ]
        );
    }

    #[tokio::test]
    async fn follows_a_redirect_to_where_the_file_is_served() {
        let mut answers = HashMap::new();
        answers.insert(
            path_of("model.safetensors"),
            answer(302, &[("location", "/served/weights")], ""),
        );
        answers.insert("/served/weights".to_owned(), answer(200, &[], "weights"));
        let stand_in = StandIn::start(answers).await;
        let cache = tempfile::tempdir().unwrap();

        let dir = Hub::connect(&stand_in.base, None)
            .unwrap()
            .fetch(
                cache.path(),
                "owner/model",
                REVISION,
                &["model.safetensors"],
            )
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("model.safetensors")).unwrap(),
            "weights"
        );
    }

    #[tokio::test]
    async fn reports_why_hugging_face_refused_and_leaves_no_file() {
        let mut answers = HashMap::new();
        answers.insert(
            path_of("config.json"),
            answer(
                401,
                &[(
                    "x-error-message",
                    "Access to model owner/model is restricted.",
                )],
                "",
            ),
        );
        let stand_in = StandIn::start(answers).await;
        let cache = tempfile::tempdir().unwrap();

        let error = Hub::connect(&stand_in.base, None)
            .unwrap()
            .fetch(cache.path(), "owner/model", REVISION, &["config.json"])
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "Hugging Face answered 401 Unauthorized for {}{}: Access to model owner/model is restricted.",
                stand_in.base,
                path_of("config.json")
            )
        );
        assert!(entries(&model_dir(cache.path())).is_empty());
    }

    #[tokio::test]
    async fn leaves_no_file_when_the_download_ends_early() {
        let mut answers = HashMap::new();
        // The body is shorter than the content-length says.
        answers.insert(
            path_of("model.safetensors"),
            "HTTP/1.1 200 OK\r\ncontent-length: 100\r\nconnection: close\r\n\r\nshort".to_owned(),
        );
        let stand_in = StandIn::start(answers).await;
        let cache = tempfile::tempdir().unwrap();

        let result = Hub::connect(&stand_in.base, None)
            .unwrap()
            .fetch(
                cache.path(),
                "owner/model",
                REVISION,
                &["model.safetensors"],
            )
            .await;

        assert!(result.is_err());
        assert!(entries(&model_dir(cache.path())).is_empty());
    }

    #[tokio::test]
    async fn removes_partial_files_of_stopped_fetches_but_not_of_running_ones() {
        let stand_in = StandIn::start(files_answered(&[("model.safetensors", "weights")])).await;
        let cache = tempfile::tempdir().unwrap();
        let dir = model_dir(cache.path());
        place_partials(
            &dir,
            &[".model.safetensors.a1.part", ".config.json.b2.part"],
            &[".model.safetensors.c3.part"],
        );

        Hub::connect(&stand_in.base, None)
            .unwrap()
            .fetch(
                cache.path(),
                "owner/model",
                REVISION,
                &["model.safetensors"],
            )
            .await
            .unwrap();

        assert_eq!(
            entries(&dir),
            [".model.safetensors.c3.part", "model.safetensors"]
        );
    }

    // Another process may finish the file the stopped fetch was for, and then
    // no fetch downloads it again.
    #[tokio::test]
    async fn removes_partial_files_of_stopped_fetches_when_every_file_is_already_there() {
        let cache = tempfile::tempdir().unwrap();
        let dir = model_dir(cache.path());
        place_partials(&dir, &[".config.json.a1.part"], &[]);
        std::fs::write(dir.join("config.json"), "{}").unwrap();

        Hub::connect("http://127.0.0.1:9", None)
            .unwrap()
            .fetch(cache.path(), "owner/model", REVISION, &["config.json"])
            .await
            .unwrap();

        assert_eq!(entries(&dir), ["config.json"]);
    }

    // A fetch whose download stalled for longer than the age still holds the
    // lock on its partial file.
    #[tokio::test]
    async fn keeps_an_old_partial_file_that_a_running_fetch_holds() {
        let cache = tempfile::tempdir().unwrap();
        let dir = model_dir(cache.path());
        place_partials(&dir, &[".model.safetensors.a1.part"], &[]);
        let held = std::fs::File::options()
            .write(true)
            .open(dir.join(".model.safetensors.a1.part"))
            .unwrap();
        held.lock().unwrap();
        std::fs::write(dir.join("model.safetensors"), "weights").unwrap();

        Hub::connect("http://127.0.0.1:9", None)
            .unwrap()
            .fetch(
                cache.path(),
                "owner/model",
                REVISION,
                &["model.safetensors"],
            )
            .await
            .unwrap();

        assert_eq!(
            entries(&dir),
            [".model.safetensors.a1.part", "model.safetensors"]
        );
        drop(held);
    }

    #[tokio::test]
    async fn two_fetches_that_both_download_the_same_file_both_succeed() {
        // Neither request is answered before both are in, so both fetches
        // download the file before either moves it into place.
        let stand_in =
            StandIn::start_in_batches(files_answered(&[("model.safetensors", "weights")]), 2).await;
        let cache = tempfile::tempdir().unwrap();
        let hub = Arc::new(Hub::connect(&stand_in.base, None).unwrap());

        let fetches: Vec<_> = (0..2)
            .map(|_| {
                let hub = hub.clone();
                let cache_dir = cache.path().to_owned();
                tokio::spawn(async move {
                    hub.fetch(&cache_dir, "owner/model", REVISION, &["model.safetensors"])
                        .await
                        .map_err(|error| error.to_string())
                })
            })
            .collect();
        for fetch in fetches {
            fetch.await.unwrap().unwrap();
        }

        assert_eq!(stand_in.received().len(), 2);
        let dir = model_dir(cache.path());
        assert_eq!(entries(&dir), ["model.safetensors"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("model.safetensors")).unwrap(),
            "weights"
        );
    }

    #[tokio::test]
    async fn fetches_a_file_in_a_subdirectory_into_that_subdirectory() {
        let stand_in =
            StandIn::start(files_answered(&[("2_Dense/model.safetensors", "dense")])).await;
        let cache = tempfile::tempdir().unwrap();

        let dir = Hub::connect(&stand_in.base, None)
            .unwrap()
            .fetch(
                cache.path(),
                "owner/model",
                REVISION,
                &["2_Dense/model.safetensors"],
            )
            .await
            .unwrap();

        assert_eq!(
            stand_in.received()[0].path,
            path_of("2_Dense/model.safetensors")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("2_Dense").join("model.safetensors")).unwrap(),
            "dense"
        );
        assert_eq!(entries(&dir.join("2_Dense")), ["model.safetensors"]);
    }

    #[tokio::test]
    async fn removes_partial_files_of_stopped_fetches_in_a_subdirectory() {
        let cache = tempfile::tempdir().unwrap();
        let subdirectory = model_dir(cache.path()).join("2_Dense");
        place_partials(
            &subdirectory,
            &[".model.safetensors.a1.part"],
            &[".model.safetensors.b2.part"],
        );
        std::fs::write(subdirectory.join("model.safetensors"), "dense").unwrap();

        Hub::connect("http://127.0.0.1:9", None)
            .unwrap()
            .fetch(
                cache.path(),
                "owner/model",
                REVISION,
                &["2_Dense/model.safetensors"],
            )
            .await
            .unwrap();

        assert_eq!(
            entries(&subdirectory),
            [".model.safetensors.b2.part", "model.safetensors"]
        );
    }

    #[tokio::test]
    async fn keeps_the_file_already_there_when_moving_a_partial_file_into_place() {
        let cache = tempfile::tempdir().unwrap();
        let dir = cache.path();
        std::fs::write(dir.join("model.safetensors"), "first").unwrap();
        let (mut out, partial) = new_partial(dir, "model.safetensors").await.unwrap();
        out.write_all(b"second").unwrap();

        persist(out, partial, dir.join("model.safetensors"))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(dir.join("model.safetensors")).unwrap(),
            "first"
        );
        assert_eq!(entries(dir), ["model.safetensors"]);
    }
}
