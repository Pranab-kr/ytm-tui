//! `ytm config` end to end, against the real binary.
//!
//! Through the process rather than calling `run_config`, because two of the three
//! bugs these cover live in argument dispatch: `--config` was honoured by every
//! other subcommand but not this one, and `main` loaded config.toml before
//! dispatching, so an invalid file blocked the command that exists to repair it.
//!
//! No network, no audio — `config` touches only the filesystem.

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_ytm-tui");

/// A unique empty directory. Cheaper than a dev-dependency for four tests.
fn scratch(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ytm-config-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `--no-edit` everywhere: these must not try to spawn $EDITOR under `cargo test`.
fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN)
        .args(args)
        .env_remove("EDITOR")
        .env_remove("VISUAL")
        .output()
        .expect("the binary runs")
}

#[test]
fn the_config_path_override_is_where_config_writes() {
    // `--config` is declared `global = true`, so it reads as honoured by every
    // subcommand. `config` ignored it and wrote the platform default instead,
    // which meant there was no way to produce a config file anywhere else.
    let dir = scratch("override");
    let path = dir.join("elsewhere.toml");

    let out = run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        path.exists(),
        "--config path was not written: {}",
        path.display()
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("[keys]"),
        "the documented example was not what landed"
    );
    assert!(
        text.contains("# focus_current = \"c\""),
        "new configs must document the current-song binding"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(path.to_str().unwrap()),
        "the path printed was not the path asked for"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_invalid_config_does_not_block_the_config_command() {
    // The chicken and egg: `main` loaded config.toml before dispatching, so a
    // typo in the file made `config` — the command whose whole job is opening
    // that file to fix it — exit 1 without opening anything.
    let dir = scratch("invalid");
    let path = dir.join("config.toml");
    std::fs::write(&path, "this is not valid toml [[[\n").unwrap();

    let out = run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);

    assert!(
        out.status.success(),
        "config refused to run on a broken file: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And it must not have thrown the user's file away while recovering.
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "this is not valid toml [[[\n",
        "the invalid file was overwritten rather than opened"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_incomplete_config_gains_the_sections_it_lacks() {
    // The documented promise is that the file carries every setting and binding at
    // its default, commented out. An existing file never gained anything, so one
    // written from an older README stayed permanently without a `[keys]` section.
    let dir = scratch("partial");
    let path = dir.join("config.toml");
    let theirs = "[auth]\nkind = \"cookie\"\ncookie_file = \"~/mine.txt\"\n";
    std::fs::write(&path, theirs).unwrap();

    let out = run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let after = std::fs::read_to_string(&path).unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        after.starts_with(theirs),
        "the user's own lines were not left alone:\n{after}"
    );
    for section in ["[playback]", "[ui]", "[behaviour]", "[keys]"] {
        assert!(after.contains(section), "{section} was not added:\n{after}");
    }
    assert!(
        after.contains("# add_to_queue = \"a\""),
        "the bindings were added without their commented defaults:\n{after}"
    );
    // Whatever landed has to be loadable, or the command handed the user a
    // config that refuses to parse.
    assert!(
        after.parse::<toml::Table>().is_ok(),
        "the appended text does not parse:\n{after}"
    );
    assert!(
        stdout.contains("behaviour") && stdout.contains("keys"),
        "what was added was not named:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn settings_absent_from_a_section_that_exists_are_written_into_it() {
    // The other half of the same promise. Appending whole sections is not enough: a
    // `[ui]` predating a setting keeps parsing and stays permanently without it, so
    // the setting never appears in the file the command claims lists them all.
    let dir = scratch("partialsection");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[ui]\nvim_keys = true\n").unwrap();

    let out = run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);
    let after = std::fs::read_to_string(&path).unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for line in [
        "# theme = \"auto\"",
        "# mouse = true",
        "# start_pane = \"playlists\"",
        "# tick_ms = 250",
        "# album_art = true",
    ] {
        assert!(after.contains(line), "{line} was not written in:\n{after}");
    }
    // Written into `[ui]`, not dumped after the sections that follow it: the
    // keys must still land under the header they belong to.
    let ui = after.find("[ui]").expect("[ui] survived");
    let next = after[ui + 4..]
        .find("\n[")
        .map_or(after.len(), |i| ui + 4 + i);
    assert!(
        after[ui..next].contains("# theme = \"auto\""),
        "the settings landed outside [ui]:\n{after}"
    );

    // Commented, so what the file actually does is unchanged: the user set
    // vim_keys and nothing else, and that must still be all it sets.
    let parsed: toml::Table = after.parse().expect("the result parses");
    let keys: Vec<&String> = parsed["ui"].as_table().unwrap().keys().collect();
    assert_eq!(
        keys,
        vec!["vim_keys"],
        "topping up changed what the config does"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn adding_the_missing_sections_happens_once() {
    // Run twice and the file must not grow a second `[keys]`, which would not
    // parse. The append is a repair, not something that stacks.
    let dir = scratch("twice");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[auth]\nkind = \"cookie\"\n").unwrap();

    run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);
    let once = std::fs::read_to_string(&path).unwrap();
    let out = run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);
    let twice = std::fs::read_to_string(&path).unwrap();

    assert!(out.status.success(), "the second run failed");
    assert_eq!(once, twice, "the second run appended again");
    assert!(
        twice.parse::<toml::Table>().is_ok(),
        "the result stopped parsing"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_complete_config_is_not_nagged_about() {
    // The counter-test: the bundled example is complete, so it must draw no
    // "missing" line. Without this the fix above would warn on every file.
    let dir = scratch("complete");
    let path = dir.join("config.toml");

    run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);
    let out = run(&["--config", path.to_str().unwrap(), "config", "--no-edit"]);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("missing"),
        "a complete config was reported as incomplete:\n{stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
