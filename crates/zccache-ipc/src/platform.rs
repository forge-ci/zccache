//! zccache-owned local-IPC endpoint and admission policy.
//!
//! Endpoint text, test-name shape, connect deadlines, and peer-rejection
//! vocabulary are product policy. The transport objects remain the canonical
//! protected kernal-api socket/pipe capabilities.

/// Stat observations shared with the filesystem cache. Keep native metadata
/// access in this approved adapter until kernal-api exposes the same operation
/// on metadata the caller already holds (its path APIs would add another stat).
pub mod fs {
    /// `(ctime_ns, inode)` from an existing stat; `(0, 0)` when unavailable.
    ///
    /// This performs no I/O. Unix change time detects restored-mtime edits and
    /// the inode detects replacement. Windows retains watcher-based detection.
    #[must_use]
    pub fn metadata_identity(metadata: &std::fs::Metadata) -> (u64, u64) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (
                (metadata.ctime() as u64)
                    .wrapping_mul(1_000_000_000)
                    .wrapping_add(metadata.ctime_nsec() as u64),
                metadata.ino(),
            )
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            (0, 0)
        }
    }
}

// Keep host selection for Unix-only transport tests inside this boundary.
#[cfg(all(test, unix))]
macro_rules! unix_transport_tests {
    ($($item:tt)*) => { $($item)* };
}

#[cfg(all(test, not(unix)))]
macro_rules! unix_transport_tests {
    ($($item:tt)*) => {};
}

#[cfg(test)]
pub(crate) use unix_transport_tests;

pub(crate) mod host {
    pub(crate) fn runtime_dir() -> Option<String> {
        (!kernal_api::platform::host::target_is_windows())
            .then(|| std::env::var("XDG_RUNTIME_DIR").ok())
            .flatten()
    }

    pub(crate) fn current_user() -> Option<String> {
        if kernal_api::platform::host::target_is_windows() {
            std::env::var("USERNAME").ok()
        } else {
            std::env::var("USER").ok()
        }
    }

    /// This process's PID namespace, on hosts that have them (#2035).
    pub(crate) fn pid_namespace() -> Option<String> {
        parse_pid_namespace(&kernal_api::platform::host::namespace_id()?)
    }

    /// The `pid:[inode]` part of the facade's `<mnt ns>:<pid ns>` text. Only
    /// the PID namespace scopes a PID: containers sharing one can have
    /// different mount namespaces and still see the same processes.
    pub(crate) fn parse_pid_namespace(namespace_id: &str) -> Option<String> {
        let start = namespace_id.rfind("pid:[")?;
        Some(namespace_id[start..].to_string())
    }
}

pub(crate) mod process {
    pub(crate) mod inspect {
        pub(crate) fn executable_path(pid: u32) -> Option<std::path::PathBuf> {
            let process = kernal_api::platform::process::ProcessLiveness::open(pid).ok()?;
            kernal_api::platform::process::executable_path(&process).ok()
        }
    }
}

pub(crate) mod ipc {
    use std::fmt;
    use std::io;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::task::{Context, Poll};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    /// Who may replace a Unix socket path (#2106).
    ///
    /// A listener holds an advisory lock on `<socket>.bind-lock` for its whole
    /// life, so the kernel releases it on any exit, including a crash. Holding
    /// the lock therefore proves no locking listener is alive: there is no
    /// check-then-unlink window between processes using it. The lock file is
    /// never deleted, because a holder of a deleted file excludes nobody.
    #[cfg(unix)]
    mod bind_lock {
        use std::io;
        use std::time::{Duration, Instant};

        use kernal_api::platform::fs::{self, OwnedFileLock};

        use super::Endpoint;

        /// Outlasts a concurrent retire or a failing bind, which hold the lock
        /// for microseconds. A live listener outlasts it and means "in use".
        const CONTENTION_BUDGET: Duration = Duration::from_millis(100);
        const CONTENTION_PAUSE: Duration = Duration::from_millis(5);
        /// A probe that gets no answer here is treated as live.
        const PROBE_BUDGET: Duration = Duration::from_secs(1);

        pub(super) fn try_acquire(endpoint: &Endpoint) -> io::Result<OwnedFileLock> {
            let mut path = std::ffi::OsString::from(endpoint.as_str());
            path.push(".bind-lock");
            fs::try_lock_exclusive_owned(fs::open_lock_file(std::path::Path::new(&path))?)
        }

        pub(super) fn acquire(endpoint: &Endpoint) -> io::Result<OwnedFileLock> {
            let deadline = Instant::now() + CONTENTION_BUDGET;
            loop {
                match try_acquire(endpoint) {
                    Err(error) if fs::is_lock_conflict(&error) => {
                        if Instant::now() >= deadline {
                            return Err(in_use(endpoint));
                        }
                        std::thread::sleep(CONTENTION_PAUSE);
                    }
                    result => return result,
                }
            }
        }

        /// A daemon from a build without the bind lock leaves the lock free
        /// while it serves, and only a connect can see it. This runs at bind
        /// and only when a socket file is present, never on a compile request.
        pub(super) fn refuse_unlocked_live_listener(endpoint: &Endpoint) -> io::Result<()> {
            use std::os::unix::fs::FileTypeExt as _;
            // Only a socket can have a listener. Hosts differ on what a connect
            // to anything else reports (Linux refuses it, macOS does not), and
            // the retire that follows refuses a non-socket on every host.
            let is_socket = std::fs::symlink_metadata(endpoint.as_str())
                .is_ok_and(|metadata| metadata.file_type().is_socket());
            if !is_socket {
                return Ok(());
            }
            match crate::probe::probe_local_socket_with_deadline(endpoint.as_str(), PROBE_BUDGET) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                    ) =>
                {
                    Ok(())
                }
                // Any other outcome leaves it open whether something is
                // listening, and that is no licence to unlink the path.
                _ => Err(in_use(endpoint)),
            }
        }

        fn in_use(endpoint: &Endpoint) -> io::Error {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a live listener already owns {endpoint}"),
            )
        }
    }

    static TEST_ENDPOINT: AtomicU64 = AtomicU64::new(0);

    /// Product-owned endpoint spelling. Transport validation occurs at bind or
    /// connect, after the platform has selected the native representation.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct Endpoint(String);

    impl Endpoint {
        pub(crate) fn from_native(value: impl Into<String>) -> Self {
            Self(value.into())
        }

        pub(crate) fn as_str(&self) -> &str {
            &self.0
        }

        /// Remove a stale endpoint. A socket whose listener holds the bind lock
        /// is not stale, so it stays and this reports success (#2106).
        pub(crate) fn retire(&self) -> io::Result<()> {
            #[cfg(unix)]
            {
                if std::fs::symlink_metadata(&self.0).is_err() {
                    return self.retire_unguarded();
                }
                match bind_lock::try_acquire(self) {
                    Ok(_lock) => self.retire_unguarded(),
                    Err(error) if kernal_api::platform::fs::is_lock_conflict(&error) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            #[cfg(windows)]
            {
                self.retire_unguarded()
            }
        }

        fn retire_unguarded(&self) -> io::Result<()> {
            kernal_api::platform::ipc::retire_socket_endpoint(std::path::Path::new(&self.0))
        }

        pub(crate) fn unique_test(name: &str) -> Self {
            let id = TEST_ENDPOINT.fetch_add(1, Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            if kernal_api::platform::host::target_is_windows() {
                Self(format!(
                    r"\\.\pipe\zccache-ipc-{name}-{}-{nonce}-{id}",
                    std::process::id()
                ))
            } else {
                Self(format!(
                    "/tmp/zccache-ipc-{}-{nonce}-{id}/{name}.sock",
                    std::process::id()
                ))
            }
        }

        pub(crate) fn select(file_path: impl Into<String>, pipe_name: impl Into<String>) -> Self {
            if kernal_api::platform::host::target_is_windows() {
                Self::from_running_process(pipe_name)
            } else {
                Self(file_path.into())
            }
        }

        pub(crate) fn to_running_process(&self) -> String {
            if kernal_api::platform::host::target_is_windows() {
                self.0
                    .strip_prefix(r"\\.\pipe\")
                    .unwrap_or(&self.0)
                    .to_owned()
            } else {
                self.0.clone()
            }
        }

        pub(crate) fn from_running_process(value: impl Into<String>) -> Self {
            let value = value.into();
            if kernal_api::platform::host::target_is_windows() && !value.starts_with(r"\\.\pipe\") {
                Self(format!(r"\\.\pipe\{value}"))
            } else {
                Self(value)
            }
        }

        pub(crate) fn file_path_is_portable(value: &str) -> bool {
            kernal_api::platform::host::target_is_windows() || value.len() <= 100
        }

        pub(crate) fn uses_file_path(&self) -> bool {
            !kernal_api::platform::host::target_is_windows()
        }

        pub(crate) fn connect_timeout(&self) -> Duration {
            if kernal_api::platform::host::target_is_windows() {
                Duration::from_secs(5)
            } else {
                Duration::from_secs(30)
            }
        }
    }

    impl fmt::Display for Endpoint {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.as_str())
        }
    }

    /// Product-facing, stable reason taxonomy for peer rejection.
    pub(crate) struct PeerIdentity {
        pid: Option<u32>,
        current_user: bool,
        credentials_available: bool,
    }

    impl PeerIdentity {
        #[cfg(unix)]
        fn from_credentials(credentials: kernal_api::platform::ipc::SocketPeerCredentials) -> Self {
            Self {
                pid: credentials.pid(),
                current_user: credentials.is_current_user(),
                credentials_available: credentials.credentials_available(),
            }
        }

        #[cfg(windows)]
        fn current_user_pipe() -> Self {
            Self {
                pid: None,
                current_user: true,
                credentials_available: true,
            }
        }

        pub(crate) fn pid(&self) -> Option<u32> {
            self.pid
        }

        pub(crate) fn rejection_reason(&self) -> Option<&'static str> {
            if !self.credentials_available {
                Some("peer-cred-unavailable")
            } else if !self.current_user {
                Some("foreign-uid")
            } else {
                None
            }
        }
    }

    pub(crate) enum Stream {
        #[cfg(unix)]
        Unix(kernal_api::platform::ipc::LocalSocketStream),
        #[cfg(windows)]
        Server(kernal_api::platform::ipc::OwnerOnlyPipeInstance),
        #[cfg(windows)]
        Client(kernal_api::platform::ipc::LocalPipeClient),
    }

    impl AsyncRead for Stream {
        fn poll_read(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            #[cfg(unix)]
            {
                let Self::Unix(stream) = self.get_mut();
                match stream.poll_read(context, buffer.initialize_unfilled()) {
                    Poll::Ready(Ok(read)) => {
                        buffer.advance(read);
                        Poll::Ready(Ok(()))
                    }
                    Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                    Poll::Pending => Poll::Pending,
                }
            }
            #[cfg(windows)]
            match self.get_mut() {
                Self::Server(stream) => {
                    match stream.poll_read(context, buffer.initialize_unfilled()) {
                        Poll::Ready(Ok(read)) => {
                            buffer.advance(read);
                            Poll::Ready(Ok(()))
                        }
                        Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                        Poll::Pending => Poll::Pending,
                    }
                }
                Self::Client(stream) => {
                    match stream.poll_read(context, buffer.initialize_unfilled()) {
                        Poll::Ready(Ok(read)) => {
                            buffer.advance(read);
                            Poll::Ready(Ok(()))
                        }
                        Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                        Poll::Pending => Poll::Pending,
                    }
                }
            }
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            #[cfg(unix)]
            {
                let Self::Unix(stream) = self.get_mut();
                stream.poll_write(context, buffer)
            }
            #[cfg(windows)]
            match self.get_mut() {
                Self::Server(stream) => stream.poll_write(context, buffer),
                Self::Client(stream) => stream.poll_write(context, buffer),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
            #[cfg(unix)]
            {
                let Self::Unix(stream) = self.get_mut();
                stream.poll_flush(context)
            }
            #[cfg(windows)]
            match self.get_mut() {
                Self::Server(stream) => stream.poll_flush(context),
                Self::Client(stream) => stream.poll_flush(context),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
            #[cfg(unix)]
            {
                let Self::Unix(stream) = self.get_mut();
                stream.poll_shutdown(context)
            }
            #[cfg(windows)]
            match self.get_mut() {
                Self::Server(stream) => stream.poll_shutdown(context),
                Self::Client(stream) => stream.poll_shutdown(context),
            }
        }
    }

    pub(crate) struct Listener {
        #[cfg(unix)]
        inner: kernal_api::platform::ipc::LocalSocketListener,
        #[cfg(unix)]
        tightened_parent: bool,
        /// Declared after `inner`: the socket closes before the path is released.
        #[cfg(unix)]
        _bind_lock: kernal_api::platform::fs::OwnedFileLock,
        #[cfg(windows)]
        endpoint: String,
        #[cfg(windows)]
        pool: std::collections::VecDeque<kernal_api::platform::ipc::OwnerOnlyPipeInstance>,
    }

    impl Listener {
        pub(crate) fn bind(endpoint: &Endpoint) -> io::Result<Self> {
            #[cfg(unix)]
            {
                let mut tightened_parent = false;
                if let Some(parent) = std::path::Path::new(endpoint.as_str()).parent() {
                    kernal_api::platform::fs::create_dir_all_private(parent)?;
                    tightened_parent = kernal_api::platform::fs::ensure_dir_private(parent)
                        .map_err(|error| {
                            io::Error::new(
                                error.kind(),
                                format!("insecure socket directory: {error}"),
                            )
                        })?;
                }
                // The lock comes before any look at the path and lasts as long
                // as the listener, so no bind or retire can unlink it (#2106).
                let bind_lock = bind_lock::acquire(endpoint)?;
                bind_lock::refuse_unlocked_live_listener(endpoint)?;
                endpoint.retire_unguarded()?;
                Ok(Self {
                    inner: kernal_api::platform::ipc::LocalSocketListener::bind_owner_only(
                        std::path::Path::new(endpoint.as_str()),
                    )?,
                    tightened_parent,
                    _bind_lock: bind_lock,
                })
            }
            #[cfg(windows)]
            {
                let pool_size = std::env::var("ZCCACHE_PIPE_POOL_SIZE")
                    .ok()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or_else(|| {
                        std::thread::available_parallelism()
                            .map(|parallelism| parallelism.get().saturating_mul(4))
                            .unwrap_or(64)
                            .clamp(16, 128)
                    });
                let mut pool = std::collections::VecDeque::with_capacity(pool_size);
                pool.push_back(create_first_with_retry(endpoint.as_str())?);
                for _ in 1..pool_size {
                    pool.push_back(create_pipe(endpoint.as_str(), false)?);
                }
                Ok(Self {
                    endpoint: endpoint.as_str().to_owned(),
                    pool,
                })
            }
        }

        pub(crate) async fn bind_async(endpoint: &Endpoint) -> io::Result<Self> {
            Self::bind(endpoint)
        }

        pub(crate) async fn accept(&mut self) -> io::Result<(Stream, PeerIdentity)> {
            #[cfg(unix)]
            {
                let (stream, peer) = self.inner.accept().await?;
                Ok((Stream::Unix(stream), PeerIdentity::from_credentials(peer)))
            }
            #[cfg(windows)]
            loop {
                let pipe = match self.pool.pop_front() {
                    Some(pipe) => pipe,
                    None => create_with_retry(&self.endpoint).await?,
                };
                // Keep the connect future alive until Windows completes it.
                // Cancelling a pending named-pipe connect destroys this server
                // instance even when a client has already opened it.  That
                // closes the client's otherwise healthy compile connection
                // without a response under sustained connection churn.
                match Self::await_pipe_connection(pipe.connect()).await {
                    Ok(()) => {
                        if let Ok(replacement) = create_with_retry(&self.endpoint).await {
                            self.pool.push_back(replacement);
                        }
                        return Ok((Stream::Server(pipe), PeerIdentity::current_user_pipe()));
                    }
                    Err(_) => {
                        if let Ok(replacement) = create_with_retry(&self.endpoint).await {
                            self.pool.push_back(replacement);
                        }
                    }
                }
            }
        }

        /// Preserve the exact pending Windows connect operation until the OS
        /// completes it. A deadline here would be destructive, not merely
        /// observational: dropping the future closes its pipe instance.
        #[cfg(windows)]
        async fn await_pipe_connection<F>(connect: F) -> io::Result<()>
        where
            F: std::future::Future<Output = io::Result<()>>,
        {
            connect.await
        }

        pub(crate) fn tightened_parent(&self) -> bool {
            #[cfg(unix)]
            {
                self.tightened_parent
            }
            #[cfg(windows)]
            {
                false
            }
        }

        #[cfg(test)]
        pub(crate) fn drain_accept_pool(&mut self) -> usize {
            #[cfg(unix)]
            {
                0
            }
            #[cfg(windows)]
            {
                let count = self.pool.len();
                self.pool.clear();
                count
            }
        }
    }

    pub(crate) async fn connect(endpoint: &Endpoint) -> io::Result<Stream> {
        #[cfg(unix)]
        {
            return kernal_api::platform::ipc::LocalSocketStream::connect(std::path::Path::new(
                endpoint.as_str(),
            ))
            .await
            .map(Stream::Unix);
        }
        #[cfg(windows)]
        {
            let mut delay = Duration::from_millis(10);
            loop {
                let endpoint = endpoint.as_str().to_owned();
                let opened = tokio::task::spawn_blocking(move || {
                    kernal_api::platform::ipc::LocalPipeClient::open(&endpoint)
                })
                .await
                .map_err(|error| io::Error::other(format!("pipe open worker failed: {error}")))?;
                match opened {
                    Ok(client) => return Ok(Stream::Client(client)),
                    Err(error) if error.raw_os_error() == Some(231) => {
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(Duration::from_millis(500));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }

    pub(crate) fn current_user_name() -> Option<String> {
        super::host::current_user()
    }

    pub(crate) fn select_host_text(file_value: String, windows_value: String) -> String {
        if kernal_api::platform::host::target_is_windows() {
            windows_value
        } else {
            file_value
        }
    }

    /// Dial an endpoint given in running-process spelling and drop the
    /// connection. On Windows the `RUNNING_PROCESS_FAKE_BACKEND` seam carries
    /// a bare pipe name, which must gain the `\\.\pipe\` prefix before the
    /// native pipe open (an already-native name, or a Unix socket path,
    /// passes through unchanged).
    pub(crate) fn probe_running_process(endpoint: &str) -> io::Result<()> {
        let endpoint = Endpoint::from_running_process(endpoint);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| io::Error::other(format!("IPC probe runtime failed: {error}")))?;
        zccache_core::wait::block_on_for(
            &runtime,
            async {
                drop(connect(&endpoint).await?);
                Ok(())
            },
            std::time::Duration::from_secs(5),
        )
        .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?
    }

    #[cfg(windows)]
    fn create_pipe(
        endpoint: &str,
        first: bool,
    ) -> io::Result<kernal_api::platform::ipc::OwnerOnlyPipeInstance> {
        kernal_api::platform::ipc::OwnerOnlyPipeInstance::create(endpoint, first)
    }

    #[cfg(windows)]
    fn create_first_with_retry(
        endpoint: &str,
    ) -> io::Result<kernal_api::platform::ipc::OwnerOnlyPipeInstance> {
        const ATTEMPTS: u32 = 8;
        let mut delay = Duration::from_millis(20);
        let mut attempt = 1;
        loop {
            match create_pipe(endpoint, true) {
                Ok(pipe) => return Ok(pipe),
                // The final attempt's error is the one reported.
                Err(error) if attempt == ATTEMPTS => return Err(error),
                Err(_) => {}
            }
            std::thread::sleep(delay);
            delay = (delay * 2).min(Duration::from_millis(160));
            attempt += 1;
        }
    }

    #[cfg(windows)]
    async fn create_with_retry(
        endpoint: &str,
    ) -> io::Result<kernal_api::platform::ipc::OwnerOnlyPipeInstance> {
        const ATTEMPTS: u32 = 5;
        let mut delay = Duration::from_millis(5);
        let mut attempt = 1;
        loop {
            let endpoint = endpoint.to_owned();
            let created = tokio::task::spawn_blocking(move || create_pipe(&endpoint, false))
                .await
                .map_err(|error| io::Error::other(format!("pipe create worker failed: {error}")))?;
            match created {
                Ok(pipe) => return Ok(pipe),
                // The final attempt's error is the one reported.
                Err(error) if attempt == ATTEMPTS => return Err(error),
                Err(_) => {}
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_millis(80));
            attempt += 1;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[cfg(windows)]
        struct DropDetector {
            cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
            armed: bool,
        }

        #[cfg(windows)]
        impl DropDetector {
            fn disarm(&mut self) {
                self.armed = false;
            }
        }

        #[cfg(windows)]
        impl Drop for DropDetector {
            fn drop(&mut self) {
                if self.armed {
                    self.cancelled.store(true, Ordering::SeqCst);
                }
            }
        }

        /// The production pipe-connect policy must retain the same pending
        /// operation beyond the former five-second deadline. The detector is
        /// disarmed only after normal completion, so a timeout/cancellation
        /// reintroduced inside `await_pipe_connection` fails this test.
        #[cfg(windows)]
        #[tokio::test]
        async fn pipe_connection_wait_does_not_cancel_the_pending_operation() {
            let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut detector = DropDetector {
                cancelled: std::sync::Arc::clone(&cancelled),
                armed: true,
            };

            Listener::await_pipe_connection(async move {
                tokio::time::sleep(Duration::from_millis(5_100)).await;
                detector.disarm();
                Ok(())
            })
            .await
            .expect("connect operation should complete without cancellation");

            assert!(
                !cancelled.load(Ordering::SeqCst),
                "pending connect operation was dropped before completion"
            );
        }

        #[test]
        fn unique_test_endpoints_are_distinct() {
            assert_ne!(Endpoint::unique_test("a"), Endpoint::unique_test("a"));
        }

        #[test]
        fn endpoint_spelling_roundtrips_through_running_process() {
            let endpoint = Endpoint::from_running_process("zccache-test");
            assert_eq!(
                Endpoint::from_running_process(endpoint.to_running_process()),
                endpoint
            );
        }
    }
}
