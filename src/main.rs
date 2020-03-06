//! An implementation of [tldr](https://github.com/tldr-pages/tldr) in Rust.
//
// Copyright (c) 2015-2020 tealdeer developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. All files in the project carrying such notice may not be
// copied, modified, or distributed except according to those terms.

#![deny(clippy::all)]
#![warn(clippy::pedantic)]
#![allow(clippy::similar_names)]
#![allow(clippy::module_name_repetitions)]

#[cfg(feature = "logging")]
extern crate env_logger;
extern crate clap;

use std::fs::File;
use std::io::BufReader;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use ansi_term::Color;
use app_dirs::AppInfo;
use docopt::Docopt;
use clap::{Arg, App, SubCommand};
#[cfg(not(target_os = "windows"))]
use pager::Pager;
use serde_derive::Deserialize;

mod cache;
mod config;
mod error;
mod formatter;
mod tokenizer;
mod types;

use crate::cache::Cache;
use crate::config::{get_config_path, make_default_config, Config};
use crate::error::TealdeerError::{CacheError, ConfigError, UpdateError};
use crate::formatter::print_lines;
use crate::tokenizer::Tokenizer;
use crate::types::OsType;

const NAME: &str = "tealdeer";
const APP_INFO: AppInfo = AppInfo {
    name: NAME,
    author: NAME,
};
const VERSION: &str = env!("CARGO_PKG_VERSION");
const USAGE: &str = "
Usage:

    tldr [options] <command>...
    tldr [options]

Options:

    -h --help           Show this screen
    -v --version        Show version information
    -l --list           List all commands in the cache
    -f --render <file>  Render a specific markdown file
    -o --os <type>      Override the operating system [linux, osx, sunos, windows]
    -u --update         Update the local cache
    -c --clear-cache    Clear the local cache
    -p --pager          Use a pager to page output
    -m --markdown       Display the raw markdown instead of rendering it
    -q --quiet          Suppress informational messages
    --config-path       Show config file path
    --seed-config       Create a basic config

Examples:

    $ tldr tar
    $ tldr --list

To control the cache:

    $ tldr --update
    $ tldr --clear-cache

To render a local file (for testing):

    $ tldr --render /path/to/file.md
";
const ARCHIVE_URL: &str = "https://github.com/tldr-pages/tldr/archive/master.tar.gz";
const MAX_CACHE_AGE: Duration = Duration::from_secs(2_592_000); // 30 days
#[cfg(not(target_os = "windows"))]
const PAGER_COMMAND: &str = "less -R";

#[derive(Debug, Deserialize)]
struct Args {
    arg_command: Option<Vec<String>>,
    flag_help: bool,
    flag_version: bool,
    flag_list: bool,
    flag_render: Option<String>,
    flag_os: Option<OsType>,
    flag_update: bool,
    flag_clear_cache: bool,
    flag_pager: bool,
    flag_quiet: bool,
    flag_config_path: bool,
    flag_seed_config: bool,
    flag_markdown: bool,
}

/// Print page by path
fn print_page(path: &Path, enable_markdown: bool, enable_styles: bool) -> Result<(), String> {
    // Open file
    let file = File::open(path).map_err(|msg| format!("Could not open file: {}", msg))?;
    let reader = BufReader::new(file);

    // Look up config file, if none is found fall back to default config.
    let config = match Config::load(enable_styles) {
        Ok(config) => config,
        Err(ConfigError(msg)) => {
            eprintln!("Could not load config: {}", msg);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Could not load config: {}", e);
            process::exit(1);
        }
    };

    if enable_markdown {
        // Print the raw markdown of the file.
        for line in reader.lines() {
            println!("{}", line.unwrap());
        }
    } else {
        // Create tokenizer and print output
        let mut tokenizer = Tokenizer::new(reader);
        print_lines(&mut tokenizer, &config);
    };

    Ok(())
}

/// Set up display pager
#[cfg(not(target_os = "windows"))]
fn configure_pager(args: &Arg, enable_styles: bool) {
    // Flags have precedence
    if args.flag_pager {
        Pager::with_default_pager(PAGER_COMMAND).setup();
        return;
    }

    // Then check config
    let config = match Config::load(enable_styles) {
        Ok(config) => config,
        Err(ConfigError(msg)) => {
            eprintln!("Could not load config: {}", msg);
            process::exit(1);
        }
        Err(e) => {
            eprintln!("Could not load config: {}", e);
            process::exit(1);
        }
    };

    if config.display.use_pager {
        Pager::with_default_pager(PAGER_COMMAND).setup();
    }
}

#[cfg(target_os = "windows")]
fn configure_pager(_args: &Arg, _enable_styles: bool) {
    eprintln!("Warning: -p / --pager flag not available on Windows!");
}

/// Check the cache for freshness
fn check_cache(args: &Arg) {
    if !args.flag_update {
        match Cache::last_update() {
            Some(ago) if ago > MAX_CACHE_AGE => {
                if args.flag_quiet {
                    return;
                }
                println!(
                    "{}",
                    Color::Yellow.paint(format!(
                        "The cache hasn't been updated for more than {} days.\n\
                         You should probably run `tldr --update` soon.",
                        MAX_CACHE_AGE.as_secs() / 24 / 3600
                    ))
                );
            }
            None => {
                eprintln!("Cache not found. Please run `tldr --update`.");
                process::exit(1);
            }
            _ => {}
        }
    };
}

/// Clear the cache
fn clear_cache(quietly: bool) {
    Cache::clear().unwrap_or_else(|e| {
        match e {
            CacheError(msg) | ConfigError(msg) | UpdateError(msg) => {
                eprintln!("Could not delete cache: {}", msg)
            }
        };
        process::exit(1);
    });
    if !quietly {
        println!("Successfully deleted cache.");
    }
}

/// Update the cache
fn update_cache(cache: &Cache, quietly: bool) {
    cache.update().unwrap_or_else(|e| {
        match e {
            CacheError(msg) | ConfigError(msg) | UpdateError(msg) => {
                eprintln!("Could not update cache: {}", msg)
            }
        };
        process::exit(1);
    });
    if !quietly {
        println!("Successfully updated cache.");
    }
}

/// Show the config path
fn show_config_path() {
    match get_config_path() {
        Ok(config_file_path) => {
            println!("Config path is: {}", config_file_path.to_str().unwrap());
        }
        Err(ConfigError(msg)) => {
            eprintln!("Could not look up config_path: {}", msg);
            process::exit(1);
        }
        Err(_) => {
            eprintln!("Unknown error");
            process::exit(1);
        }
    }
}

/// Create seed config file and exit
fn create_config_and_exit() {
    match make_default_config() {
        Ok(config_file_path) => {
            println!(
                "Successfully created seed config file here: {}",
                config_file_path.to_str().unwrap()
            );
            process::exit(0);
        }
        Err(ConfigError(msg)) => {
            eprintln!("Could not create seed config: {}", msg);
            process::exit(1);
        }
        Err(_) => {
            eprintln!("Unknown error");
            process::exit(1);
        }
    }
}

#[cfg(feature = "logging")]
fn init_log() {
    env_logger::init();
}

#[cfg(not(feature = "logging"))]
fn init_log() {}

#[cfg(target_os = "linux")]
fn get_os() -> OsType {
    OsType::Linux
}

#[cfg(any(target_os = "macos",
          target_os = "freebsd",
          target_os = "netbsd",
          target_os = "openbsd",
          target_os = "dragonfly"))]
fn get_os() -> OsType {
    OsType::OsX
}

#[cfg(target_os = "windows")]
fn get_os() -> OsType {
    OsType::Windows
}

#[cfg(not(any(target_os = "linux",
              target_os = "macos",
              target_os = "freebsd",
              target_os = "netbsd",
              target_os = "openbsd",
              target_os = "dragonfly",
              target_os = "windows")))]
fn get_os() -> OsType {
    OsType::Other
}

fn main() {
    // Initialize logger
    init_log();

    // Parse arguments
    let args = App::new("tldr")
                          .version("1.3.1")
                          .author("tealdeer")
                          .about("tldr - Simplified and community-driven man pages")
                          .arg(Arg::with_name("command")
                               .help("Sets the command to tldr")
                               .required(false)
                               .index(1))
                          .arg(Arg::with_name("help")
                               .short("h")
                               .long("help")
                               .help("Show version information"))
                          .arg(Arg::with_name("version")
                               .short("v")
                               .long("version")
                               .help("Show version information"))
                          .arg(Arg::with_name("list")
                               .short("l")
                               .long("list")
                               .help("List all commands in the cache"))
                          .arg(Arg::with_name("render")
                               .short("f")
                               .long("render")
                               .value_name("file")
                               .help("Render a specific markdown file"))
                          .arg(Arg::with_name("os")
                               .short("o")
                               .long("os")
                               .value_name("type")
                               .help("Override the operating system [linux, osx, sunos, windows]"))
                          .arg(Arg::with_name("update")
                               .short("u")
                               .long("update")
                               .help("Update the local cache"))
                          .arg(Arg::with_name("clear_cache")
                               .short("c")
                               .long("clear-cache")
                               .help("Clear the local cache"))
                          .arg(Arg::with_name("pager")
                               .short("p")
                               .long("pager")
                               .help("Use a pager to page output"))
                          .arg(Arg::with_name("markdown")
                               .short("m")
                               .long("markdown")
                               .help("Display the raw markdown instead of rendering it"))
                          .arg(Arg::with_name("quiet")
                               .short("q")
                               .long("quiet")
                               .help("Suppress informational messages"))
                          .arg(Arg::with_name("config_path")
                               .long("config-path")
                               .help("Show config file path"))
                          .arg(Arg::with_name("seed_config")
                               .long("seed-config")
                               .help("Create a basic config"))
                          .get_matches();
    
    // Show version and exit
    if args.is_present("version") {
        let os = get_os();
        println!("{} v{} ({})", NAME, VERSION, os);
        process::exit(0);
    }

    // Determine the usage of styles
    #[cfg(target_os = "windows")]
    let enable_styles = ansi_term::enable_ansi_support().is_ok();
    #[cfg(not(target_os = "windows"))]
    let enable_styles = true;

    // Configure pager
    configure_pager(&args, enable_styles);

    // Specify target OS
    let os: OsType = match args.value_of("os") {
        Some(os) => os,
        None => get_os(),
    };

    // Initialize cache
    let cache = Cache::new(ARCHIVE_URL, os);

    // Clear cache, pass through
    if args.flag_clear_cache {
        clear_cache(args.value_of("quiet"));
    }

    // Update cache, pass through
    if args.is_present("update") {
        update_cache(&cache, args.value_of);
    }

    // Show config file and path, pass through
    if args.is_present("config_path") {
        show_config_path();
    }

    // Create a basic config and exit
    if args.is_present("seed_config") {
        create_config_and_exit();
    }

    // Render local file and exit
    if let Some(ref file) = args.value_of("render") {
        let path = PathBuf::from(file);
        if let Err(msg) = print_page(&path, args.value_of("markdown"), enable_styles) {
            eprintln!("{}", msg);
            process::exit(1);
        } else {
            process::exit(0);
        };
    }

    // List cached commands and exit
    if args.flag_list {
        // Check cache for freshness
        check_cache(&args);

        // Get list of pages
        let pages = cache.list_pages().unwrap_or_else(|e| {
            match e {
                CacheError(msg) | ConfigError(msg) | UpdateError(msg) => {
                    eprintln!("Could not get list of pages: {}", msg)
                }
            }
            process::exit(1);
        });

        // Print pages
        println!("{}", pages.join(", "));
        process::exit(0);
    }

    // Show command from cache
    if let Some(ref command) = args.value_of("command") {
        let command = command.join("-");
        // Check cache for freshness
        check_cache(&args);

        // Search for command in cache
        if let Some(path) = cache.find_page(&command) {
            if let Err(msg) = print_page(&path, args.value_of("markdown"), enable_styles) {
                eprintln!("{}", msg);
                process::exit(1);
            } else {
                process::exit(0);
            }
        } else {
            if !args.flag_quiet {
                println!("Page {} not found in cache", &command);
                println!("Try updating with `tldr --update`, or submit a pull request to:");
                println!("https://github.com/tldr-pages/tldr");
            }
            process::exit(1);
        }
    }

    // Some flags can be run without a command.
    if !(args.is_present("update") || args.is_present("clear_cache") || args.is_present("config_path")) {
        eprintln!("{}", USAGE);
        process::exit(1);
    }
}

#[cfg(test)]
mod test {
    use docopt::{Docopt, Error};
    use crate::{Args, OsType, USAGE};

    fn test_helper(argv: &[&str]) -> Result<Args, Error> {
        Docopt::new(USAGE).and_then(|d| d.argv(argv.iter()).deserialize())
    }

    #[test]
    fn test_docopt_os_case_insensitive() {
        let argv = vec!["cp", "--os", "LiNuX"];
        let os = test_helper(&argv).unwrap().flag_os.unwrap();
        assert_eq!(OsType::Linux, os);
    }

    #[test]
    fn test_docopt_expect_error() {
        let argv = vec!["cp", "--os", "lindows"];
        assert!(!test_helper(&argv).is_ok());
    }
}
