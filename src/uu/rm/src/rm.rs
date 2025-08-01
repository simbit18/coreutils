// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore (path) eacces inacc rm-r4

use clap::builder::{PossibleValue, ValueParser};
use clap::{Arg, ArgAction, Command, parser::ValueSource};
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::io::{IsTerminal, stdin};
use std::ops::BitOr;
#[cfg(not(windows))]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::MAIN_SEPARATOR;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uucore::display::Quotable;
use uucore::error::{FromIo, UError, UResult};
use uucore::parser::shortcut_value_parser::ShortcutValueParser;
use uucore::translate;

use uucore::{format_usage, os_str_as_bytes, prompt_yes, show_error};

#[derive(Debug, Error)]
enum RmError {
    #[error("{}", translate!("rm-error-missing-operand", "util_name" => uucore::execution_phrase()))]
    MissingOperand,
    #[error("{}", translate!("rm-error-cannot-remove-no-such-file", "file" => _0.quote()))]
    CannotRemoveNoSuchFile(String),
    #[error("{}", translate!("rm-error-cannot-remove-permission-denied", "file" => _0.quote()))]
    CannotRemovePermissionDenied(String),
    #[error("{}", translate!("rm-error-cannot-remove-is-directory", "file" => _0.quote()))]
    CannotRemoveIsDirectory(String),
    #[error("{}", translate!("rm-error-dangerous-recursive-operation"))]
    DangerousRecursiveOperation,
    #[error("{}", translate!("rm-error-use-no-preserve-root"))]
    UseNoPreserveRoot,
    #[error("{}", translate!("rm-error-refusing-to-remove-directory", "path" => _0))]
    RefusingToRemoveDirectory(String),
}

impl UError for RmError {}

#[derive(Eq, PartialEq, Clone, Copy)]
/// Enum, determining when the `rm` will prompt the user about the file deletion
pub enum InteractiveMode {
    /// Never prompt
    Never,
    /// Prompt once before removing more than three files, or when removing
    /// recursively.
    Once,
    /// Prompt before every removal
    Always,
    /// Prompt only on write-protected files
    PromptProtected,
}

// We implement `From` instead of `TryFrom` because clap guarantees that we only receive valid values.
//
// The `PromptProtected` variant is not supposed to be created from a string.
impl From<&str> for InteractiveMode {
    fn from(s: &str) -> Self {
        match s {
            "never" => Self::Never,
            "once" => Self::Once,
            "always" => Self::Always,
            _ => unreachable!("should be prevented by clap"),
        }
    }
}

/// Options for the `rm` command
///
/// All options are public so that the options can be programmatically
/// constructed by other crates, such as Nushell. That means that this struct
/// is part of our public API. It should therefore not be changed without good
/// reason.
///
/// The fields are documented with the arguments that determine their value.
pub struct Options {
    /// `-f`, `--force`
    pub force: bool,
    /// Iterative mode, determines when the command will prompt.
    ///
    /// Set by the following arguments:
    /// - `-i`: [`InteractiveMode::Always`]
    /// - `-I`: [`InteractiveMode::Once`]
    /// - `--interactive`: sets one of the above or [`InteractiveMode::Never`]
    /// - `-f`: implicitly sets [`InteractiveMode::Never`]
    ///
    /// If no other option sets this mode, [`InteractiveMode::PromptProtected`]
    /// is used
    pub interactive: InteractiveMode,
    #[allow(dead_code)]
    /// `--one-file-system`
    pub one_fs: bool,
    /// `--preserve-root`/`--no-preserve-root`
    pub preserve_root: bool,
    /// `-r`, `--recursive`
    pub recursive: bool,
    /// `-d`, `--dir`
    pub dir: bool,
    /// `-v`, `--verbose`
    pub verbose: bool,
    #[doc(hidden)]
    /// `---presume-input-tty`
    /// Always use `None`; GNU flag for testing use only
    pub __presume_input_tty: Option<bool>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            force: false,
            interactive: InteractiveMode::PromptProtected,
            one_fs: false,
            preserve_root: true,
            recursive: false,
            dir: false,
            verbose: false,
            __presume_input_tty: None,
        }
    }
}

static OPT_DIR: &str = "dir";
static OPT_INTERACTIVE: &str = "interactive";
static OPT_FORCE: &str = "force";
static OPT_NO_PRESERVE_ROOT: &str = "no-preserve-root";
static OPT_ONE_FILE_SYSTEM: &str = "one-file-system";
static OPT_PRESERVE_ROOT: &str = "preserve-root";
static OPT_PROMPT_ALWAYS: &str = "prompt-always";
static OPT_PROMPT_ONCE: &str = "prompt-once";
static OPT_RECURSIVE: &str = "recursive";
static OPT_VERBOSE: &str = "verbose";
static PRESUME_INPUT_TTY: &str = "-presume-input-tty";

static ARG_FILES: &str = "files";

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let matches = uu_app().try_get_matches_from(args)?;

    let files: Vec<_> = matches
        .get_many::<OsString>(ARG_FILES)
        .map(|v| v.map(OsString::as_os_str).collect())
        .unwrap_or_default();

    let force_flag = matches.get_flag(OPT_FORCE);

    if files.is_empty() && !force_flag {
        // Still check by hand and not use clap
        // Because "rm -f" is a thing
        return Err(RmError::MissingOperand.into());
    }

    // If -f(--force) is before any -i (or variants) we want prompts else no prompts
    let force_prompt_never = force_flag && {
        let force_index = matches.index_of(OPT_FORCE).unwrap_or(0);
        ![OPT_PROMPT_ALWAYS, OPT_PROMPT_ONCE, OPT_INTERACTIVE]
            .iter()
            .any(|flag| {
                matches.value_source(flag) == Some(ValueSource::CommandLine)
                    && matches.index_of(flag).unwrap_or(0) > force_index
            })
    };

    let options = Options {
        force: force_flag,
        interactive: {
            if force_prompt_never {
                InteractiveMode::Never
            } else if matches.get_flag(OPT_PROMPT_ALWAYS) {
                InteractiveMode::Always
            } else if matches.get_flag(OPT_PROMPT_ONCE) {
                InteractiveMode::Once
            } else if matches.contains_id(OPT_INTERACTIVE) {
                InteractiveMode::from(matches.get_one::<String>(OPT_INTERACTIVE).unwrap().as_str())
            } else {
                InteractiveMode::PromptProtected
            }
        },
        one_fs: matches.get_flag(OPT_ONE_FILE_SYSTEM),
        preserve_root: !matches.get_flag(OPT_NO_PRESERVE_ROOT),
        recursive: matches.get_flag(OPT_RECURSIVE),
        dir: matches.get_flag(OPT_DIR),
        verbose: matches.get_flag(OPT_VERBOSE),
        __presume_input_tty: if matches.get_flag(PRESUME_INPUT_TTY) {
            Some(true)
        } else {
            None
        },
    };
    if options.interactive == InteractiveMode::Once && (options.recursive || files.len() > 3) {
        let msg: String = format!(
            "remove {} {}{}",
            files.len(),
            if files.len() > 1 {
                "arguments"
            } else {
                "argument"
            },
            if options.recursive {
                " recursively?"
            } else {
                "?"
            }
        );
        if !prompt_yes!("{msg}") {
            return Ok(());
        }
    }

    if remove(&files, &options) {
        return Err(1.into());
    }

    Ok(())
}

pub fn uu_app() -> Command {
    Command::new(uucore::util_name())
        .version(uucore::crate_version!())
        .about(translate!("rm-about"))
        .override_usage(format_usage(&translate!("rm-usage")))
        .after_help(translate!("rm-after-help"))
        .infer_long_args(true)
        .args_override_self(true)
        .arg(
            Arg::new(OPT_FORCE)
                .short('f')
                .long(OPT_FORCE)
                .help(translate!("rm-help-force"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_PROMPT_ALWAYS)
                .short('i')
                .help(translate!("rm-help-prompt-always"))
                .overrides_with_all([OPT_PROMPT_ONCE, OPT_INTERACTIVE])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_PROMPT_ONCE)
                .short('I')
                .help(translate!("rm-help-prompt-once"))
                .overrides_with_all([OPT_PROMPT_ALWAYS, OPT_INTERACTIVE])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_INTERACTIVE)
                .long(OPT_INTERACTIVE)
                .help(translate!("rm-help-interactive"))
                .value_name("WHEN")
                .value_parser(ShortcutValueParser::new([
                    PossibleValue::new("always").alias("yes"),
                    PossibleValue::new("once"),
                    PossibleValue::new("never").alias("no").alias("none"),
                ]))
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("always")
                .overrides_with_all([OPT_PROMPT_ALWAYS, OPT_PROMPT_ONCE]),
        )
        .arg(
            Arg::new(OPT_ONE_FILE_SYSTEM)
                .long(OPT_ONE_FILE_SYSTEM)
                .help(translate!("rm-help-one-file-system"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_NO_PRESERVE_ROOT)
                .long(OPT_NO_PRESERVE_ROOT)
                .help(translate!("rm-help-no-preserve-root"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_PRESERVE_ROOT)
                .long(OPT_PRESERVE_ROOT)
                .help(translate!("rm-help-preserve-root"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_RECURSIVE)
                .short('r')
                .visible_short_alias('R')
                .long(OPT_RECURSIVE)
                .help(translate!("rm-help-recursive"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_DIR)
                .short('d')
                .long(OPT_DIR)
                .help(translate!("rm-help-dir"))
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(OPT_VERBOSE)
                .short('v')
                .long(OPT_VERBOSE)
                .help(translate!("rm-help-verbose"))
                .action(ArgAction::SetTrue),
        )
        // From the GNU source code:
        // This is solely for testing.
        // Do not document.
        // It is relatively difficult to ensure that there is a tty on stdin.
        // Since rm acts differently depending on that, without this option,
        // it'd be harder to test the parts of rm that depend on that setting.
        // In contrast with Arg::long, Arg::alias does not strip leading
        // hyphens. Therefore it supports 3 leading hyphens.
        .arg(
            Arg::new(PRESUME_INPUT_TTY)
                .long("presume-input-tty")
                .alias(PRESUME_INPUT_TTY)
                .hide(true)
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new(ARG_FILES)
                .action(ArgAction::Append)
                .value_parser(ValueParser::os_string())
                .num_args(1..)
                .value_hint(clap::ValueHint::AnyPath),
        )
}

// TODO: implement one-file-system (this may get partially implemented in walkdir)
/// Remove (or unlink) the given files
///
/// Returns true if it has encountered an error.
///
/// Behavior is determined by the `options` parameter, see [`Options`] for
/// details.
pub fn remove(files: &[&OsStr], options: &Options) -> bool {
    let mut had_err = false;

    for filename in files {
        let file = Path::new(filename);

        had_err = match file.symlink_metadata() {
            Ok(metadata) => {
                if metadata.is_dir() {
                    handle_dir(file, options)
                } else if is_symlink_dir(&metadata) {
                    remove_dir(file, options)
                } else {
                    remove_file(file, options)
                }
            }

            Err(_e) => {
                // TODO: actually print out the specific error
                // TODO: When the error is not about missing files
                // (e.g., permission), even rm -f should fail with
                // outputting the error, but there's no easy way.
                if options.force {
                    false
                } else {
                    show_error!(
                        "{}",
                        RmError::CannotRemoveNoSuchFile(filename.to_string_lossy().to_string())
                    );
                    true
                }
            }
        }
        .bitor(had_err);
    }

    had_err
}

/// Whether the given directory is empty.
///
/// `path` must be a directory. If there is an error reading the
/// contents of the directory, this returns `false`.
fn is_dir_empty(path: &Path) -> bool {
    fs::read_dir(path).is_ok_and(|mut iter| iter.next().is_none())
}

#[cfg(unix)]
fn is_readable_metadata(metadata: &Metadata) -> bool {
    let mode = metadata.permissions().mode();
    (mode & 0o400) > 0
}

/// Whether the given file or directory is readable.
#[cfg(unix)]
fn is_readable(path: &Path) -> bool {
    match fs::metadata(path) {
        Err(_) => false,
        Ok(metadata) => is_readable_metadata(&metadata),
    }
}

/// Whether the given file or directory is readable.
#[cfg(not(unix))]
fn is_readable(_path: &Path) -> bool {
    true
}

#[cfg(unix)]
fn is_writable_metadata(metadata: &Metadata) -> bool {
    let mode = metadata.permissions().mode();
    (mode & 0o200) > 0
}

/// Whether the given file or directory is writable.
#[cfg(unix)]
fn is_writable(path: &Path) -> bool {
    match fs::metadata(path) {
        Err(_) => false,
        Ok(metadata) => is_writable_metadata(&metadata),
    }
}

/// Whether the given file or directory is writable.
#[cfg(not(unix))]
fn is_writable(_path: &Path) -> bool {
    // TODO Not yet implemented.
    true
}

/// Recursively remove the directory tree rooted at the given path.
///
/// If `path` is a file or a symbolic link, just remove it. If it is a
/// directory, remove all of its entries recursively and then remove the
/// directory itself. In case of an error, print the error message to
/// `stderr` and return `true`. If there were no errors, return `false`.
fn remove_dir_recursive(path: &Path, options: &Options) -> bool {
    // Base case 1: this is a file or a symbolic link.
    //
    // The symbolic link case is important because it could be a link to
    // a directory and we don't want to recurse. In particular, this
    // avoids an infinite recursion in the case of a link to the current
    // directory, like `ln -s . link`.
    if !path.is_dir() || path.is_symlink() {
        return remove_file(path, options);
    }

    // Base case 2: this is a non-empty directory, but the user
    // doesn't want to descend into it.
    if options.interactive == InteractiveMode::Always
        && !is_dir_empty(path)
        && !prompt_descend(path)
    {
        return false;
    }

    // Special case: if we cannot access the metadata because the
    // filename is too long, fall back to try
    // `fs::remove_dir_all()`.
    //
    // TODO This is a temporary bandage; we shouldn't need to do this
    // at all. Instead of using the full path like "x/y/z", which
    // causes a `InvalidFilename` error when trying to access the file
    // metadata, we should be able to use just the last part of the
    // path, "z", and know that it is relative to the parent, "x/y".
    if let Some(s) = path.to_str() {
        if s.len() > 1000 {
            match fs::remove_dir_all(path) {
                Ok(_) => return false,
                Err(e) => {
                    let e = e.map_err_context(
                        || translate!("rm-error-cannot-remove", "file" => path.quote()),
                    );
                    show_error!("{e}");
                    return true;
                }
            }
        }
    }

    // Recursive case: this is a directory.
    let mut error = false;
    match fs::read_dir(path) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            // This is not considered an error.
        }
        Err(_) => error = true,
        Ok(iter) => {
            for entry in iter {
                match entry {
                    Err(_) => error = true,
                    Ok(entry) => {
                        let child_error = remove_dir_recursive(&entry.path(), options);
                        error = error || child_error;
                    }
                }
            }
        }
    }

    // Ask the user whether to remove the current directory.
    if options.interactive == InteractiveMode::Always && !prompt_dir(path, options) {
        return false;
    }

    // Try removing the directory itself.
    match fs::remove_dir(path) {
        Err(_) if !error && !is_readable(path) => {
            // For compatibility with GNU test case
            // `tests/rm/unread2.sh`, show "Permission denied" in this
            // case instead of "Directory not empty".
            show_error!("cannot remove {}: Permission denied", path.quote());
            error = true;
        }
        Err(e) if !error => {
            let e =
                e.map_err_context(|| translate!("rm-error-cannot-remove", "file" => path.quote()));
            show_error!("{e}");
            error = true;
        }
        Err(_) => {
            // If there has already been at least one error when
            // trying to remove the children, then there is no need to
            // show another error message as we return from each level
            // of the recursion.
        }
        Ok(_) if options.verbose => println!(
            "{}",
            translate!("rm-verbose-removed-directory", "file" => normalize(path).quote())
        ),
        Ok(_) => {}
    }

    error
}

fn handle_dir(path: &Path, options: &Options) -> bool {
    let mut had_err = false;

    let path = clean_trailing_slashes(path);
    if path_is_current_or_parent_directory(path) {
        show_error!(
            "{}",
            RmError::RefusingToRemoveDirectory(path.display().to_string())
        );
        return true;
    }

    let is_root = path.has_root() && path.parent().is_none();
    if options.recursive && (!is_root || !options.preserve_root) {
        had_err = remove_dir_recursive(path, options);
    } else if options.dir && (!is_root || !options.preserve_root) {
        had_err = remove_dir(path, options).bitor(had_err);
    } else if options.recursive {
        show_error!("{}", RmError::DangerousRecursiveOperation);
        show_error!("{}", RmError::UseNoPreserveRoot);
        had_err = true;
    } else {
        show_error!(
            "{}",
            RmError::CannotRemoveIsDirectory(path.to_string_lossy().to_string())
        );
        had_err = true;
    }

    had_err
}

/// Remove the given directory, asking the user for permission if necessary.
///
/// Returns true if it has encountered an error.
fn remove_dir(path: &Path, options: &Options) -> bool {
    // Ask the user for permission.
    if !prompt_dir(path, options) {
        return false;
    }

    // Called to remove a symlink_dir (windows) without "-r"/"-R" or "-d".
    if !options.dir && !options.recursive {
        show_error!(
            "{}",
            RmError::CannotRemoveIsDirectory(path.to_string_lossy().to_string())
        );
        return true;
    }

    // Try to remove the directory.
    match fs::remove_dir(path) {
        Ok(_) => {
            if options.verbose {
                println!(
                    "{}",
                    translate!("rm-verbose-removed-directory", "file" => normalize(path).quote())
                );
            }
            false
        }
        Err(e) => {
            let e =
                e.map_err_context(|| translate!("rm-error-cannot-remove", "file" => path.quote()));
            show_error!("{e}");
            true
        }
    }
}

fn remove_file(path: &Path, options: &Options) -> bool {
    if prompt_file(path, options) {
        match fs::remove_file(path) {
            Ok(_) => {
                if options.verbose {
                    println!(
                        "{}",
                        translate!("rm-verbose-removed", "file" => normalize(path).quote())
                    );
                }
            }
            Err(e) => {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    // GNU compatibility (rm/fail-eacces.sh)
                    show_error!(
                        "{}",
                        RmError::CannotRemovePermissionDenied(path.to_string_lossy().to_string())
                    );
                } else {
                    show_error!("cannot remove {}: {e}", path.quote());
                }
                return true;
            }
        }
    }

    false
}

fn prompt_dir(path: &Path, options: &Options) -> bool {
    // If interactive is Never we never want to send prompts
    if options.interactive == InteractiveMode::Never {
        return true;
    }

    // We can't use metadata.permissions.readonly for directories because it only works on files
    // So we have to handle whether a directory is writable manually
    if let Ok(metadata) = fs::metadata(path) {
        handle_writable_directory(path, options, &metadata)
    } else {
        true
    }
}

fn prompt_file(path: &Path, options: &Options) -> bool {
    // If interactive is Never we never want to send prompts
    if options.interactive == InteractiveMode::Never {
        return true;
    }
    // If interactive is Always we want to check if the file is symlink to prompt the right message
    if options.interactive == InteractiveMode::Always {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if metadata.is_symlink() {
                return prompt_yes!("remove symbolic link {}?", path.quote());
            }
        }
    }

    let Ok(metadata) = fs::metadata(path) else {
        return true;
    };

    if options.interactive == InteractiveMode::Always && is_writable(path) {
        return if metadata.len() == 0 {
            prompt_yes!("remove regular empty file {}?", path.quote())
        } else {
            prompt_yes!("remove file {}?", path.quote())
        };
    }
    prompt_file_permission_readonly(path, options)
}

fn prompt_file_permission_readonly(path: &Path, options: &Options) -> bool {
    let stdin_ok = options.__presume_input_tty.unwrap_or(false) || stdin().is_terminal();
    match (stdin_ok, fs::metadata(path), options.interactive) {
        (false, _, InteractiveMode::PromptProtected) => true,
        (_, Ok(_), _) if is_writable(path) => true,
        (_, Ok(metadata), _) if metadata.len() == 0 => prompt_yes!(
            "remove write-protected regular empty file {}?",
            path.quote()
        ),
        _ => prompt_yes!("remove write-protected regular file {}?", path.quote()),
    }
}

// For directories finding if they are writable or not is a hassle. In Unix we can use the built-in rust crate to check mode bits. But other os don't have something similar afaik
// Most cases are covered by keep eye out for edge cases
#[cfg(unix)]
fn handle_writable_directory(path: &Path, options: &Options, metadata: &Metadata) -> bool {
    let stdin_ok = options.__presume_input_tty.unwrap_or(false) || stdin().is_terminal();
    match (
        stdin_ok,
        is_readable_metadata(metadata),
        is_writable_metadata(metadata),
        options.interactive,
    ) {
        (false, _, _, InteractiveMode::PromptProtected) => true,
        (_, false, false, _) => prompt_yes!(
            "attempt removal of inaccessible directory {}?",
            path.quote()
        ),
        (_, false, true, InteractiveMode::Always) => prompt_yes!(
            "attempt removal of inaccessible directory {}?",
            path.quote()
        ),
        (_, true, false, _) => prompt_yes!("remove write-protected directory {}?", path.quote()),
        (_, _, _, InteractiveMode::Always) => prompt_yes!("remove directory {}?", path.quote()),
        (_, _, _, _) => true,
    }
}

/// Checks if the path is referring to current or parent directory , if it is referring to current or any parent directory in the file tree e.g  '/../..' , '../..'
fn path_is_current_or_parent_directory(path: &Path) -> bool {
    let path_str = os_str_as_bytes(path.as_os_str());
    let dir_separator = MAIN_SEPARATOR as u8;
    if let Ok(path_bytes) = path_str {
        return path_bytes == ([b'.'])
            || path_bytes == ([b'.', b'.'])
            || path_bytes.ends_with(&[dir_separator, b'.'])
            || path_bytes.ends_with(&[dir_separator, b'.', b'.'])
            || path_bytes.ends_with(&[dir_separator, b'.', dir_separator])
            || path_bytes.ends_with(&[dir_separator, b'.', b'.', dir_separator]);
    }
    false
}

// For windows we can use windows metadata trait and file attributes to see if a directory is readonly
#[cfg(windows)]
fn handle_writable_directory(path: &Path, options: &Options, metadata: &Metadata) -> bool {
    use std::os::windows::prelude::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_READONLY;
    let not_user_writable = (metadata.file_attributes() & FILE_ATTRIBUTE_READONLY) != 0;
    let stdin_ok = options.__presume_input_tty.unwrap_or(false) || stdin().is_terminal();
    match (stdin_ok, not_user_writable, options.interactive) {
        (false, _, InteractiveMode::PromptProtected) => true,
        (_, true, _) => prompt_yes!("remove write-protected directory {}?", path.quote()),
        (_, _, InteractiveMode::Always) => prompt_yes!("remove directory {}?", path.quote()),
        (_, _, _) => true,
    }
}

// I have this here for completeness but it will always return "remove directory {}" because metadata.permissions().readonly() only works for file not directories
#[cfg(not(windows))]
#[cfg(not(unix))]
fn handle_writable_directory(path: &Path, options: &Options, metadata: &Metadata) -> bool {
    if options.interactive == InteractiveMode::Always {
        prompt_yes!("remove directory {}?", path.quote())
    } else {
        true
    }
}

/// Removes trailing slashes, for example 'd/../////' yield 'd/../' required to fix rm-r4 GNU test
fn clean_trailing_slashes(path: &Path) -> &Path {
    let path_str = os_str_as_bytes(path.as_os_str());
    let dir_separator = MAIN_SEPARATOR as u8;

    if let Ok(path_bytes) = path_str {
        let mut idx = if path_bytes.len() > 1 {
            path_bytes.len() - 1
        } else {
            return path;
        };
        // Checks if element at the end is a '/'
        if path_bytes[idx] == dir_separator {
            for i in (1..path_bytes.len()).rev() {
                // Will break at the start of the continuous sequence of '/', eg: "abc//////" , will break at
                // "abc/", this will clean ////// to the root '/', so we have to be careful to not
                // delete the root.
                if path_bytes[i - 1] != dir_separator {
                    idx = i;
                    break;
                }
            }
            #[cfg(unix)]
            return Path::new(OsStr::from_bytes(&path_bytes[0..=idx]));

            #[cfg(not(unix))]
            // Unwrapping is fine here as os_str_as_bytes() would return an error on non unix
            // systems with non utf-8 characters and thus bypass the if let Ok branch
            return Path::new(std::str::from_utf8(&path_bytes[0..=idx]).unwrap());
        }
    }
    path
}

fn prompt_descend(path: &Path) -> bool {
    prompt_yes!("descend into directory {}?", path.quote())
}

fn normalize(path: &Path) -> PathBuf {
    // copied from https://github.com/rust-lang/cargo/blob/2e4cfc2b7d43328b207879228a2ca7d427d188bb/src/cargo/util/paths.rs#L65-L90
    // both projects are MIT https://github.com/rust-lang/cargo/blob/master/LICENSE-MIT
    // for std impl progress see rfc https://github.com/rust-lang/rfcs/issues/2208
    // TODO: replace this once that lands
    uucore::fs::normalize_path(path)
}

#[cfg(not(windows))]
fn is_symlink_dir(_metadata: &Metadata) -> bool {
    false
}

#[cfg(windows)]
fn is_symlink_dir(metadata: &Metadata) -> bool {
    use std::os::windows::prelude::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;

    metadata.file_type().is_symlink()
        && ((metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY) != 0)
}

mod tests {

    #[test]
    // Testing whether path the `/////` collapses to `/`
    fn test_collapsible_slash_path() {
        use std::path::Path;

        use crate::clean_trailing_slashes;
        let path = Path::new("/////");

        assert_eq!(Path::new("/"), clean_trailing_slashes(path));
    }
}
