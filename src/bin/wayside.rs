//! Command line for wayside, a thin shell over the wayside
//! library.
//!
//! Every command names the store first. Errors the store
//! raises are printed as one line on stderr with the exit code
//! the error carries, and no traceback is ever printed for
//! them. The argument parser mirrors the Python argparse
//! surface of the Round 1 command line: the same usage texts,
//! the same parser error messages at exit 2, unambiguous
//! prefix abbreviations of long options, the equals form, and
//! parser extras reported by the top level parser.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wayside::Error;

const MAIN_USAGE: &str = "usage: wayside [-h] --store STORE\n               {checkpoint,restore,rewind,fork,list,show} ...";
const COMMANDS: [&str; 6] = ["checkpoint", "restore", "rewind", "fork", "list", "show"];

const MAIN_HELP: &str = "usage: wayside [-h] --store STORE
               {checkpoint,restore,rewind,fork,list,show} ...

Checkpoint the working state of an agent session so it can be resumed, forked,
or rewound after the process is gone.

positional arguments:
  {checkpoint,restore,rewind,fork,list,show}
    checkpoint          record a new checkpoint of a session
    restore             materialize one checkpoint into a target directory
    rewind              restore a checkpoint earlier than the newest one
    fork                start a new session from a named checkpoint
    list                list the checkpoints of a session
    show                print the manifest of one checkpoint

options:
  -h, --help            show this help message and exit
  --store STORE         the store root directory, created by the first
                        checkpoint";

fn sub_usage(command: &str) -> &'static str {
    match command {
        "checkpoint" => {
            "usage: wayside checkpoint [-h] --state STATE [--memory MEMORY]\n                          [--env KEY=VALUE]\n                          session"
        }
        "restore" => "usage: wayside restore [-h] --target TARGET session sequence",
        "rewind" => "usage: wayside rewind [-h] --target TARGET session sequence",
        "fork" => "usage: wayside fork [-h] session sequence new_session",
        "list" => "usage: wayside list [-h] session",
        _ => "usage: wayside show [-h] session sequence",
    }
}

fn sub_help(command: &str) -> String {
    let body = match command {
        "checkpoint" => {
            "positional arguments:\n  session\n\noptions:\n  -h, --help       show this help message and exit\n  --state STATE    the state directory to record\n  --memory MEMORY  a file holding the memory document to record, empty when\n                   omitted\n  --env KEY=VALUE  an env pair to declare, repeatable"
        }
        "restore" => {
            "positional arguments:\n  session\n  sequence\n\noptions:\n  -h, --help       show this help message and exit\n  --target TARGET"
        }
        "rewind" => {
            "positional arguments:\n  session\n  sequence\n\noptions:\n  -h, --help       show this help message and exit\n  --target TARGET"
        }
        "fork" => {
            "positional arguments:\n  session\n  sequence\n  new_session\n\noptions:\n  -h, --help   show this help message and exit"
        }
        "list" => {
            "positional arguments:\n  session\n\noptions:\n  -h, --help   show this help message and exit"
        }
        _ => {
            "positional arguments:\n  session\n  sequence\n\noptions:\n  -h, --help   show this help message and exit"
        }
    };
    format!("{}\n\n{body}", sub_usage(command))
}

struct ParserError {
    usage: &'static str,
    prog: String,
    message: String,
}

fn parser_error(usage: &'static str, prog: &str, message: String) -> ParserError {
    ParserError {
        usage,
        prog: prog.to_string(),
        message,
    }
}

fn is_option_like(token: &str) -> bool {
    if token.len() < 2 || !token.starts_with('-') || token == "-" {
        return false;
    }
    // A token in the shape of a negative number is positional,
    // as argparse's negative number matcher holds when no
    // option looks like a negative number.
    let body = &token[1..];
    let digits: String = body.chars().filter(|c| *c != '.').collect();
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    true
}

/// Resolve a long option token against the candidates of one
/// parser, the argparse way: an exact match wins, an
/// unambiguous prefix matches, an ambiguous prefix is an
/// error, and no match means the token is an extra.
fn resolve_long<'a>(name: &'a str, candidates: &[&'a str]) -> Result<Option<&'a str>, String> {
    if candidates.contains(&name) {
        return Ok(Some(name));
    }
    let matches: Vec<&str> = candidates
        .iter()
        .filter(|candidate| candidate.starts_with(name))
        .cloned()
        .collect();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches[0])),
        _ => Err(format!(
            "ambiguous option: {name} could match {}",
            matches.join(", ")
        )),
    }
}

struct SubArgs {
    positionals: Vec<String>,
    options: Vec<(String, String)>,
    extras: Vec<String>,
}

/// Parse one subcommand's arguments. Option values are drawn
/// from the following token unless it is option like, and
/// tokens the subcommand does not consume are collected as
/// extras for the top level parser to report.
fn parse_sub(
    command: &str,
    tokens: &[String],
    value_options: &[&str],
    positional_count: usize,
) -> Result<SubArgs, ParserError> {
    let mut candidates: Vec<&str> = value_options.to_vec();
    candidates.push("--help");
    let mut result = SubArgs {
        positionals: Vec::new(),
        options: Vec::new(),
        extras: Vec::new(),
    };
    let mut index = 0;
    let mut after_separator = false;
    while index < tokens.len() {
        let token = &tokens[index];
        if !after_separator && token == "--" {
            after_separator = true;
            index += 1;
            continue;
        }
        if !after_separator && (token == "-h" || token == "--help") {
            print!("{}", sub_help(command));
            println!();
            std::process::exit(0);
        }
        if !after_separator && token.starts_with("--") && token.len() > 2 {
            let (name, inline_value) = match token.find('=') {
                Some(pos) => (&token[..pos], Some(token[pos + 1..].to_string())),
                None => (&token[..], None),
            };
            match resolve_long(name, &candidates) {
                Ok(Some("--help")) => {
                    print!("{}", sub_help(command));
                    println!();
                    std::process::exit(0);
                }
                Ok(Some(option)) => {
                    if let Some(value) = inline_value {
                        result.options.push((option.to_string(), value));
                        index += 1;
                    } else if index + 1 < tokens.len() && !is_option_like(&tokens[index + 1]) {
                        result
                            .options
                            .push((option.to_string(), tokens[index + 1].clone()));
                        index += 2;
                    } else {
                        return Err(parser_error(
                            sub_usage(command),
                            &format!("wayside {command}"),
                            format!("argument {option}: expected one argument"),
                        ));
                    }
                    continue;
                }
                Ok(None) => {
                    result.extras.push(token.clone());
                    index += 1;
                    continue;
                }
                Err(message) => {
                    return Err(parser_error(
                        sub_usage(command),
                        &format!("wayside {command}"),
                        message,
                    ));
                }
            }
        }
        if !after_separator && is_option_like(token) {
            result.extras.push(token.clone());
            index += 1;
            continue;
        }
        if result.positionals.len() < positional_count {
            result.positionals.push(token.clone());
        } else {
            result.extras.push(token.clone());
        }
        index += 1;
    }
    Ok(result)
}

fn option_value<'a>(args: &'a SubArgs, name: &str) -> Option<&'a str> {
    args.options
        .iter()
        .rev()
        .find(|(seen, _)| seen == name)
        .map(|(_, value)| value.as_str())
}

fn require(
    command: &str,
    args: &SubArgs,
    positional_names: &[&str],
    required_options: &[&str],
) -> Result<(), ParserError> {
    let mut missing: Vec<String> = Vec::new();
    for (index, name) in positional_names.iter().enumerate() {
        if args.positionals.len() <= index {
            missing.push(name.to_string());
        }
    }
    for name in required_options {
        if option_value(args, name).is_none() {
            missing.push(name.to_string());
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(parser_error(
            sub_usage(command),
            &format!("wayside {command}"),
            format!(
                "the following arguments are required: {}",
                missing.join(", ")
            ),
        ))
    }
}

/// Python's int conversion for the sequence argument: optional
/// surrounding whitespace, an optional sign, digits with
/// single underscores between them.
fn python_int(text: &str) -> Option<i64> {
    let trimmed = text.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c'));
    let (sign, body) = if let Some(rest) = trimmed.strip_prefix('-') {
        (-1i128, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (1i128, rest)
    } else {
        (1i128, trimmed)
    };
    if body.is_empty() {
        return None;
    }
    let mut value: i128 = 0;
    let mut previous_digit = false;
    let mut previous_underscore = false;
    let mut seen_digit = false;
    for ch in body.chars() {
        if ch == '_' {
            if !previous_digit || previous_underscore {
                return None;
            }
            previous_underscore = true;
            previous_digit = false;
        } else if ch.is_ascii_digit() {
            value = value
                .checked_mul(10)?
                .checked_add(ch as i128 - '0' as i128)?;
            previous_digit = true;
            previous_underscore = false;
            seen_digit = true;
        } else {
            return None;
        }
    }
    if previous_underscore || !seen_digit {
        return None;
    }
    i64::try_from(sign * value).ok()
}

fn sequence_arg(text: &str) -> Result<i64, Error> {
    python_int(text).ok_or_else(|| {
        Error::InvalidName(format!(
            "invalid sequence {}, use a positive integer",
            wayside::py_repr(text)
        ))
    })
}

fn parse_env(pairs: &[String]) -> Result<BTreeMap<String, String>, Error> {
    let mut env = BTreeMap::new();
    for pair in pairs {
        match pair.split_once('=') {
            Some((key, value)) => {
                env.insert(key.to_string(), value.to_string());
            }
            None => {
                return Err(Error::Wayside(format!(
                    "invalid env pair {}, use KEY=VALUE",
                    wayside::py_repr(pair)
                )));
            }
        }
    }
    Ok(env)
}

fn read_memory(path: Option<&str>) -> Result<Vec<u8>, Error> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let memory_path = Path::new(path);
    if memory_path.is_symlink() {
        return Err(Error::Wayside(format!(
            "the memory document at {path} is a symbolic link, \
             so it cannot be read as a memory document"
        )));
    }
    if !memory_path.is_file() {
        return Err(Error::Wayside(format!(
            "the memory document at {path} is not a regular file, \
             so it cannot be read as a memory document"
        )));
    }
    std::fs::read(memory_path).map_err(|exc| {
        Error::Wayside(format!(
            "the memory document at {path} cannot be read, {exc}"
        ))
    })
}

fn report_error(error: &Error) -> ExitCode {
    eprintln!("error: {}", error.message());
    ExitCode::from(error.exit_code() as u8)
}

fn run() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match dispatch(&argv) {
        Ok(code) => code,
        Err(error) => report_error(&error),
    }
}

fn dispatch(argv: &[String]) -> Result<ExitCode, Error> {
    // Parse the top level: the store option and the subcommand
    // word, with the remaining tokens handed to the subcommand.
    let mut store: Option<String> = None;
    let mut index = 0;
    let mut command: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut extras: Vec<String> = Vec::new();
    while index < argv.len() {
        let token = &argv[index];
        if token == "-h" || token == "--help" {
            println!("{MAIN_HELP}");
            return Ok(ExitCode::from(0));
        }
        if token.starts_with("--") && token.len() > 2 {
            let (name, inline_value) = match token.find('=') {
                Some(pos) => (&token[..pos], Some(token[pos + 1..].to_string())),
                None => (&token[..], None),
            };
            match resolve_long(name, &["--store", "--help"]) {
                Ok(Some("--help")) => {
                    println!("{MAIN_HELP}");
                    return Ok(ExitCode::from(0));
                }
                Ok(Some("--store")) => {
                    if let Some(value) = inline_value {
                        store = Some(value);
                        index += 1;
                    } else if index + 1 < argv.len() && !is_option_like(&argv[index + 1]) {
                        store = Some(argv[index + 1].clone());
                        index += 2;
                    } else {
                        return Ok(parser_fail(
                            MAIN_USAGE,
                            "wayside",
                            "argument --store: expected one argument",
                        ));
                    }
                    continue;
                }
                Ok(Some(_)) => unreachable!(),
                Ok(None) => {
                    extras.push(token.clone());
                    index += 1;
                    continue;
                }
                Err(message) => {
                    return Ok(parser_fail(MAIN_USAGE, "wayside", &message));
                }
            }
        }
        if is_option_like(token) {
            extras.push(token.clone());
            index += 1;
            continue;
        }
        command = Some(token.clone());
        rest = argv[index + 1..].to_vec();
        break;
    }
    let Some(command) = command else {
        let mut missing = Vec::new();
        if store.is_none() {
            missing.push("--store");
        }
        missing.push("command");
        return Ok(parser_fail(
            MAIN_USAGE,
            "wayside",
            &format!(
                "the following arguments are required: {}",
                missing.join(", ")
            ),
        ));
    };
    if !COMMANDS.contains(&command.as_str()) {
        return Ok(parser_fail(
            MAIN_USAGE,
            "wayside",
            &format!(
                "argument command: invalid choice: {} (choose from 'checkpoint', 'restore', 'rewind', 'fork', 'list', 'show')",
                wayside::py_repr(&command)
            ),
        ));
    }
    let parsed = match command.as_str() {
        "checkpoint" => parse_sub(&command, &rest, &["--state", "--memory", "--env"], 1),
        "restore" | "rewind" => parse_sub(&command, &rest, &["--target"], 2),
        "fork" => parse_sub(&command, &rest, &[], 3),
        "list" => parse_sub(&command, &rest, &[], 1),
        _ => parse_sub(&command, &rest, &[], 2),
    };
    let args = match parsed {
        Ok(args) => args,
        Err(failure) => {
            return Ok(parser_fail(failure.usage, &failure.prog, &failure.message));
        }
    };
    let required = match command.as_str() {
        "checkpoint" => require(&command, &args, &["session"], &["--state"]),
        "restore" | "rewind" => require(&command, &args, &["session", "sequence"], &["--target"]),
        "fork" => require(
            &command,
            &args,
            &["session", "sequence", "new_session"],
            &[],
        ),
        "list" => require(&command, &args, &["session"], &[]),
        _ => require(&command, &args, &["session", "sequence"], &[]),
    };
    if let Err(failure) = required {
        return Ok(parser_fail(failure.usage, &failure.prog, &failure.message));
    }
    if store.is_none() {
        return Ok(parser_fail(
            MAIN_USAGE,
            "wayside",
            "the following arguments are required: --store",
        ));
    }
    let store = PathBuf::from(store.unwrap());
    extras.extend(args.extras.iter().cloned());
    if !extras.is_empty() {
        return Ok(parser_fail(
            MAIN_USAGE,
            "wayside",
            &format!("unrecognized arguments: {}", extras.join(" ")),
        ));
    }
    let session = args.positionals[0].clone();
    match command.as_str() {
        "checkpoint" => {
            let pairs: Vec<String> = args
                .options
                .iter()
                .filter(|(name, _)| name == "--env")
                .map(|(_, value)| value.clone())
                .collect();
            let env = parse_env(&pairs)?;
            let memory = read_memory(option_value(&args, "--memory"))?;
            let state = option_value(&args, "--state").unwrap();
            let manifest =
                wayside::checkpoint(&store, &session, Path::new(state), Some(&env), &memory)?;
            println!(
                "session {} checkpoint {}",
                manifest.session, manifest.sequence
            );
        }
        "restore" => {
            let sequence = sequence_arg(&args.positionals[1])?;
            let target = option_value(&args, "--target").unwrap();
            wayside::restore(&store, &session, sequence, Path::new(target))?;
            println!("restored session {session} checkpoint {sequence} to {target}");
        }
        "rewind" => {
            let sequence = sequence_arg(&args.positionals[1])?;
            let target = option_value(&args, "--target").unwrap();
            wayside::rewind(&store, &session, sequence, Path::new(target))?;
            println!("rewound session {session} to checkpoint {sequence} at {target}");
        }
        "fork" => {
            let sequence = sequence_arg(&args.positionals[1])?;
            let new_session = args.positionals[2].clone();
            let manifest = wayside::fork(&store, &session, sequence, &new_session)?;
            println!(
                "forked session {session} checkpoint {sequence} to session {} checkpoint 1",
                manifest.session
            );
        }
        "list" => {
            for entry in wayside::list_checkpoints(&store, &session)? {
                println!("{} {}", entry.sequence, entry.created_at);
            }
        }
        _ => {
            let sequence = sequence_arg(&args.positionals[1])?;
            let manifest = wayside::show(&store, &session, sequence)?;
            println!("{}", wayside::render_manifest(&manifest));
        }
    }
    Ok(ExitCode::from(0))
}

fn parser_fail(usage: &str, prog: &str, message: &str) -> ExitCode {
    eprintln!("{usage}");
    eprintln!("{prog}: error: {message}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    run()
}
