#![cfg(test)]

use super::enhanced_terminal_tool::{
    command_is_dangerous, jobs, new_job_id, JobRecord, DEFAULT_OUTPUT_LIMIT,
};
use anyhow::Result;
use std::time::{Duration, SystemTime};

fn with_clean_jobs<T>(f: impl FnOnce() -> T) -> T {
    let mut map = jobs()
        .lock()
        .expect("job registry mutex should not be poisoned");
    map.clear();
    drop(map);
    f()
}

// Replicates the runtime_secs calculation used by the status/list tools.
fn compute_runtime_secs(rec: &JobRecord, now: SystemTime) -> u64 {
    match rec.finished_at {
        Some(finish) => finish
            .duration_since(rec.started_at)
            .ok()
            .map(|d| d.as_secs())
            .unwrap_or(0),
        None => now
            .duration_since(rec.started_at)
            .ok()
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

#[test]
fn job_id_format_and_uniqueness() -> Result<()> {
    with_clean_jobs(|| {
        let id1 = new_job_id();
        let id2 = new_job_id();

        assert!(id1.starts_with("enhterm-job-"));
        assert!(id2.starts_with("enhterm-job-"));
        assert_ne!(id1, id2);
    });

    Ok(())
}

#[test]
fn cancellation_flag_flow() -> Result<()> {
    with_clean_jobs(|| {
        let jid = new_job_id();
        let started = SystemTime::now();

        {
            let mut map = jobs()
                .lock()
                .expect("job registry mutex should not be poisoned");
            map.insert(
                jid.clone(),
                JobRecord {
                    command: "sleep 10".into(),
                    started_at: started,
                    finished_at: None,
                    exit_code: None,
                    success: false,
                    used_sudo: false,
                    output: String::new(),
                    truncated: false,
                    full_output: String::new(),
                    canceled: false,
                    dangerous: false,
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }

        // Simulate best-effort cancellation
        {
            let mut map = jobs()
                .lock()
                .expect("job registry mutex should not be poisoned");
            let rec = map.get_mut(&jid).expect("record must exist");
            rec.canceled = true;
        }

        {
            let map = jobs()
                .lock()
                .expect("job registry mutex should not be poisoned");
            let rec = map.get(&jid).expect("record must exist");
            assert!(rec.canceled, "cancellation flag should be set");
            assert!(rec.finished_at.is_none(), "still running (not finished)");
        }
    });

    Ok(())
}

#[test]
fn runtime_secs_running_and_finished() -> Result<()> {
    with_clean_jobs(|| {
        let now = SystemTime::now();

        // Running job: started 2s ago
        let running = JobRecord {
            command: "sleep 10".into(),
            started_at: now - Duration::from_secs(2),
            finished_at: None,
            exit_code: None,
            success: false,
            used_sudo: false,
            output: String::new(),
            truncated: false,
            full_output: String::new(),
            canceled: false,
            dangerous: false,
            #[cfg(unix)]
            pid: None,
        };
        let run_secs = compute_runtime_secs(&running, now);
        assert!(
            run_secs >= 2 && run_secs <= 3,
            "running job runtime_secs should be about 2s, got {}",
            run_secs
        );

        // Finished job: started 3s ago, finished 1s ago => runtime ~2s
        let finished = JobRecord {
            command: "echo done".into(),
            started_at: now - Duration::from_secs(3),
            finished_at: Some(now - Duration::from_secs(1)),
            exit_code: Some(0),
            success: true,
            used_sudo: false,
            output: "done\n".into(),
            truncated: false,
            full_output: "done\n".into(),
            canceled: false,
            dangerous: false,
            #[cfg(unix)]
            pid: None,
        };
        let fin_secs = compute_runtime_secs(&finished, now);
        assert!(
            fin_secs >= 1 && fin_secs <= 3,
            "finished job runtime_secs should be about 2s, got {}",
            fin_secs
        );
    });

    Ok(())
}

#[test]
fn dangerous_pattern_detection() -> Result<()> {
    // "why": ensure denylist core pattern detection remains conservative
    assert!(
        command_is_dangerous("sudo rm -rf /"),
        "rm -rf / must be considered dangerous"
    );
    assert!(
        command_is_dangerous(":/(){ :|:& };:"),
        "fork bomb pattern must be considered dangerous"
    );
    assert!(
        !command_is_dangerous("echo 'hello world'"),
        "simple echo must not be considered dangerous"
    );

    Ok(())
}

#[test]
fn registry_insert_and_count() -> Result<()> {
    with_clean_jobs(|| {
        // Insert two jobs and verify registry reflects both
        let jid1 = new_job_id();
        let jid2 = new_job_id();
        let started = SystemTime::now();

        {
            let mut map = jobs()
                .lock()
                .expect("job registry mutex should not be poisoned");
            map.insert(
                jid1.clone(),
                JobRecord {
                    command: "sleep 1".into(),
                    started_at: started,
                    finished_at: None,
                    exit_code: None,
                    success: false,
                    used_sudo: false,
                    output: String::new(),
                    truncated: false,
                    full_output: String::new(),
                    canceled: false,
                    dangerous: false,
                    #[cfg(unix)]
                    pid: None,
                },
            );
            map.insert(
                jid2.clone(),
                JobRecord {
                    command: "echo ok".into(),
                    started_at: started,
                    finished_at: Some(started),
                    exit_code: Some(0),
                    success: true,
                    used_sudo: false,
                    output: "ok\n".into(),
                    truncated: false,
                    full_output: "ok\n".into(),
                    canceled: false,
                    dangerous: false,
                    #[cfg(unix)]
                    pid: None,
                },
            );
        }

        let map = jobs()
            .lock()
            .expect("job registry mutex should not be poisoned");
        assert_eq!(map.len(), 2, "registry should contain exactly two jobs");
        assert!(map.contains_key(&jid1));
        assert!(map.contains_key(&jid2));
    });

    Ok(())
}

#[test]
fn preview_truncation_flag_behavior() -> Result<()> {
    // "why": ensure truncation control is consistent with the default limits contract
    let mut rec = JobRecord {
        command: "generate long output".into(),
        started_at: SystemTime::now(),
        finished_at: None,
        exit_code: None,
        success: false,
        used_sudo: false,
        output: String::new(),
        truncated: false,
        full_output: String::new(),
        canceled: false,
        dangerous: false,
        #[cfg(unix)]
        pid: None,
    };

    // Simulate streaming accumulating more than DEFAULT_OUTPUT_LIMIT bytes
    rec.full_output = "A".repeat(DEFAULT_OUTPUT_LIMIT + 10);
    // Simulate logic: preview is truncated at boundary, truncated flag true
    let mut preview = rec.full_output.clone();
    let mut end_ix = DEFAULT_OUTPUT_LIMIT;
    while !preview.is_char_boundary(end_ix) && end_ix > 0 {
        end_ix -= 1;
    }
    preview.truncate(end_ix);
    rec.output = preview;
    rec.truncated = true;

    assert!(
        rec.output.len() <= DEFAULT_OUTPUT_LIMIT,
        "preview must not exceed DEFAULT_OUTPUT_LIMIT"
    );
    assert!(
        rec.truncated,
        "truncated flag must be true when preview is cut"
    );

    Ok(())
}
