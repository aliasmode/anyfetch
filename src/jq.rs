//! jq programs over JSON answers, run by jaq in a child process.
//!
//! A program such as `"a" * 1e12` asks for more memory than exists, and a failed allocation
//! aborts the whole process. `def f: [f]; f` overflows the stack, which also aborts. So the
//! program runs in a copy of this binary that limits its own memory to 1 GB first, and only
//! that copy dies.

use crate::{ToolError, transport::MAX_BODY};
use jaq_core::{
    Compiler, Ctx, Vars, data,
    load::{self, Arena, File, Loader},
    unwrap_valr,
};
use jaq_json::{Val, write::Pp};
use std::{
    io::{Read, Write},
    process::{ExitCode, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The first argument that makes the binary run as the jq child.
pub const CHILD_ARG: &str = "__jq";

/// Filters that read the environment (proxy credentials live there), print to stderr, read
/// more input or stop the process. The program comes from an agent, so it gets none of them.
const DENIED: [&str; 9] = [
    "env",
    "halt",
    "halt_error",
    "debug",
    "debug_empty",
    "stderr",
    "stderr_empty",
    "input",
    "inputs",
];

/// Longest a program may run. `repeat(1)` never ends.
const TIMEOUT: Duration = Duration::from_secs(15);

/// CPU time after which the OS ends a child whose parent died before it could kill it.
const CPU_SECONDS: u64 = 20;

const MEMORY_LIMIT: usize = 1024 * 1024 * 1024;

/// Stack of the thread that runs the program. Deep recursion overflows it and ends the child.
const STACK_SIZE: usize = 16 * 1024 * 1024;

/// Deepest nesting of arrays and objects accepted in the input.
const MAX_DEPTH: usize = 256;

const MAX_RESULTS: usize = 10_000;

/// Runs the jq `program` on the JSON in `body`. One result is that value; several are an array
/// of them. The answer is pretty-printed JSON.
pub(crate) async fn run(body: String, program: String) -> Result<String, ToolError> {
    let unavailable = |message: String| ToolError::new("jq_unavailable", message);
    let exe = std::env::current_exe()
        .map_err(|error| unavailable(format!("Cannot start jq: {error}")))?;
    let mut child = tokio::process::Command::new(anyfetch_exe(exe))
        .arg(CHILD_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| unavailable(format!("Cannot start jq: {error}")))?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(unavailable("Cannot start jq".to_owned()));
    };
    let exchange = async {
        // A child that dies early closes its stdin. Its exit status tells why, so a failed write
        // is not reported itself.
        let request = async {
            let header = format!("{}\n", program.len());
            stdin.write_all(header.as_bytes()).await?;
            stdin.write_all(program.as_bytes()).await?;
            stdin.write_all(body.as_bytes()).await
        };
        let _ = request.await;
        drop(stdin);
        let mut answer = Vec::new();
        let limit = (MAX_BODY + 64 * 1024) as u64;
        let read = stdout.take(limit).read_to_end(&mut answer).await;
        let status = child.wait().await;
        (
            answer,
            read.is_ok() && status.is_ok_and(|status| status.success()),
        )
    };
    // On timeout the future drops, and with it the child, which `kill_on_drop` ends.
    let (answer, exited) = tokio::time::timeout(TIMEOUT, exchange)
        .await
        .map_err(|_| timeout())?;
    if !exited {
        let message = "The jq program used more than 1 GB, recursed too deep or crashed";
        return Err(ToolError::new("invalid_request", message));
    }
    // A clean exit without the framed answer means the binary is not an anyfetch child.
    let no_answer = || unavailable("The jq child gave no answer".to_owned());
    let answer = String::from_utf8(answer).map_err(|_| no_answer())?;
    if let Some(result) = answer.strip_prefix("ok\n") {
        return Ok(result.to_owned());
    }
    let (code, message) = answer
        .strip_prefix("error\n")
        .and_then(|error| error.split_once('\n'))
        .ok_or_else(no_answer)?;
    Err(ToolError::new(known_code(code), message))
}

/// The binary that runs the child. Test binaries in `target/<profile>/deps` use the anyfetch
/// binary that Cargo builds next to that folder.
fn anyfetch_exe(exe: std::path::PathBuf) -> std::path::PathBuf {
    if cfg!(debug_assertions) && exe.file_stem().is_some_and(|stem| stem != "anyfetch") {
        let name = format!("anyfetch{}", std::env::consts::EXE_SUFFIX);
        if let Some(built) = exe
            .parent()
            .and_then(|deps| deps.parent())
            .map(|dir| dir.join(name))
        {
            return built;
        }
    }
    exe
}

/// The child's error code as one of the codes it can send.
fn known_code(code: &str) -> &'static str {
    ["not_json", "no_match", "too_large", "timeout"]
        .into_iter()
        .find(|known| *known == code)
        .unwrap_or("invalid_request")
}

/// The jq child: limits its memory, reads `<program length>\n<program><body>` from stdin and
/// writes `ok\n<result>` or `error\n<code>\n<message>` to stdout.
pub fn child() -> ExitCode {
    let answer = match limit_memory() {
        Ok(()) => answer(),
        Err(error) => Err(ToolError::new(
            "invalid_request",
            format!("Cannot limit the memory of jq: {error}"),
        )),
    };
    let mut stdout = std::io::stdout().lock();
    let written = match &answer {
        Ok(result) => write!(stdout, "ok\n{result}"),
        Err(error) => write!(stdout, "error\n{}\n{}", error.code, error.message),
    };
    match written.and_then(|()| stdout.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn answer() -> Result<String, ToolError> {
    let bad_request = || ToolError::new("invalid_request", "The jq child got a bad request");
    let mut request = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut request)
        .map_err(|_| bad_request())?;
    let newline = request
        .iter()
        .position(|&byte| byte == b'\n')
        .ok_or_else(bad_request)?;
    let length: usize = std::str::from_utf8(&request[..newline])
        .ok()
        .and_then(|length| length.parse().ok())
        .ok_or_else(bad_request)?;
    let rest = &request[newline + 1..];
    if length > rest.len() {
        return Err(bad_request());
    }
    let (program, body) = rest.split_at(length);
    let program = std::str::from_utf8(program).map_err(|_| bad_request())?;
    let body = std::str::from_utf8(body).map_err(|_| bad_request())?;
    let deadline = Instant::now() + TIMEOUT;
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, || evaluate(body, program, deadline))
            .map_err(|error| ToolError::new("invalid_request", format!("Cannot run jq: {error}")))?
            .join()
            .unwrap_or_else(|_| Err(ToolError::new("invalid_request", "The jq program failed")))
    })
}

#[cfg(windows)]
fn limit_memory() -> std::io::Result<()> {
    use windows_sys::Win32::System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOB_OBJECT_LIMIT_PROCESS_TIME, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        },
        Threading::GetCurrentProcess,
    };
    // SAFETY: null attributes and a null name are valid and ask for an unnamed job.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // Both flags: cap committed memory and CPU time per process in the job.
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_PROCESS_TIME;
    // The CPU time limit counts 100 ns ticks.
    limits.BasicLimitInformation.PerProcessUserTimeLimit = CPU_SECONDS as i64 * 10_000_000;
    limits.ProcessMemoryLimit = MEMORY_LIMIT;
    let size = size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
    let limits = (&raw const limits).cast();
    // SAFETY: `limits` points to the struct this information class names, `size` long.
    let set =
        unsafe { SetInformationJobObject(job, JobObjectExtendedLimitInformation, limits, size) };
    // SAFETY: `job` is a job handle we own, and the pseudo handle names this process.
    if set == 0 || unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // The job stays open until the process ends, so its limits stay in force.
    Ok(())
}

#[cfg(unix)]
fn limit_memory() -> std::io::Result<()> {
    // Linux counts the heap, anonymous maps and thread stacks in RLIMIT_DATA. RLIMIT_AS would
    // also count the mapped binary and malloc's reserved but unused arenas.
    #[cfg(target_os = "linux")]
    let memory = libc::RLIMIT_DATA;
    #[cfg(not(target_os = "linux"))]
    let memory = libc::RLIMIT_AS;
    for (resource, value) in [
        (memory, MEMORY_LIMIT as libc::rlim_t),
        (libc::RLIMIT_CPU, CPU_SECONDS as libc::rlim_t),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: `limit` is a valid rlimit that lives across the call.
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn limit_memory() -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn evaluate(body: &str, program: &str, deadline: Instant) -> Result<String, ToolError> {
    let invalid = |message: String| ToolError::new("invalid_request", message);
    // jaq parses nested input by recursion, so deep input could overflow the stack.
    if too_deep(body.as_bytes()) {
        return Err(invalid(format!(
            "The JSON is nested deeper than {MAX_DEPTH} levels"
        )));
    }
    let input = jaq_json::read::parse_single(body.as_bytes())
        .map_err(|error| ToolError::new("not_json", format!("The body is not JSON: {error}")))?;
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs())
        .filter(|def| !DENIED.contains(&def.name));
    let funs = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs())
        .filter(|(name, ..)| !DENIED.contains(name));
    let arena = Arena::default();
    let file = File {
        code: program,
        path: (),
    };
    let modules = Loader::new(defs).load(&arena, file).map_err(|errors| {
        let errors = errors.into_iter().flat_map(|(_, error)| load_errors(error));
        invalid(format!("The jq program does not parse: {}", join(errors)))
    })?;
    let filter = Compiler::default()
        .with_funs(funs)
        .compile(modules)
        .map_err(|errors| {
            let errors = errors.into_iter().flat_map(|(_, errors)| errors);
            let errors =
                errors.map(|(name, kind)| format!("{} {name} is not defined", kind.as_str()));
            invalid(format!("The jq program does not compile: {}", join(errors)))
        })?;
    let context = Ctx::<data::JustLut<Val>>::new(&filter.lut, Vars::new([]));
    let mut results = Vec::new();
    // The parent kills the child at the deadline. This check only ends early a program that
    // keeps giving results.
    for result in filter.id.run((context, input)).map(unwrap_valr) {
        if Instant::now() > deadline {
            return Err(timeout());
        }
        results.push(result.map_err(|error| invalid(format!("The jq program failed: {error}")))?);
        if results.len() > MAX_RESULTS {
            let message = format!("The jq program gives more than {MAX_RESULTS} results");
            return Err(invalid(message));
        }
    }
    let value = match results.len() {
        0 => return Err(ToolError::new("no_match", "The jq program gave no result")),
        1 => results.remove(0),
        _ => results.into_iter().collect(),
    };
    let pretty = Pp {
        indent: Some("  ".to_owned()),
        sep_space: true,
        ..Pp::default()
    };
    let mut output = Capped(Vec::new());
    jaq_json::write::write(&mut output, &pretty, 0, &value).map_err(|_| {
        let size = MAX_BODY / 1024 / 1024;
        ToolError::new(
            "too_large",
            format!("The jq result is larger than {size} MB"),
        )
    })?;
    Ok(String::from_utf8_lossy(&output.0).into_owned())
}

/// Whether the JSON in `bytes` nests arrays and objects deeper than [`MAX_DEPTH`]. Brackets
/// inside strings do not count.
fn too_deep(bytes: &[u8]) -> bool {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for &byte in bytes {
        match (in_string, escaped, byte) {
            (true, true, _) => escaped = false,
            (true, false, b'\\') => escaped = true,
            (true, false, b'"') => in_string = false,
            (true, false, _) => {}
            (false, _, b'"') => in_string = true,
            (false, _, b'[' | b'{') => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return true;
                }
            }
            (false, _, b']' | b'}') => depth = depth.saturating_sub(1),
            (false, ..) => {}
        }
    }
    false
}

fn timeout() -> ToolError {
    ToolError::new("timeout", "The jq program ran longer than 15 seconds")
}

fn join(parts: impl Iterator<Item = String>) -> String {
    parts.collect::<Vec<_>>().join("; ")
}

fn load_errors(error: load::Error<&str>) -> Vec<String> {
    // Both error kinds hold the rest of the program from where it went wrong.
    let near = |expected: &str, rest: &str| match rest.chars().take(20).collect::<String>() {
        rest if rest.is_empty() => format!("expected {expected} at the end"),
        rest => format!("expected {expected} at `{rest}`"),
    };
    match error {
        load::Error::Io(errors) => errors
            .into_iter()
            .map(|(path, message)| format!("{path}: {message}"))
            .collect(),
        load::Error::Lex(errors) => errors
            .iter()
            .map(|(expected, rest)| near(expected.as_str(), rest))
            .collect(),
        load::Error::Parse(errors) => errors
            .iter()
            .map(|(expected, rest)| near(expected.as_str(), rest))
            .collect(),
    }
}

/// A buffer that refuses to grow past [`MAX_BODY`].
struct Capped(Vec<u8>);

impl std::io::Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len() + bytes.len() > MAX_BODY {
            return Err(std::io::Error::other("too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jq(body: &str, program: &str) -> Result<String, ToolError> {
        evaluate(body, program, Instant::now() + TIMEOUT)
    }

    fn value(body: &str, program: &str) -> serde_json::Value {
        serde_json::from_str(&jq(body, program).unwrap()).unwrap()
    }

    #[test]
    fn selects_paths_and_runs_filters() {
        let body = r#"{"prop":{"arr":[0,1,2,3,{"value":"five"}]},"items":[{"name":"a","x":1},{"name":"b","x":2}]}"#;
        assert_eq!(value(body, ".prop.arr[4].value"), "five");
        assert_eq!(value(body, ".items[].name"), serde_json::json!(["a", "b"]));
        assert_eq!(
            value(body, ".items | map(select(.x > 1))"),
            serde_json::json!([{"name": "b", "x": 2}])
        );
        assert_eq!(value(body, "keys"), serde_json::json!(["items", "prop"]));
        assert_eq!(value(body, ".prop.arr[-2]"), 3);
        assert_eq!(jq(body, "empty").unwrap_err().code, "no_match");
        assert_eq!(jq("<html>", ".").unwrap_err().code, "not_json");
    }

    #[test]
    fn rejects_bad_programs_and_the_environment() {
        let error = jq("{}", ".a[").unwrap_err();
        assert_eq!(error.code, "invalid_request");
        assert!(
            error.message.contains("does not parse"),
            "{}",
            error.message
        );
        for program in [
            "env",
            "$ENV",
            "env.PATH",
            "input",
            "halt",
            "debug",
            "now | stderr",
        ] {
            let error = jq("{}", program).unwrap_err();
            assert_eq!(error.code, "invalid_request", "{program}");
            assert!(
                error.message.contains("not defined"),
                "{program}: {}",
                error.message
            );
        }
        assert_eq!(
            jq("1", "error(\"no\")").unwrap_err().code,
            "invalid_request"
        );
    }

    #[test]
    fn stops_a_program_that_never_ends() {
        let error = evaluate("1", "repeat(1)", Instant::now()).unwrap_err();
        assert_eq!(error.code, "timeout");
    }

    #[test]
    fn rejects_deep_input_but_not_brackets_in_strings() {
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        let error = jq(&deep, ".").unwrap_err();
        assert!(
            error.message.contains("deeper than 256"),
            "{}",
            error.message
        );
        let edge = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(jq(&edge, ".").is_ok());
        let brackets = format!(r#"{{"a":"{}\"[{{"}}"#, "[".repeat(MAX_DEPTH + 1));
        assert_eq!(
            value(&brackets, ".a").as_str().unwrap().len(),
            MAX_DEPTH + 4
        );
    }

    #[test]
    fn keeps_only_the_codes_the_child_can_send() {
        assert_eq!(known_code("no_match"), "no_match");
        assert_eq!(known_code("anything"), "invalid_request");
    }
}
