use rax_core::{
    ErrorCode, ErrorPhase, GitBranchEntry, GitBranchesOutput, GitChangedFile, GitLogEntry,
    GitLogOutput, GitRemoteEntry, GitRemoteOutput, GitStatusOutput, RaxError,
};

use super::{protocol_error, strict_utf8};

pub(super) fn status(
    repository: &str,
    bytes: &[u8],
    truncated: bool,
) -> Result<GitStatusOutput, RaxError> {
    let bytes = complete_nul_prefix(bytes, truncated, "git status")?;
    let mut branch = None;
    let mut upstream = None;
    let mut ahead = 0_u64;
    let mut behind = 0_u64;
    let mut detached = false;
    let mut files = Vec::new();
    let mut records = bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());

    while let Some(raw) = records.next() {
        let record = strict_utf8(raw, "git status record")?;
        if let Some(header) = record.strip_prefix("# ") {
            if let Some(value) = header.strip_prefix("branch.head ") {
                if value == "(detached)" {
                    detached = true;
                } else {
                    branch = Some(value.to_string());
                }
            } else if let Some(value) = header.strip_prefix("branch.upstream ") {
                upstream = Some(value.to_string());
            } else if let Some(value) = header.strip_prefix("branch.ab ") {
                let mut fields = value.split_ascii_whitespace();
                ahead = parse_count(fields.next(), '+')?;
                behind = parse_count(fields.next(), '-')?;
                if fields.next().is_some() {
                    return Err(protocol_error("git branch count contained extra fields"));
                }
            }
            continue;
        }

        if let Some(rest) = record.strip_prefix("1 ") {
            let (status, path) = status_and_path(rest, 8, "ordinary")?;
            files.push(changed_file(status, path, None));
        } else if let Some(rest) = record.strip_prefix("2 ") {
            let (status, path) = status_and_path(rest, 9, "renamed")?;
            let original = records.next().ok_or_else(|| {
                protocol_error("renamed git status record omitted its original path")
            })?;
            let original = strict_utf8(original, "git status original path")?;
            files.push(changed_file(status, path, Some(original.to_string())));
        } else if let Some(rest) = record.strip_prefix("u ") {
            let (status, path) = status_and_path(rest, 10, "unmerged")?;
            files.push(changed_file(status, path, None));
        } else if let Some(path) = record.strip_prefix("? ") {
            files.push(changed_file("??", path, None));
        } else if record.starts_with("! ") {
            // Ignored entries are not changes and are intentionally omitted.
        } else {
            return Err(protocol_error("git status emitted an unknown record type"));
        }
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(GitStatusOutput {
        repository: repository.to_string(),
        branch,
        upstream,
        ahead,
        behind,
        detached,
        files,
        truncated,
    })
}

pub(super) fn log(bytes: &[u8], truncated: bool) -> Result<GitLogOutput, RaxError> {
    let bytes = complete_nul_prefix(bytes, truncated, "git log")?;
    let mut entries = Vec::new();
    for raw in bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
    {
        let record = strict_utf8(raw, "git log record")?;
        let mut fields = record.splitn(6, '\u{1f}');
        let sha = required_field(&mut fields, "sha", "git log")?;
        let parents = required_field(&mut fields, "parents", "git log")?;
        let author_name = required_field(&mut fields, "author name", "git log")?;
        let author_email = required_field(&mut fields, "author email", "git log")?;
        let timestamp = required_field(&mut fields, "timestamp", "git log")?
            .parse::<i64>()
            .map_err(|error| protocol_error(format!("git log timestamp was invalid: {error}")))?;
        let subject = required_field(&mut fields, "subject", "git log")?;
        entries.push(GitLogEntry {
            sha: sha.to_string(),
            parents: parents
                .split_ascii_whitespace()
                .map(ToOwned::to_owned)
                .collect(),
            author_name: author_name.to_string(),
            author_email: author_email.to_string(),
            timestamp,
            subject: subject.to_string(),
        });
    }
    Ok(GitLogOutput { entries, truncated })
}

pub(super) fn remotes(bytes: &[u8]) -> Result<GitRemoteOutput, RaxError> {
    let text = strict_utf8(bytes, "git remote output")?;
    let mut remotes = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (name, remainder) = line
            .split_once('\t')
            .ok_or_else(|| protocol_error("git remote -v emitted an unexpected line"))?;
        let open = remainder
            .rfind(" (")
            .ok_or_else(|| protocol_error("git remote -v emitted an unexpected direction"))?;
        let direction = remainder
            .strip_suffix(')')
            .and_then(|value| value.get(open + 2..))
            .ok_or_else(|| protocol_error("git remote direction was malformed"))?;
        remotes.push(GitRemoteEntry {
            name: name.to_string(),
            url: remainder[..open].to_string(),
            direction: direction.to_string(),
        });
    }
    remotes.sort_by(|left, right| {
        (&left.name, &left.direction, &left.url).cmp(&(&right.name, &right.direction, &right.url))
    });
    Ok(GitRemoteOutput { remotes })
}

pub(super) fn branches(bytes: &[u8]) -> Result<GitBranchesOutput, RaxError> {
    let mut branches = Vec::new();
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let mut fields = line.split(|byte| *byte == 0);
        let refname = fields
            .next()
            .ok_or_else(|| protocol_error("git branch record omitted the ref name"))?;
        let object_id = fields
            .next()
            .ok_or_else(|| protocol_error("git branch record omitted the object id"))?;
        let current = fields
            .next()
            .ok_or_else(|| protocol_error("git branch record omitted the HEAD marker"))?;
        if fields.next().is_some() {
            return Err(protocol_error("git branch record contained extra fields"));
        }
        let refname = strict_utf8(refname, "git branch name")?;
        let (remote, name) = if let Some(value) = refname.strip_prefix("refs/heads/") {
            (false, value)
        } else if let Some(value) = refname.strip_prefix("refs/remotes/") {
            (true, value)
        } else {
            continue;
        };
        branches.push(GitBranchEntry {
            name: name.to_string(),
            current: current == b"*",
            remote,
            object_id: strict_utf8(object_id, "git object id")?.to_string(),
        });
    }
    branches.sort_by(|left, right| (&left.remote, &left.name).cmp(&(&right.remote, &right.name)));
    Ok(GitBranchesOutput { branches })
}

fn status_and_path<'a>(
    record: &'a str,
    field_count: usize,
    kind: &str,
) -> Result<(&'a str, &'a str), RaxError> {
    let mut fields = record.splitn(field_count, ' ');
    let status = fields
        .next()
        .ok_or_else(|| protocol_error(format!("{kind} git status record omitted status")))?;
    let mut path = None;
    for _ in 1..field_count {
        path = fields.next();
        if path.is_none() {
            return Err(protocol_error(format!(
                "{kind} git status record was malformed"
            )));
        }
    }
    path.map(|path| (status, path))
        .ok_or_else(|| protocol_error(format!("{kind} git status record omitted path")))
}

fn changed_file(status: &str, path: &str, original_path: Option<String>) -> GitChangedFile {
    let mut chars = status.chars();
    GitChangedFile {
        path: path.to_string(),
        original_path,
        index_status: chars.next().unwrap_or('.').to_string(),
        worktree_status: chars.next().unwrap_or('.').to_string(),
    }
}

fn parse_count(value: Option<&str>, prefix: char) -> Result<u64, RaxError> {
    let value = value.ok_or_else(|| protocol_error("git branch count was missing"))?;
    value
        .strip_prefix(prefix)
        .ok_or_else(|| protocol_error("git branch count prefix was invalid"))?
        .parse::<u64>()
        .map_err(|error| protocol_error(format!("git branch count was invalid: {error}")))
}

fn required_field<'a, I>(fields: &mut I, field: &str, source: &str) -> Result<&'a str, RaxError>
where
    I: Iterator<Item = &'a str>,
{
    fields
        .next()
        .ok_or_else(|| protocol_error(format!("{source} omitted {field}")))
}

fn complete_nul_prefix<'a>(
    bytes: &'a [u8],
    truncated: bool,
    source: &str,
) -> Result<&'a [u8], RaxError> {
    if !truncated {
        if bytes.is_empty() || bytes.last() == Some(&0) {
            return Ok(bytes);
        }
        return Err(protocol_error(format!(
            "{source} did not end at a complete NUL-delimited record"
        )));
    }
    let end = bytes
        .iter()
        .rposition(|byte| *byte == 0)
        .map(|index| index + 1)
        .ok_or_else(|| {
            RaxError::new(
                ErrorCode::LimitExceeded,
                ErrorPhase::Apply,
                false,
                format!("{source} was truncated before one complete record was captured"),
            )
        })?;
    Ok(&bytes[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_nul_delimited_porcelain_v2_headers() {
        let input = b"# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\x31 M. N... 100644 100644 100644 abc abc src/lib.rs\0? new.txt\0";
        let parsed = status("", input, false).expect("parse status");
        assert_eq!(parsed.branch.as_deref(), Some("main"));
        assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
        assert_eq!(parsed.ahead, 2);
        assert_eq!(parsed.behind, 1);
        assert_eq!(parsed.files.len(), 2);
    }

    #[test]
    fn parses_rename_without_collecting_all_records() {
        let input = b"# branch.head main\0\x32 R. N... 100644 100644 100644 a a R100 new name.txt\0old name.txt\0";
        let parsed = status("", input, false).expect("parse rename");
        assert_eq!(parsed.files[0].path, "new name.txt");
        assert_eq!(
            parsed.files[0].original_path.as_deref(),
            Some("old name.txt")
        );
    }

    #[test]
    fn truncated_status_keeps_only_complete_records() {
        let input = b"# branch.head main\0? kept.txt\0? partial";
        let parsed = status("", input, true).expect("parse complete prefix");
        assert!(parsed.truncated);
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.files[0].path, "kept.txt");
    }

    #[test]
    fn rejects_output_without_a_complete_record() {
        let error = status("", b"# branch.head main", true).expect_err("missing terminator");
        assert_eq!(error.code, ErrorCode::LimitExceeded);
    }

    #[test]
    fn truncated_log_keeps_only_complete_records() {
        let input = b"a\x1f\x1fA\x1fa@example.com\x1f1\x1fsubject\0partial";
        let parsed = log(input, true).expect("parse complete prefix");
        assert!(parsed.truncated);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].sha, "a");
    }
}
