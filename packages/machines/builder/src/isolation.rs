//! Keeping a build away from what the build service holds.
//!
//! A build runs the author's code. Run as the build service's own user, that
//! code could reach whatever the service can: in the attested image, the
//! launcher's socket, which hands out tokens that attest this image (with a
//! key of the build's own choosing in them), and the service's memory, state
//! and store. So where the build service runs as root, every process a build
//! starts (the compiler, its build scripts and procedural macros, the load
//! step) runs as an unprivileged user and group of its own, in a job
//! directory handed to that user, and nothing else of the service's is
//! writable or readable by it.
//!
//! In the attested image the launcher's socket is handed to the container
//! with modes that may let any user connect to it, on a mount that may be
//! read-only. So at start, as root, the build service closes it to all but
//! root (see [`restrict`]): the socket to `0600` and its directory to
//! `0700`, both owned by root, and where those can't be changed, the
//! nearest directory above them that can, root-owned `0700`, since no one
//! reaches a socket without searching every directory on its path. Root's
//! own token requests are untouched, root being refused nothing by a mode.
//!
//! A build step's process group is killed when the step ends, but a build
//! may start a process in a session of its own, outside that group. So once
//! each step is done, when a job ends, and at start, every process the build
//! user owns is killed (see [`sweep`]): nothing of one build is left running
//! as the build user when the next starts. A sweep that leaves any of them
//! running, or can't be run, holds the queue until one comes back clean.
//!
//! Before it takes any job, the build service checks that this holds: a
//! process run as the build user tries to connect to the launcher's socket,
//! and if it can, no job is taken and the health route says why.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// The user and group every build runs as when the build service runs as
/// root: the system's `nobody`.
pub const BUILD_USER: u32 = 65_534;

/// The argument that has the build service's own program try to connect to
/// the socket after it, and say by how it exits whether it could (see
/// [`probe_here`]).
pub const PROBE: &str = "--probe-socket";

/// The argument that has the build service's own program, run as the build
/// user, kill every process the build user owns (see [`sweep_here`]).
pub const SWEEP: &str = "--sweep-build-user";

/// Exit codes of [`probe_here`].
const REACHED: i32 = 0;
const REFUSED: i32 = 3;
const ABSENT: i32 = 4;

static DROP_TO: OnceLock<Option<u32>> = OnceLock::new();

/// The user builds run as, if not the build service's own: [`BUILD_USER`]
/// when the build service runs as root, and none otherwise (a laptop, where
/// the build service is already an ordinary user and can't change user).
pub fn build_user() -> Option<u32> {
    *DROP_TO.get_or_init(|| rustix::process::geteuid().is_root().then_some(BUILD_USER))
}

/// Have `command` run as the build user, if builds run as one. Its
/// supplementary groups are dropped with it.
pub fn as_build_user(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    if let Some(id) = build_user() {
        command.uid(id).gid(id);
    }
}

/// Hand everything under `directory`, and `directory` itself, to the build
/// user, if builds run as one: a build writes only in its own job
/// directory. Links are handed over themselves, never followed.
pub fn hand_over(directory: &Path) -> std::io::Result<()> {
    let Some(id) = build_user() else {
        return Ok(());
    };
    std::os::unix::fs::lchown(directory, Some(id), Some(id))?;
    let metadata = std::fs::symlink_metadata(directory)?;
    if metadata.is_dir() {
        for entry in std::fs::read_dir(directory)? {
            hand_over(&entry?.path())?;
        }
    }
    Ok(())
}

/// How often [`sweep_here`] signals before it gives up on processes that
/// are still there, a few milliseconds apart.
const SWEEPS: u32 = 250;

/// [`sweep_here`]'s exit code when processes of the build user are left.
const LEFT: i32 = 5;

/// Why the last sweep did not come back clean, if it did not (see
/// [`unswept`]).
static UNSWEPT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Why the last sweep left processes of the build user running, or could
/// not be run: `None` once a sweep comes back clean. While it is some, no
/// job is taken.
pub fn unswept() -> Option<String> {
    UNSWEPT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Kill every process the build user owns, if builds run as one (as root;
/// otherwise there is no user of the builds' own, and nothing is done). The
/// killing is done by the build service's own program run as the build
/// user, given [`SWEEP`]: a process may signal every process of its own
/// user, which is what `kill(-1, SIGKILL)` does. Processes it leaves
/// running, or a sweep that could not be run, are said as errors and noted
/// (see [`unswept`]); a clean sweep clears the note. Then the build user's
/// processes the build service was handed, ended and not yet reaped, are
/// reaped (see [`reap_orphans`]). It blocks while it waits: on the async
/// runtime it is run apart (see [`sweep_apart`]).
pub fn sweep() {
    if build_user().is_none() {
        return;
    }
    let outcome = sweep_once();
    if let Err(why) = &outcome {
        tracing::error!(why, "the build user's processes were not all killed");
    }
    *UNSWEPT.lock().unwrap_or_else(|e| e.into_inner()) = outcome.err();
    reap_orphans();
}

/// [`sweep`], on a thread of the runtime's kept for blocking work, so no
/// worker of the runtime waits on it.
pub async fn sweep_apart() {
    if let Err(error) = tokio::task::spawn_blocking(sweep).await {
        tracing::error!(%error, "the sweep of the build user's processes did not end");
        *UNSWEPT.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(format!("the sweep did not end: {error}"));
    }
}

fn sweep_once() -> Result<(), String> {
    let program = std::env::current_exe().map_err(|error| {
        format!("the sweep could not be run: the program is not found: {error}")
    })?;
    let mut command = Command::new(program);
    command
        .arg(SWEEP)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    as_build_user(&mut command);
    match command.status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "processes of the build user are left running after a sweep (it ended {:?})",
            status.code()
        )),
        Err(error) => Err(format!("the sweep could not be run: {error}")),
    }
}

/// What the build service's program does given [`SWEEP`], run as the build
/// user: signal every process it may signal (every process of the build
/// user but itself) to be killed, again and again until none is left alive,
/// and exit `0` once none is, or [`LEFT`] if some still are after
/// [`SWEEPS`] tries, or if this is not the build user.
pub fn sweep_here() -> i32 {
    use rustix::process::{Pid, Signal, kill_process_group};

    // Only ever as the build user: run as anyone else, this would kill all
    // of that user's processes.
    if rustix::process::getuid().as_raw() != BUILD_USER {
        return LEFT;
    }
    // `kill(-1, …)`: signalling "group 1" is the call that reaches every
    // process the caller may signal, as POSIX defines it.
    for _ in 0..SWEEPS {
        match kill_process_group(Pid::INIT, Signal::KILL) {
            // None could be signalled: none is left.
            Err(rustix::io::Errno::SRCH) => return 0,
            _ if !others_alive() => return 0,
            _ => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    LEFT
}

/// Whether any process of this process's user other than itself is still
/// alive (not ended and waiting to be reaped). Where `/proc` can't be read,
/// any process that can be signalled is taken for alive.
fn others_alive() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return true;
    };
    let me = std::process::id();
    let user = rustix::process::getuid().as_raw();
    entries.flatten().any(|entry| {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            return false;
        };
        pid != me && read_status(pid).is_some_and(|status| status.uid == user && !status.ended)
    })
}

/// What `/proc/<pid>/status` says of a process.
struct Status {
    uid: u32,
    parent: u32,
    /// Ended and waiting to be reaped.
    ended: bool,
}

fn read_status(pid: u32) -> Option<Status> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    Some(Status {
        uid: field("Uid:")?.split_whitespace().next()?.parse().ok()?,
        parent: field("PPid:")?.parse().ok()?,
        ended: field("State:")?.starts_with(['Z', 'X']),
    })
}

/// Reap the build user's processes that ended handed to the build service
/// (it is the first process of its container, so a build's orphans are
/// handed to it): every child of the build service that is the build user's
/// and has ended. Only those, by id: a build step's own child is reaped by
/// the step before any sweep, and any other child is left to whoever waits
/// for it. Without `/proc`, nothing is reaped.
fn reap_orphans() {
    use rustix::process::{Pid, WaitOptions, waitpid};

    let Some(user) = build_user() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if read_status(pid)
            .is_some_and(|status| status.parent == me && status.uid == user && status.ended)
            && let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw)
        {
            let _ = waitpid(Some(pid), WaitOptions::NOHANG);
        }
    }
}

/// Whether a build could reach the socket at `socket`.
#[derive(Debug, PartialEq, Eq)]
pub enum Reach {
    /// No socket is there: nothing to reach.
    Absent,
    /// It is there, and a build is refused it.
    Refused,
    /// A build can connect to it.
    Reached,
    /// The check itself could not be made.
    Unknown(String),
}

/// Try, as a build would (the build service's own program given [`PROBE`],
/// run as the build user), to connect to `socket`.
pub fn probe(socket: &Path) -> Reach {
    let program = match std::env::current_exe() {
        Ok(program) => program,
        Err(error) => return Reach::Unknown(format!("the program is not found: {error}")),
    };
    let mut command = Command::new(program);
    command
        .arg(PROBE)
        .arg(socket)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    as_build_user(&mut command);
    match command.status() {
        Ok(status) => match status.code() {
            Some(REACHED) => Reach::Reached,
            Some(REFUSED) => Reach::Refused,
            Some(ABSENT) => Reach::Absent,
            other => Reach::Unknown(format!("the probe ended {other:?}")),
        },
        Err(error) => Reach::Unknown(format!("the probe could not be run: {error}")),
    }
}

/// What the build service's program does given [`PROBE`]: connect to the
/// socket at `socket`, and exit saying whether it could.
pub fn probe_here(socket: &Path) -> i32 {
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => REACHED,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ABSENT,
        Err(_) => REFUSED,
    }
}

/// The mode the launcher's socket is left with: its owner's alone.
pub const SOCKET_MODE: u32 = 0o600;

/// The mode the socket's directory, or the directory above it that is
/// closed instead, is left with: its owner's alone. Not even search is left
/// to others, since a process must be able to search every directory on a
/// path to reach a socket at its end.
pub const DIRECTORY_MODE: u32 = 0o700;

/// A file's mode, owner and group, or why they could not be read, as the
/// log shows them.
fn found(path: &Path) -> String {
    use std::os::unix::fs::MetadataExt;

    match std::fs::metadata(path) {
        Ok(metadata) => format!(
            "mode {:04o} uid {} gid {}",
            metadata.mode() & 0o7777,
            metadata.uid(),
            metadata.gid()
        ),
        Err(error) => format!("unreadable: {error}"),
    }
}

/// Close the socket at `socket` to all but root (see [`close`]), checking
/// each step by having the build user try to connect (see [`probe`]).
/// `needed` are the paths builds use, under which nothing is closed. Each
/// step that failed is returned, saying why.
pub fn restrict(socket: &Path, needed: &[&Path]) -> Vec<String> {
    let mut needed: Vec<std::path::PathBuf> =
        needed.iter().map(|path| path.to_path_buf()).collect();
    // The probe and the sweep run the build service's own program as the
    // build user, and a build may write temporary files.
    needed.extend(std::env::current_exe().ok());
    needed.push(std::env::temp_dir());
    let closed = close(socket, 0, 0, Path::new("/"), &needed, || {
        matches!(probe(socket), Reach::Reached | Reach::Unknown(_))
    });
    // Root is refused nothing by a mode: its own token requests go on.
    match std::os::unix::net::UnixStream::connect(socket) {
        Ok(_) => {
            tracing::info!(socket = %socket.display(), "the build service still reaches the launcher's socket")
        }
        Err(error) if socket.exists() => {
            tracing::error!(socket = %socket.display(), %error, "the build service no longer reaches the launcher's socket")
        }
        Err(_) => {}
    }
    closed
}

/// Hand `path` to `owner` and `group`, where it is not theirs already, and
/// give it `mode`, logging its mode, owner and group before and after. Why
/// either step failed is logged and returned.
fn close_one(path: &Path, owner: u32, group: u32, mode: u32, what: &str) -> Vec<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mut failed = Vec::new();
    let before = found(path);
    let owned = std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.uid() == owner && metadata.gid() == group);
    if !owned && let Err(error) = std::os::unix::fs::chown(path, Some(owner), Some(group)) {
        tracing::error!(path = %path.display(), owner, group, %error, found = before, "{what} could not be handed to its owner");
        failed.push(format!(
            "chown {owner}:{group} {}: {error} ({before})",
            path.display()
        ));
    }
    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        tracing::error!(path = %path.display(), mode = format!("{mode:04o}"), %error, found = before, "{what}'s mode could not be set");
        failed.push(format!(
            "chmod {mode:04o} {}: {error} ({before})",
            path.display()
        ));
    }
    tracing::info!(path = %path.display(), before, after = found(path), "closing {what} to all but its owner");
    failed
}

/// Close the socket at `socket` to all but `owner` (`group` too owning
/// what is closed), until `reached` says a build no longer reaches it.
///
/// First the socket's directory gets [`DIRECTORY_MODE`] and the socket
/// [`SOCKET_MODE`], both handed to `owner` and `group`; either alone keeps
/// another user out, so one failing does not stop the other. If a build
/// still reaches it (the socket's directory may be a read-only mount), the
/// directories above are closed instead, nearest first, each handed to
/// `owner` and `group` with [`DIRECTORY_MODE`]: a process must be able to
/// search every directory on a path to reach what is at its end. One that
/// can't be changed is passed over for the one above, and each closed is
/// checked with `reached` before going higher. `top` and what is above it
/// are never closed, nor a directory holding any of `needed` (nor, then,
/// anything above it); what each directory holds is logged before it is
/// closed. Each step that failed is returned, saying why.
pub fn close(
    socket: &Path,
    owner: u32,
    group: u32,
    top: &Path,
    needed: &[std::path::PathBuf],
    reached: impl FnMut() -> bool,
) -> Vec<String> {
    close_with(socket, owner, group, top, needed, reached, close_one)
}

/// [`close`], each file closed by `close_one`.
fn close_with(
    socket: &Path,
    owner: u32,
    group: u32,
    top: &Path,
    needed: &[std::path::PathBuf],
    mut reached: impl FnMut() -> bool,
    close_one: impl Fn(&Path, u32, u32, u32, &str) -> Vec<String>,
) -> Vec<String> {
    if !socket.exists() {
        tracing::warn!(socket = %socket.display(), "the launcher's socket is not there to close");
        return Vec::new();
    }
    let directory = socket.parent().unwrap_or(Path::new("/"));
    let mut failed = close_one(
        directory,
        owner,
        group,
        DIRECTORY_MODE,
        "the socket's directory",
    );
    failed.extend(close_one(
        socket,
        owner,
        group,
        SOCKET_MODE,
        "the launcher's socket",
    ));
    if !reached() {
        return failed;
    }
    tracing::warn!(
        socket = %socket.display(),
        "a build still reaches the launcher's socket: closing the directories above it"
    );
    let resolved = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let top = resolved(top);
    let needed: Vec<std::path::PathBuf> = needed.iter().map(|path| resolved(path)).collect();
    for above in directory.ancestors().skip(1) {
        let above = resolved(above);
        if above == top || !above.starts_with(&top) || above.parent().is_none() {
            break;
        }
        if let Some(need) = needed.iter().find(|need| need.starts_with(&above)) {
            tracing::error!(path = %above.display(), needed = %need.display(), "a directory above the launcher's socket holds what builds use: it is not closed");
            failed.push(format!(
                "{} holds {}, which builds use",
                above.display(),
                need.display()
            ));
            break;
        }
        let holds: Vec<String> = std::fs::read_dir(&above)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        tracing::info!(path = %above.display(), ?holds, "what the directory above the launcher's socket holds");
        let unclosed = close_one(
            &above,
            owner,
            group,
            DIRECTORY_MODE,
            "a directory above the socket",
        );
        if unclosed.is_empty() && !reached() {
            tracing::info!(path = %above.display(), "the launcher's socket is closed to builds by the directory above it");
            return failed;
        }
        failed.extend(unclosed);
    }
    failed
}

/// Why no job may be taken, given what a build can reach of `socket`, and
/// whether the attested image is running (`launcher`): `None` when builds
/// are kept away from it. In the attested image a build must be refused the
/// socket, so a socket a build can reach, or one whose reach could not be
/// checked, holds the queue; anywhere else an absent socket is the usual
/// case, and one a build can reach still holds the queue.
pub fn holds_the_queue(reach: &Reach, launcher: bool) -> Option<String> {
    match reach {
        Reach::Refused => None,
        Reach::Absent if !launcher => None,
        Reach::Absent => Some("the launcher's socket is not there to check".into()),
        Reach::Reached => Some("a build can connect to the launcher's socket".into()),
        Reach::Unknown(why) => Some(format!(
            "whether a build can reach the launcher's socket is unknown: {why}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A socket a build can connect to holds the queue, in either mode; a
    /// socket refused to builds does not; and in the attested image, one
    /// that could not be checked holds it too.
    #[test]
    fn a_socket_a_build_can_reach_holds_the_queue() {
        let directory = std::env::temp_dir().join(format!("shonin-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("teeserver.sock");
        let _listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        // Run here as the build service's own user, as on a laptop: the
        // socket is reached, and the queue is held.
        assert_eq!(probe_here(&socket), REACHED);
        assert!(holds_the_queue(&Reach::Reached, true).is_some());
        assert!(holds_the_queue(&Reach::Reached, false).is_some());

        // A socket that is not there is nothing to reach, and in the attested
        // image is not taken for one refused.
        assert_eq!(probe_here(&directory.join("none.sock")), ABSENT);
        assert!(holds_the_queue(&Reach::Absent, false).is_none());
        assert!(holds_the_queue(&Reach::Absent, true).is_some());

        assert!(holds_the_queue(&Reach::Refused, true).is_none());
        assert!(holds_the_queue(&Reach::Unknown("x".into()), true).is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// After the socket is closed, its directory and the socket itself are
    /// their owner's alone; and run as root, a child run as the build user
    /// that could connect before can't after, while root still can.
    #[test]
    fn a_closed_socket_is_refused_to_the_build_user() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory =
            std::env::temp_dir().join(format!("shonin-probe-restrict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("teeserver.sock");
        let _listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        // Open to anyone, as the launcher may hand it over.
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o777)).unwrap();

        let root = rustix::process::geteuid().is_root();
        if root {
            assert_eq!(
                child_probe(&socket),
                REACHED,
                "the build user reaches the open socket"
            );
        }

        // Root in the image; here, whoever runs the test, who may hand a file
        // only to themselves.
        let owner = rustix::process::geteuid().as_raw();
        let group = rustix::process::getegid().as_raw();
        let failed = close(&socket, owner, group, &directory, &[], || {
            root && child_probe(&socket) == REACHED
        });
        assert!(failed.is_empty(), "{failed:?}");
        for (path, mode) in [(&directory, DIRECTORY_MODE), (&socket, SOCKET_MODE)] {
            let metadata = std::fs::metadata(path).unwrap();
            assert_eq!(metadata.mode() & 0o7777, mode, "{}", path.display());
            assert_eq!((metadata.uid(), metadata.gid()), (owner, group));
        }

        if root {
            assert_eq!(
                child_probe(&socket),
                REFUSED,
                "the build user is refused it"
            );
            assert_eq!(probe_here(&socket), REACHED, "root still reaches it");
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// A tree like the attested image's: `top/run/launcher/teeserver.sock`,
    /// every part open to anyone, beside `top/run/other`. Returns the
    /// socket, its listener and the directories, nearest first.
    fn a_tree(name: &str) -> (PathBuf, std::os::unix::net::UnixListener, [PathBuf; 3]) {
        use std::os::unix::fs::PermissionsExt;

        // As the closing names them, links resolved.
        let top = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("shonin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        let run = top.join("run");
        let launcher = run.join("launcher");
        std::fs::create_dir_all(&launcher).unwrap();
        std::fs::create_dir_all(run.join("other")).unwrap();
        let socket = launcher.join("tee.sock");
        let listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        for path in [&top, &run, &launcher, &socket] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
        }
        (socket, listening, [launcher, run, top])
    }

    /// Whether another user could reach `socket` by its modes alone: search
    /// on every directory from below `top` down to it, and write on it.
    fn open_to_others(socket: &Path, top: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;

        let mode = |path: &Path| std::fs::metadata(path).unwrap().mode();
        mode(socket) & 0o002 != 0
            && socket
                .ancestors()
                .skip(1)
                .take_while(|above| *above != top)
                .all(|above| mode(above) & 0o001 != 0)
    }

    /// Closing as the image's read-only mount allows: the socket and its
    /// directory refuse every change.
    fn read_only_below<'a>(
        mount: &'a Path,
    ) -> impl Fn(&Path, u32, u32, u32, &str) -> Vec<String> + 'a {
        move |path, owner, group, mode, what| {
            if path.starts_with(mount) {
                vec![format!(
                    "chmod {mode:04o} {}: Read-only file system",
                    path.display()
                )]
            } else {
                close_one(path, owner, group, mode, what)
            }
        }
    }

    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }

    /// Where the socket and its directory are on a read-only mount, the
    /// nearest directory above them that can be changed is closed to all
    /// but its owner, with no search left to others; nothing above it, nor
    /// beside it, is changed. Run as root, the build user that reached the
    /// socket before can't after, while root still can.
    #[test]
    fn a_socket_on_a_read_only_mount_is_closed_by_the_nearest_directory_above_it() {
        use std::os::unix::fs::MetadataExt;

        let (socket, _listening, [launcher, run, top]) = a_tree("up1");
        let beside = mode_of(&run.join("other"));
        let root = rustix::process::geteuid().is_root();
        let owner = rustix::process::geteuid().as_raw();
        let group = rustix::process::getegid().as_raw();
        if root {
            assert_eq!(
                child_probe(&socket),
                REACHED,
                "the build user reaches the open socket"
            );
        }
        let failed = close_with(
            &socket,
            owner,
            group,
            &top,
            &[],
            || {
                if root {
                    child_probe(&socket) == REACHED
                } else {
                    open_to_others(&socket, &top)
                }
            },
            read_only_below(&launcher),
        );
        // The read-only steps are said, and the one above closed the path.
        assert_eq!(failed.len(), 2, "{failed:?}");
        assert_eq!(mode_of(&run), DIRECTORY_MODE);
        let metadata = std::fs::metadata(&run).unwrap();
        assert_eq!((metadata.uid(), metadata.gid()), (owner, group));
        assert_eq!(
            mode_of(&launcher),
            0o777,
            "the read-only directory was changed"
        );
        assert_eq!(
            mode_of(&top),
            0o777,
            "a directory above the one closed was changed"
        );
        assert_eq!(
            mode_of(&run.join("other")),
            beside,
            "a directory beside the socket's was changed"
        );
        assert!(!open_to_others(&socket, &top));
        if root {
            assert_eq!(
                child_probe(&socket),
                REFUSED,
                "the build user is refused it"
            );
            assert_eq!(probe_here(&socket), REACHED, "root still reaches it");
        }
        let _ = std::fs::remove_dir_all(&top);
    }

    /// A directory above the socket that holds what builds use is not
    /// closed, nor is anything above it, and the top is never closed: the
    /// socket is left reachable, and that is said.
    #[test]
    fn no_directory_holding_what_builds_use_nor_the_top_is_closed() {
        let (socket, _listening, [launcher, run, top]) = a_tree("up2");
        let owner = rustix::process::geteuid().as_raw();
        let group = rustix::process::getegid().as_raw();
        let reached = || open_to_others(&socket, &top);

        let failed = close_with(
            &socket,
            owner,
            group,
            &top,
            &[run.join("other")],
            reached,
            read_only_below(&launcher),
        );
        assert_eq!(mode_of(&run), 0o777, "a directory builds use was closed");
        assert_eq!(mode_of(&top), 0o777, "the top was closed");
        assert!(
            failed.iter().any(|why| why.contains("which builds use")),
            "{failed:?}"
        );
        assert!(open_to_others(&socket, &top));

        // Every directory below the top read-only too: the top is still
        // left as it is.
        let failed = close_with(
            &socket,
            owner,
            group,
            &top,
            &[],
            reached,
            read_only_below(&run),
        );
        assert_eq!(failed.len(), 3, "{failed:?}");
        assert_eq!(mode_of(&top), 0o777, "the top was closed");
        assert!(open_to_others(&socket, &top));
        let _ = std::fs::remove_dir_all(&top);
    }

    /// The environment variable that has [`a_child_probing_a_socket_exits_with_what_it_found`] probe a socket.
    const CHILD_PROBES: &str = "SHONIN_TEST_PROBE_SOCKET";

    /// Run this test binary again as the build user, probing `socket`, and
    /// return how it exited.
    fn child_probe(socket: &Path) -> i32 {
        use std::os::unix::process::CommandExt;

        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "isolation::tests::a_child_probing_a_socket_exits_with_what_it_found",
                "--nocapture",
            ])
            .env(CHILD_PROBES, socket)
            .uid(BUILD_USER)
            .gid(BUILD_USER)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .code()
            .unwrap()
    }

    /// The child [`child_probe`] runs: given a socket, it exits with what
    /// [`probe_here`] found; otherwise it does nothing.
    #[test]
    fn a_child_probing_a_socket_exits_with_what_it_found() {
        if let Some(socket) = std::env::var_os(CHILD_PROBES) {
            std::process::exit(probe_here(Path::new(&socket)));
        }
    }

    /// A socket only its owner may use is refused to anyone else, which is
    /// what the build user is to the root-owned launcher socket.
    #[test]
    fn a_socket_without_permission_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let directory =
            std::env::temp_dir().join(format!("shonin-probe-closed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("teeserver.sock");
        let _listening = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root connects whatever the mode says; anyone else is refused.
        if !rustix::process::geteuid().is_root() {
            assert_eq!(probe_here(&socket), REFUSED);
        }
        let _ = std::fs::remove_dir_all(&directory);
    }
}
