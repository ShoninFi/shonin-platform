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
