//! Include/exclude/type filtering and the xargs-style argument partition.
//!
//! Filtering matches pre-commit's `Classifier`: top-level patterns, then hook
//! patterns, then `types` (AND), `types_or` (OR), `exclude_types`. Patterns use
//! the linear-time `regex` crate.

use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;

use crate::identify;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionError {
    pub argument: String,
}

/// Drop paths that do not exist, normalize slashes, then apply include/exclude.
pub fn existing_paths<'a>(root: &Path, names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        let normalized = normalize_slashes(name);
        if path_exists(root, &normalized) {
            out.push(normalized);
        }
    }
    out
}

pub fn filter_by_include_exclude<'a>(
    names: impl IntoIterator<Item = &'a str>,
    include: &Regex,
    exclude: &Regex,
) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| include.is_match(name) && !exclude.is_match(name))
        .map(str::to_string)
        .collect()
}

pub fn filter_by_types(
    root: &Path,
    names: &[String],
    types: &[String],
    types_or: &[String],
    exclude_types: &[String],
    git_modes: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        let path = root.join(name);
        let mode = git_modes.get(name).map(String::as_str);
        let Ok(tags) = identify::classify(&path, mode) else {
            continue;
        };
        let tag_list: Vec<&str> = tags.iter().copied().collect();
        if tags_match(&tag_list, types, types_or, exclude_types) {
            out.push(name.clone());
        }
    }
    out
}

/// `types` is a conjunction, `types_or` a disjunction, `exclude_types` a rejection.
/// An empty `types_or` does not constrain the file.
pub fn tags_match(
    tags: &[&str],
    types: &[String],
    types_or: &[String],
    exclude_types: &[String],
) -> bool {
    if !types.iter().all(|tag| tags.iter().any(|have| *have == tag)) {
        return false;
    }
    if !types_or.is_empty()
        && !types_or
            .iter()
            .any(|tag| tags.iter().any(|have| *have == tag))
    {
        return false;
    }
    !exclude_types
        .iter()
        .any(|tag| tags.iter().any(|have| *have == tag))
}

pub fn normalize_slashes(path: &str) -> String {
    path.replace('\\', "/")
}

fn path_exists(root: &Path, rel: &str) -> bool {
    // `..` is not a file we will check, but existence uses the joined path
    // only after rejecting escape hatches. Missing files are dropped, matching
    // pre-commit's `lexists` filter.
    if rel.contains('\0') || rel.contains('\n') || Path::new(rel).is_absolute() {
        return false;
    }
    if rel.split('/').any(|part| part == "..") {
        return false;
    }
    root.join(rel).symlink_metadata().is_ok()
}

/// Partition `varargs` the way pre-commit's `xargs.partition` does.
///
/// `max_length` is the command-line budget. `target_concurrency` of 0 is treated as 1.
/// Length is UTF-8 bytes on non-Windows and UTF-16 code units on Windows, which is
/// the encoding pre-commit uses on those platforms (`sys.getfilesystemencoding()` is
/// approximated as UTF-8).
pub fn partition(
    cmd: &[String],
    varargs: &[String],
    target_concurrency: usize,
    max_length: usize,
) -> Result<Vec<Vec<String>>, PartitionError> {
    let target = target_concurrency.max(1);
    let max_args = (varargs.len().div_ceil(target)).max(4);
    let mut pending: Vec<&String> = varargs.iter().rev().collect();
    let mut batches = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut total = command_length(cmd) + 1;

    while let Some(arg) = pending.pop() {
        let arg_length = command_length(std::slice::from_ref(arg)) + 1;
        if total + arg_length <= max_length && current.len() < max_args {
            current.push(arg.clone());
            total += arg_length;
        } else if current.is_empty() {
            return Err(PartitionError {
                argument: arg.clone(),
            });
        } else {
            let mut batch = cmd.to_vec();
            batch.append(&mut current);
            batches.push(batch);
            total = command_length(cmd) + 1;
            pending.push(arg);
        }
    }
    let mut batch = cmd.to_vec();
    batch.extend(current);
    batches.push(batch);
    Ok(batches)
}

/// POSIX budget is `arg_max - 2048 - environ_size`, clamped to `[4096, 131072]`.
/// Windows uses `32768 - 2048` and ignores `arg_max`. Reading `SC_ARG_MAX` is a
/// libc call this crate does not make (`unsafe_code` is forbidden); pass the
/// limit in when a caller has it.
pub fn command_budget(arg_max: usize, environ_size: usize) -> usize {
    #[cfg(windows)]
    {
        let _ = (arg_max, environ_size);
        32_768usize.saturating_sub(2048)
    }
    #[cfg(not(windows))]
    {
        arg_max
            .saturating_sub(2048)
            .saturating_sub(environ_size)
            .clamp(4096, 131_072)
    }
}

pub fn command_length(parts: &[String]) -> usize {
    let joined = parts.join(" ");
    #[cfg(windows)]
    {
        joined.encode_utf16().count()
    }
    #[cfg(not(windows))]
    {
        joined.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn empty_include_matches_all_and_default_exclude_matches_none() {
        let include = Regex::new("").unwrap();
        let exclude = Regex::new("^$").unwrap();
        let names = ["a.py", "dir/b.rs", ""];
        let kept = filter_by_include_exclude(names, &include, &exclude);
        assert_eq!(kept, vec!["a.py".to_string(), "dir/b.rs".to_string()]);
    }

    #[test]
    fn partition_preserves_arguments_and_budget() {
        let cmd = vec!["hook".to_string()];
        let args: Vec<String> = (0..10).map(|n| format!("f{n}")).collect();
        let batches = partition(&cmd, &args, 2, 24).unwrap();
        let mut flat = Vec::new();
        for batch in &batches {
            assert!(command_length(batch) <= 24);
            assert_eq!(batch[0], "hook");
            flat.extend(batch.iter().skip(1).cloned());
        }
        assert_eq!(flat, args);
    }

    #[test]
    fn command_budget_clamps() {
        #[cfg(not(windows))]
        {
            assert_eq!(command_budget(0, 0), 4096);
            assert_eq!(command_budget(200_000, 0), 131_072);
            assert_eq!(command_budget(20_000, 100), 20_000 - 2048 - 100);
        }
        #[cfg(windows)]
        {
            assert_eq!(command_budget(0, 0), 32_768 - 2048);
        }
    }

    #[test]
    fn partition_errors_when_one_argument_does_not_fit() {
        let err = partition(&["cmd".to_string()], &["toolong".to_string()], 1, 4).unwrap_err();
        assert_eq!(err.argument, "toolong");
    }

    proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]

        #[test]
        fn partition_is_a_cover(args in proptest::collection::vec("[a-z]{1,4}", 0..12)) {
            let cmd = vec!["c".to_string()];
            let owned: Vec<String> = args;
            let batches = partition(&cmd, &owned, 3, 64).unwrap();
            let mut flat = Vec::new();
            for batch in batches {
                prop_assert!(command_length(&batch) <= 64);
                flat.extend(batch.into_iter().skip(1));
            }
            prop_assert_eq!(flat, owned);
        }

        #[test]
        fn include_exclude_keeps_order(
            names in proptest::collection::vec("[A-Za-z0-9.]{0,8}", 0..8),
            include_src in prop_oneof![
                Just("".to_string()),
                Just("^$".to_string()),
                Just("a".to_string()),
                Just("[0-9]".to_string()),
                Just(".*".to_string()),
                Just("^[A-Z]".to_string()),
            ],
            exclude_src in prop_oneof![
                Just("^$".to_string()),
                Just("b".to_string()),
                Just("z".to_string()),
                Just(".*".to_string()),
            ],
        ) {
            let include = Regex::new(&include_src).unwrap();
            let exclude = Regex::new(&exclude_src).unwrap();
            let got = filter_by_include_exclude(names.iter().map(String::as_str), &include, &exclude);
            let expect: Vec<String> = names
                .iter()
                .filter(|name| include.is_match(name) && !exclude.is_match(name))
                .cloned()
                .collect();
            prop_assert_eq!(got.clone(), expect);
            let again = filter_by_include_exclude(got.iter().map(String::as_str), &include, &exclude);
            prop_assert_eq!(again, got);
        }

        #[test]
        fn type_filter_is_and_or_exclude(
            present in proptest::collection::hash_set("[a-z]{1,5}", 0..5),
            types in proptest::collection::vec("[a-z]{1,5}", 0..4),
            types_or in proptest::collection::vec("[a-z]{1,5}", 0..4),
            exclude_types in proptest::collection::vec("[a-z]{1,5}", 0..4),
        ) {
            let tags: Vec<&str> = present.iter().map(String::as_str).collect();
            let selected = tags_match(&tags, &types, &types_or, &exclude_types);
            let and_ok = types.iter().all(|tag| present.contains(tag));
            let or_ok = types_or.is_empty() || types_or.iter().any(|tag| present.contains(tag));
            let excluded = exclude_types.iter().any(|tag| present.contains(tag));
            prop_assert_eq!(selected, and_ok && or_ok && !excluded);
        }
    }
}
