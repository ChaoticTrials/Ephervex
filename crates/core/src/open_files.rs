//! The number of files this process may hold open at once.

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// What the limit is raised to when the system permits more.
///
/// Not the hard limit, which is commonly a million: a child process inherits
/// whatever is set here, and some of them size buffers by it.
const WANTED: u64 = 65_536;

/// Raises this process's open-file limit toward what the system already allows.
///
/// A desktop session starts an application with a soft limit of 1024 while
/// permitting far more, and a terminal in the same session often does not. That
/// difference decided whether installing a Minecraft version's assets worked,
/// which is not a thing an application should let its launcher choose for it.
///
/// Returns the limit in force afterwards. Never lowers one, and reports a
/// failure to the caller rather than treating it as fatal: the limit is
/// headroom, not a requirement.
pub fn raise_limit() -> Result<u64, rustix::io::Errno> {
    let limit = getrlimit(Resource::Nofile);
    let hard = limit.maximum.unwrap_or(u64::MAX);
    let current = limit.current.unwrap_or(0);
    let wanted = WANTED.min(hard);
    if current >= wanted {
        return Ok(current);
    }
    setrlimit(
        Resource::Nofile,
        Rlimit {
            current: Some(wanted),
            maximum: limit.maximum,
        },
    )?;
    Ok(wanted)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Headroom, never a reduction: a caller that already has more keeps it.
    #[test]
    fn raising_the_limit_never_lowers_one_that_is_already_higher() {
        let before = getrlimit(Resource::Nofile);
        let raised = raise_limit().expect("the limit can be read and set");
        let after = getrlimit(Resource::Nofile);

        assert!(raised >= before.current.unwrap_or(0));
        assert_eq!(after.current, Some(raised));
        // Nothing is asked for beyond what the system already permits.
        if let Some(hard) = after.maximum {
            assert!(raised <= hard);
        }
        assert_eq!(after.maximum, before.maximum, "the hard limit is untouched");
    }
}
