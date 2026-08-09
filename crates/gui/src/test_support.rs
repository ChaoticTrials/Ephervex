#![cfg(test)]

use std::path::PathBuf;

use chrono::Utc;

pub(crate) fn process_evidence() -> ephervex_core::ProcessEvidence {
    ephervex_core::ProcessEvidence {
        role: ephervex_core::ProcessRole::Supervisor,
        pid: 1,
        boot_id: "test-boot".to_owned(),
        start_ticks: 1,
        executable: PathBuf::from("/usr/bin/java"),
        executable_device: None,
        executable_inode: None,
        started_at: Utc::now(),
    }
}
