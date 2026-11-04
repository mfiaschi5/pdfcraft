//! A fake `claude` (a shell script) stands in for the program: no network, no real sign-in.
#![cfg(unix)]

use std::ops::ControlFlow;
use std::os::unix::fs::PermissionsExt;

use pdfcraft_ai::claude::{self, Options, Passage};
use pdfcraft_ai::event::Event;
use pdfcraft_ai::model::OPUS;
use pdfcraft_ai::source::SourceId;
use pdfcraft_ai::transport::Secret;

use super::*;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pdfcraft-cc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A program that runs `body` (sh), with its arguments saved one per line in `args.txt`.
fn program(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("claude");
    let script = format!("#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\n{body}\n", dir.join("args.txt").display());
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn ask(cc: &ClaudeCode) -> Result<Vec<Event>, TransportError> {
    let passages = [Passage { id: SourceId::ThisDocument { page: 0 }, title: "rates.pdf, page 1".into(), text: "Rates rose.".into() }];
    let turn = claude::user_turn(&passages, None, "Why?").unwrap();
    let req = claude::request(&Options::new(&OPUS), &Secret::new("claude-code"), &[], &turn).unwrap();
    let mut seen = Vec::new();
    claude::send(cc, &req, claude::sent_sources(&[turn]), &mut |e| {
        seen.push(e.clone());
        ControlFlow::Continue(())
    })?;
    Ok(seen)
}

const ANSWER: &str = r#"cat > /dev/null
echo '{"type":"system","subtype":"init","tools":[]}'
echo '{"type":"stream_event","event":{"type":"message_start","message":{"id":"m","type":"message","role":"assistant","content":[],"usage":{"input_tokens":9,"output_tokens":1}}}}'
echo '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}'
echo '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"They rose [1]."}}}'
echo '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
echo '{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}}'
echo '{"type":"stream_event","event":{"type":"message_stop"}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"They rose [1]."}'"#;

#[test]
fn the_program_answers_through_the_assistants_decoder_and_runs_locked_down() {
    let d = dir("answer");
    let cc = ClaudeCode::new(program(&d, ANSWER));
    let events = ask(&cc).unwrap();
    let text: String = events.iter().filter_map(|e| if let Event::Text { text, .. } = e { Some(text.as_str()) } else { None }).collect();
    assert_eq!(text, "They rose.");
    assert!(events.iter().any(|e| matches!(e, Event::Citation { .. })), "{events:?}");
    let args = std::fs::read_to_string(d.join("args.txt")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    let after = |flag: &str| args.iter().position(|a| *a == flag).and_then(|i| args.get(i + 1)).copied();
    assert_eq!(after("--tools"), Some(""), "{args:?}");
    assert!(args.contains(&"--safe-mode") && args.contains(&"--no-session-persistence"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn failures_are_reported_and_stuck_or_flooding_programs_are_stopped() {
    let d = dir("fail");
    // Bad arguments: what it printed is the reason.
    let cc = ClaudeCode::new(program(&d, "cat > /dev/null\necho 'error: unknown option' >&2\nexit 2"));
    let events = ask(&cc).unwrap();
    assert!(matches!(events.as_slice(), [Event::ProviderError { message, .. }] if message.contains("unknown option")), "{events:?}");
    // Not there at all.
    let missing = ClaudeCode::new(d.join("no-such-program"));
    assert!(matches!(ask(&missing), Err(TransportError::Connect(_))));
    // Silent for too long.
    let quiet = ClaudeCode::new(program(&d, "cat > /dev/null\nsleep 30")).quiet_after(Duration::from_millis(400));
    let started = Instant::now();
    assert!(matches!(ask(&quiet), Err(TransportError::Timeout)));
    assert!(started.elapsed() < Duration::from_secs(10), "stopped, not waited for");
    // A line past the cap.
    let flood = ClaudeCode::new(program(&d, "cat > /dev/null\nhead -c 5000000 /dev/zero | tr '\\0' 'a'"));
    assert!(matches!(ask(&flood), Err(TransportError::TooLarge(_))));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_sign_in_is_asked_of_the_program() {
    let d = dir("status");
    let cc =
        ClaudeCode::new(program(&d, r#"if [ "$1" = auth ] && [ "$2" = status ]; then echo '{"loggedIn": true, "authMethod": "claude.ai"}'; fi"#));
    assert!(cc.sign_in().unwrap().is_subscription());
    let out = ClaudeCode::new(program(&d, r#"echo '{"loggedIn": false}'"#));
    assert!(out.sign_in().is_err());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_program_is_found_where_claude_code_installs_it() {
    let home = dir("home");
    assert_eq!(find(Some(&home), None).filter(|p| p.starts_with(&home)), None);
    // The Claude desktop app's copy, newest version first.
    for v in ["2.1.9", "2.1.10"] {
        let app = home.join(format!("Library/Application Support/Claude/claude-code/{v}/abc/claude.app/Contents/MacOS"));
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("claude"), "").unwrap();
    }
    let found = desktop_copy(&home).unwrap();
    assert!(found.to_string_lossy().contains("/2.1.10/"), "{found:?}");
    // The installer's place wins over it.
    let bin = home.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), "").unwrap();
    if std::env::var_os("PDFCRAFT_CLAUDE").is_none() {
        assert_eq!(find(Some(&home), None), Some(bin.join("claude")));
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn no_api_key_reaches_the_program() {
    let c = ClaudeCode::new(PathBuf::from("/bin/true")).command();
    let removed: Vec<String> = c.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_string_lossy().into_owned()).collect();
    for var in ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"] {
        assert!(removed.iter().any(|r| r == var), "{var}: {removed:?}");
    }
    assert!(c.get_current_dir().is_some_and(|d| d.ends_with("pdfcraft-claude-code")), "an empty folder of its own");
}
