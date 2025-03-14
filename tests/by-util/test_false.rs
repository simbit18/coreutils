// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.
use crate::common::util::TestScenario;
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
use std::fs::OpenOptions;

#[test]
fn test_exit_code() {
    new_ucmd!().fails().no_output();
}

#[test]
fn test_version() {
    new_ucmd!()
        .args(&["--version"])
        .fails()
        .stdout_contains("false");
}

#[test]
fn test_help() {
    new_ucmd!()
        .args(&["--help"])
        .fails()
        .stdout_contains("false");
}

#[test]
fn test_short_options() {
    for option in ["-h", "-V"] {
        new_ucmd!().arg(option).fails().no_output();
    }
}

#[test]
fn test_conflict() {
    new_ucmd!()
        .args(&["--help", "--version"])
        .fails()
        .no_output();
}

#[test]
#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "netbsd"))]
fn test_full() {
    for option in ["--version", "--help"] {
        let dev_full = OpenOptions::new().write(true).open("/dev/full").unwrap();

        new_ucmd!()
            .arg(option)
            .set_stdout(dev_full)
            .fails()
            .stderr_contains("No space left on device");
    }
}
