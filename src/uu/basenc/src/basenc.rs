// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore lsbf msbf

use clap::{Arg, ArgAction, Command};
use uu_base32::base_common::{self, BASE_CMD_PARSE_ERROR, Config};
use uucore::error::UClapError;
use uucore::locale::get_message;
use uucore::{
    encoding::Format,
    error::{UResult, UUsageError},
};
const ENCODINGS: &[(&str, Format, &str)] = &[
    ("base64", Format::Base64, "same as 'base64' program"),
    ("base64url", Format::Base64Url, "file- and url-safe base64"),
    ("base32", Format::Base32, "same as 'base32' program"),
    (
        "base32hex",
        Format::Base32Hex,
        "extended hex alphabet base32",
    ),
    ("base16", Format::Base16, "hex encoding"),
    (
        "base2lsbf",
        Format::Base2Lsbf,
        "bit string with least significant bit (lsb) first",
    ),
    (
        "base2msbf",
        Format::Base2Msbf,
        "bit string with most significant bit (msb) first",
    ),
    (
        "z85",
        Format::Z85,
        "ascii85-like encoding;\n\
        when encoding, input length must be a multiple of 4;\n\
        when decoding, input length must be a multiple of 5",
    ),
];

pub fn uu_app() -> Command {
    let about: &'static str = Box::leak(get_message("basenc-about").into_boxed_str());
    let usage: &'static str = Box::leak(get_message("basenc-usage").into_boxed_str());

    let mut command = base_common::base_app(about, usage);
    for encoding in ENCODINGS {
        let raw_arg = Arg::new(encoding.0)
            .long(encoding.0)
            .help(encoding.2)
            .action(ArgAction::SetTrue);
        let overriding_arg = ENCODINGS
            .iter()
            .fold(raw_arg, |arg, enc| arg.overrides_with(enc.0));
        command = command.arg(overriding_arg);
    }
    command
}

fn parse_cmd_args(args: impl uucore::Args) -> UResult<(Config, Format)> {
    let matches = uu_app()
        .try_get_matches_from(args.collect_lossy())
        .with_exit_code(1)?;
    let format = ENCODINGS
        .iter()
        .find(|encoding| matches.get_flag(encoding.0))
        .ok_or_else(|| UUsageError::new(BASE_CMD_PARSE_ERROR, "missing encoding type"))?
        .1;
    let config = Config::from(&matches)?;
    Ok((config, format))
}

#[uucore::main]
pub fn uumain(args: impl uucore::Args) -> UResult<()> {
    let (config, format) = parse_cmd_args(args)?;

    let mut input = base_common::get_input(&config)?;

    base_common::handle_input(&mut input, format, config)
}
