//! Discovering opencode containers and giving them stable slot numbers.
//!
//! Containers are found with `podman ps --format json` and filtered by name:
//! `opencode` or `opencode-<something>`. Their main process ID comes from
//! `podman inspect`, which reports the host-visible PID - that PID is the handle
//! used to reach the container's namespaces.
//!
//! Slots are assigned by **creation time, oldest first**. That is stable across
//! repeated runs of this program, which is what matters for a keypad button
//! pointing at a fixed slot. Terminating a container does renumber the ones
//! created after it; that was accepted deliberately. Container ID breaks ties, so
//! two containers created in the same second cannot swap places between runs.
//!
//! Podman labels were considered and rejected: they are immutable after container
//! creation, so this program could not assign them itself even if asked to.

use std::process::Command;

use serde::Deserialize;

/// Default pattern for an opencode container name: `opencode` or `opencode-*`.
const NAME_PREFIX: &str = "opencode";

/// One discovered container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Container {
    /// Slot number, 1-based, assigned by creation order.
    pub slot: usize,
    /// Container name as podman reports it, with no leading slash.
    pub name: String,
    /// Container ID, used only as a tiebreaker and for diagnostics.
    pub id: String,
    /// Creation time as reported by podman, in seconds since the epoch.
    pub created: i64,
    /// Host-visible PID of the container's main process, once known.
    pub pid: Option<i32>,
}

/// A container as it appears in `podman ps --format json`.
///
/// Field names vary in case across podman versions, and `Names` is a list. Only
/// what is needed is deserialised, so unrelated schema churn cannot break this.
#[derive(Debug, Deserialize)]
struct PsEntry {
    #[serde(alias = "ID", alias = "Id", alias = "id")]
    id: String,
    #[serde(alias = "Names", alias = "names")]
    names: Vec<String>,
    /// Unix seconds. Podman also emits `CreatedAt` as a string, which is not
    /// used: the numeric field is unambiguous.
    #[serde(alias = "Created", alias = "created")]
    created: i64,
}

/// True if a container name identifies an opencode instance.
///
/// Accepts exactly `opencode`, or `opencode-` followed by at least one character.
/// Deliberately does not accept `opencodex` or `my-opencode`, so unrelated
/// containers are never probed.
pub fn is_opencode_name(name: &str) -> bool {
    if name == NAME_PREFIX {
        return true;
    }
    match name.strip_prefix(NAME_PREFIX) {
        Some(rest) => rest.starts_with('-') && rest.len() > 1,
        None => false,
    }
}

/// Parses `podman ps --format json` output into slot-numbered containers.
///
/// Entries whose names do not identify opencode are dropped. The result is sorted
/// by creation time, then by container ID, and numbered from 1.
pub fn parse_ps(json: &str) -> Result<Vec<Container>, String> {
    let entries: Vec<PsEntry> =
        serde_json::from_str(json).map_err(|e| format!("cannot parse podman ps output: {e}"))?;

    let mut found: Vec<Container> = entries
        .into_iter()
        .filter_map(|entry| {
            // Podman reports a list of names; the first is the primary one.
            let name = entry
                .names
                .iter()
                .find(|n| is_opencode_name(n.trim_start_matches('/')))?
                .trim_start_matches('/')
                .to_string();
            Some(Container {
                slot: 0,
                name,
                id: entry.id,
                created: entry.created,
                pid: None,
            })
        })
        .collect();

    // Creation order, with the ID as a deterministic tiebreaker so same-second
    // creations cannot swap between runs.
    found.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
    for (index, container) in found.iter_mut().enumerate() {
        container.slot = index + 1;
    }
    Ok(found)
}

/// Parses the batched output of `podman inspect --format '{{.Name}} {{.State.Pid}}'`.
///
/// One `name pid` pair per line. Unparseable lines are skipped: a container that
/// stopped between `ps` and `inspect` should not fail the whole run.
pub fn parse_pids(output: &str) -> Vec<(String, i32)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?.trim_start_matches('/').to_string();
            let pid: i32 = parts.next()?.parse().ok()?;
            // A stopped container reports PID 0.
            if pid <= 0 {
                return None;
            }
            Some((name, pid))
        })
        .collect()
}

/// Attaches PIDs to containers, dropping any whose PID could not be determined.
pub fn attach_pids(containers: &mut Vec<Container>, pids: &[(String, i32)]) {
    for container in containers.iter_mut() {
        container.pid = pids
            .iter()
            .find(|(name, _)| *name == container.name)
            .map(|(_, pid)| *pid);
    }
    containers.retain(|c| c.pid.is_some());
}

/// Runs `podman` with the given arguments and returns stdout.
fn podman(args: &[&str]) -> Result<String, String> {
    let output = Command::new("podman")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run podman: {e} (is podman installed and on PATH?)"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("podman {} failed: {stderr}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Discovers every running opencode container, slot-numbered and with PIDs.
///
/// Two podman invocations in total, regardless of how many containers there are:
/// one `ps`, then one batched `inspect`.
pub fn discover() -> Result<Vec<Container>, String> {
    let ps = podman(&["ps", "--format", "json"])?;
    let mut containers = parse_ps(&ps)?;
    if containers.is_empty() {
        return Ok(containers);
    }

    let mut args: Vec<&str> = vec!["inspect", "--format", "{{.Name}} {{.State.Pid}}"];
    for container in &containers {
        args.push(&container.id);
    }
    let inspected = podman(&args)?;
    attach_pids(&mut containers, &parse_pids(&inspected));
    Ok(containers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only exact opencode names are recognised.
    #[test]
    fn recognises_opencode_names() {
        assert!(is_opencode_name("opencode"));
        assert!(is_opencode_name("opencode-web"));
        assert!(is_opencode_name("opencode-1"));
        assert!(is_opencode_name("opencode-a-b-c"));
    }

    /// Names that merely resemble opencode are not probed.
    #[test]
    fn rejects_lookalike_names() {
        assert!(!is_opencode_name("opencodex"));
        assert!(!is_opencode_name("my-opencode"));
        assert!(!is_opencode_name("opencode_web"));
        assert!(!is_opencode_name("openconnect"));
        assert!(!is_opencode_name("opencode-"));
        assert!(!is_opencode_name(""));
        assert!(!is_opencode_name("OPENCODE"));
    }

    /// Slots follow creation order, oldest first, regardless of listing order.
    #[test]
    fn numbers_slots_by_creation_time() {
        let json = r#"[
          {"Id":"ccc","Names":["opencode-third"],"Created":300},
          {"Id":"aaa","Names":["opencode-first"],"Created":100},
          {"Id":"bbb","Names":["opencode-second"],"Created":200}
        ]"#;
        let got = parse_ps(json).unwrap();
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["opencode-first", "opencode-second", "opencode-third"]);
        assert_eq!(got.iter().map(|c| c.slot).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    /// Same-second creations are ordered by container ID, so the numbering cannot
    /// differ between two runs.
    #[test]
    fn breaks_creation_ties_by_id_deterministically() {
        let json = r#"[
          {"Id":"zzz","Names":["opencode-z"],"Created":100},
          {"Id":"aaa","Names":["opencode-a"],"Created":100},
          {"Id":"mmm","Names":["opencode-m"],"Created":100}
        ]"#;
        let first = parse_ps(json).unwrap();
        let names: Vec<&str> = first.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["opencode-a", "opencode-m", "opencode-z"]);

        // Re-parsing a differently ordered listing gives identical slots.
        let reordered = r#"[
          {"Id":"mmm","Names":["opencode-m"],"Created":100},
          {"Id":"zzz","Names":["opencode-z"],"Created":100},
          {"Id":"aaa","Names":["opencode-a"],"Created":100}
        ]"#;
        assert_eq!(parse_ps(reordered).unwrap(), first);
    }

    /// Non-opencode containers are filtered out and do not consume slots.
    #[test]
    fn ignores_unrelated_containers() {
        let json = r#"[
          {"Id":"aaa","Names":["postgres"],"Created":100},
          {"Id":"bbb","Names":["opencode-web"],"Created":200},
          {"Id":"ccc","Names":["openconnect"],"Created":300}
        ]"#;
        let got = parse_ps(json).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "opencode-web");
        assert_eq!(got[0].slot, 1);
    }

    /// Podman's alternative field spellings are all accepted.
    #[test]
    fn accepts_podman_field_name_variants() {
        for json in [
            r#"[{"Id":"a","Names":["opencode"],"Created":1}]"#,
            r#"[{"ID":"a","Names":["opencode"],"Created":1}]"#,
            r#"[{"id":"a","names":["opencode"],"created":1}]"#,
        ] {
            let got = parse_ps(json).unwrap();
            assert_eq!(got.len(), 1, "failed for {json}");
            assert_eq!(got[0].name, "opencode");
        }
    }

    /// Names with podman's leading slash are normalised.
    #[test]
    fn strips_leading_slash_from_names() {
        let json = r#"[{"Id":"a","Names":["/opencode-web"],"Created":1}]"#;
        let got = parse_ps(json).unwrap();
        assert_eq!(got[0].name, "opencode-web");
    }

    /// No containers is a valid, empty result rather than an error.
    #[test]
    fn empty_listing_is_not_an_error() {
        assert_eq!(parse_ps("[]").unwrap(), Vec::new());
    }

    /// Malformed podman output is reported rather than silently treated as empty.
    #[test]
    fn malformed_listing_is_an_error() {
        assert!(parse_ps("").is_err());
        assert!(parse_ps("not json").is_err());
        assert!(parse_ps(r#"{"Id":"a"}"#).is_err());
    }

    /// PID lines parse into name/PID pairs.
    #[test]
    fn parses_inspect_pid_output() {
        let out = "/opencode-web 1234\n/opencode-api 5678\n";
        assert_eq!(
            parse_pids(out),
            vec![
                ("opencode-web".to_string(), 1234),
                ("opencode-api".to_string(), 5678)
            ]
        );
    }

    /// A stopped container reports PID 0 and is skipped.
    #[test]
    fn skips_zero_and_malformed_pids() {
        let out = "/opencode-a 0\n/opencode-b notanumber\nbroken\n\n/opencode-c 42\n";
        assert_eq!(parse_pids(out), vec![("opencode-c".to_string(), 42)]);
    }

    /// PIDs are matched onto containers by name.
    #[test]
    fn attaches_pids_by_name() {
        let mut containers = parse_ps(
            r#"[
              {"Id":"aaa","Names":["opencode-web"],"Created":100},
              {"Id":"bbb","Names":["opencode-api"],"Created":200}
            ]"#,
        )
        .unwrap();
        attach_pids(
            &mut containers,
            &[("opencode-api".to_string(), 22), ("opencode-web".to_string(), 11)],
        );
        assert_eq!(containers[0].pid, Some(11));
        assert_eq!(containers[1].pid, Some(22));
    }

    /// A container with no PID is dropped, but the slots already assigned to the
    /// survivors are left alone - renumbering here would defeat the point.
    #[test]
    fn drops_containers_without_a_pid_keeping_slots() {
        let mut containers = parse_ps(
            r#"[
              {"Id":"aaa","Names":["opencode-web"],"Created":100},
              {"Id":"bbb","Names":["opencode-api"],"Created":200},
              {"Id":"ccc","Names":["opencode-db"],"Created":300}
            ]"#,
        )
        .unwrap();
        attach_pids(
            &mut containers,
            &[("opencode-web".to_string(), 11), ("opencode-db".to_string(), 33)],
        );
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].slot, 1);
        assert_eq!(containers[1].slot, 3);
    }
}
