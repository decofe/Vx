//! Compare Vx compiler outputs with Alive2. Build this file directly with rustc.
//! This checks LLVM IR refinement, not the correctness of Vx source-to-IR lowering.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output};

const HELP: &str = "Usage: vx-alive2 --mode codegen|mlir|llvm|reference --function NAME \
    --out DIRECTORY [--reference FILE.ll] FILE.vx

codegen:   compare legacy and default code generation at -O0 after mem2reg, in both directions
mlir:      compare default code generation at -O0 and -O3
llvm:      check LLVM's function(mem2reg,instcombine,simplifycfg) pipeline
reference: check Vx -O0 against the supplied LLVM IR specification

Repeat --function to check several functions. Use leaf functions with matching ABIs.
The output directory must not exist. It retains commands, IR, and tool reports.
Set VXC, MLIR_TRANSLATE_PATH, OPT_PATH, and ALIVE_TV to select executables.
Use a matching LLVM version for Vx, mlir-translate, opt, and Alive2.
Exit codes: 0 = proved, 1 = counterexample candidate, 2 = incomplete or tool failure.
Alive2 uses bounded loop reasoning. This does not verify the Vx frontend.";

#[derive(Debug)]
struct Options {
    mode: String,
    functions: Vec<String>,
    out: PathBuf,
    input: PathBuf,
    reference: Option<PathBuf>,
}

fn options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let mut mode = None;
    let mut functions = Vec::new();
    let mut out = None;
    let mut input = None;
    let mut reference = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => mode = Some(args.next().ok_or("--mode needs a value")?),
            "--function" => functions.push(args.next().ok_or("--function needs a value")?),
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out needs a value")?)),
            "--reference" => {
                reference = Some(PathBuf::from(
                    args.next().ok_or("--reference needs a value")?,
                ));
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
            _ if input.is_none() => input = Some(PathBuf::from(arg)),
            _ => return Err("provide one Vx source file".into()),
        }
    }
    let mode = mode.ok_or("--mode is required")?;
    if !matches!(mode.as_str(), "codegen" | "mlir" | "llvm" | "reference") {
        return Err(format!("unknown mode: {mode}"));
    }
    if functions.is_empty() {
        return Err("--function is required".into());
    }
    for function in &functions {
        if function.is_empty()
            || !function
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.$".contains(&c))
        {
            return Err(format!("unsupported function name: {function}"));
        }
    }
    functions.sort();
    functions.dedup();
    if (mode == "reference") != reference.is_some() {
        return Err("--reference is required only in reference mode".into());
    }
    Ok(Options {
        mode,
        functions,
        out: out.ok_or("--out is required")?,
        input: input.ok_or("a Vx source file is required")?,
        reference,
    })
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Proved,
    Counterexample,
    Incomplete,
}

fn verdict(report: &str) -> Verdict {
    let Some((_, summary)) = report.rsplit_once("Summary:") else {
        return Verdict::Incomplete;
    };
    let count = |label: &str| {
        summary.lines().find_map(|line| {
            line.trim()
                .strip_suffix(label)
                .and_then(|n| n.trim().parse::<usize>().ok())
        })
    };
    let (correct, incorrect, failed, errors) = (
        count("correct transformations"),
        count("incorrect transformations"),
        count("failed-to-prove transformations"),
        count("Alive2 errors"),
    );
    match (correct, incorrect, failed, errors) {
        (_, Some(n), _, _) if n > 0 => Verdict::Counterexample,
        (Some(n), Some(0), Some(0), Some(0)) if n > 0 => Verdict::Proved,
        _ => Verdict::Incomplete,
    }
}

fn status(output: &Output) -> Verdict {
    // Alive2 may return an error status for a genuine counterexample; its summary is
    // authoritative in that case. A proof with a failed process is never a success.
    match verdict(&format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )) {
        Verdict::Proved if !output.status.success() => Verdict::Incomplete,
        other => other,
    }
}

fn tool(variable: &str, default: &str) -> std::ffi::OsString {
    env::var_os(variable).unwrap_or_else(|| default.into())
}

fn run(mut command: Command, out: &Path, name: &str) -> Result<Output, String> {
    fs::write(
        out.join(format!("{name}.command")),
        format!("{command:?}\n"),
    )
    .map_err(|e| e.to_string())?;
    let output = command.output().map_err(|e| format!("{name}: {e}"))?;
    fs::write(out.join(format!("{name}.stdout")), &output.stdout).map_err(|e| e.to_string())?;
    fs::write(out.join(format!("{name}.stderr")), &output.stderr).map_err(|e| e.to_string())?;
    Ok(output)
}

fn checked(command: Command, out: &Path, name: &str) -> Result<Output, String> {
    let output = run(command, out, name)?;
    if !output.status.success() {
        return Err(format!(
            "{name} failed ({}) - see saved stderr",
            output.status
        ));
    }
    Ok(output)
}

fn compile(options: &Options, name: &str, level: &str, legacy: bool) -> Result<PathBuf, String> {
    let mut command = Command::new(tool("VXC", "target/debug/vxc"));
    command.arg(&options.input).args(["--emit-llvm", level]);
    if legacy {
        command.arg("--legacy-codegen");
    }
    let output = checked(command, &options.out, &format!("{name}-vxc"))?;
    let mlir = options.out.join(format!("{name}.mlir"));
    fs::write(&mlir, output.stdout).map_err(|e| e.to_string())?;
    let llvm = options.out.join(format!("{name}.ll"));
    let mut command = Command::new(tool("MLIR_TRANSLATE_PATH", "mlir-translate"));
    command
        .arg("--mlir-to-llvmir")
        .arg(mlir)
        .arg("-o")
        .arg(&llvm);
    checked(command, &options.out, &format!("{name}-translate"))?;
    Ok(llvm)
}

fn normalize(options: &Options, name: &str, input: &Path) -> Result<PathBuf, String> {
    // Flat codegen uses entry-block stack slots where legacy codegen uses phi nodes.
    // Alive2 can report spurious poison for equivalent raw slot/phi pairs; run the
    // standard LLVM promotion on both sides, keeping the original IR for audit.
    let output = options.out.join(format!("{name}-normalized.ll"));
    let mut command = Command::new(tool("OPT_PATH", "opt"));
    command
        .args(["-S", "-passes=mem2reg"])
        .arg(input)
        .arg("-o")
        .arg(&output);
    checked(command, &options.out, &format!("{name}-normalize"))?;
    Ok(output)
}

fn has_definition(ir: &str, function: &str) -> bool {
    ir.lines().any(|line| {
        line.starts_with("define ")
            && (line.contains(&format!("@{function}("))
                || line.contains(&format!("@\"{function}\"(")))
    })
}

fn validate(options: &Options) -> Result<Verdict, String> {
    fs::create_dir(&options.out).map_err(|e| format!("cannot create output directory: {e}"))?;
    fs::copy(&options.input, options.out.join("input.vx")).map_err(|e| e.to_string())?;
    let source = if options.mode == "reference" {
        let source = options.out.join("source.ll");
        fs::copy(options.reference.as_ref().unwrap(), &source).map_err(|e| e.to_string())?;
        source
    } else {
        compile(options, "source", "-O0", options.mode == "codegen")?
    };
    let target = match options.mode.as_str() {
        "llvm" => {
            let target = options.out.join("target.ll");
            let mut command = Command::new(tool("OPT_PATH", "opt"));
            command
                .args(["-S", "-passes=function(mem2reg,instcombine,simplifycfg)"])
                .arg(&source)
                .arg("-o")
                .arg(&target);
            checked(command, &options.out, "opt")?;
            target
        }
        "mlir" => compile(options, "target", "-O3", false)?,
        _ => compile(options, "target", "-O0", false)?,
    };
    let (source, target) = if options.mode == "codegen" {
        (
            normalize(options, "source", &source)?,
            normalize(options, "target", &target)?,
        )
    } else {
        (source, target)
    };
    let source_ir = fs::read_to_string(&source).map_err(|e| e.to_string())?;
    let target_ir = fs::read_to_string(&target).map_err(|e| e.to_string())?;
    for function in &options.functions {
        for (name, ir) in [("source", &source_ir), ("target", &target_ir)] {
            if !has_definition(ir, function) {
                return Err(format!("{name} has no definition of {function}"));
            }
        }
    }
    let mut overall = Verdict::Proved;
    for (index, function) in options.functions.iter().enumerate() {
        let mut command = Command::new(tool("ALIVE_TV", "alive-tv"));
        command
            .arg(&source)
            .arg(&target)
            .arg(format!("--func={function}"))
            .args(["--always-verify", "--smt-to=10000"]);
        if options.mode == "codegen" {
            command.arg("--bidirectional");
        }
        let output = run(command, &options.out, &format!("alive2-{index}"))?;
        let result = status(&output);
        println!("{function}: {result:?}");
        if result == Verdict::Counterexample {
            overall = Verdict::Counterexample;
        } else if result == Verdict::Incomplete && overall != Verdict::Counterexample {
            overall = Verdict::Incomplete;
        }
    }
    Ok(overall)
}

fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("{HELP}");
        return;
    }
    let result = options(args).and_then(|options| validate(&options));
    let code = match result {
        Ok(Verdict::Proved) => 0,
        Ok(Verdict::Counterexample) => 1,
        Ok(Verdict::Incomplete) => 2,
        Err(error) => {
            eprintln!("{error}\n{HELP}");
            2
        }
    };
    process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(correct: usize, incorrect: usize, failed: usize, errors: usize) -> String {
        format!("Summary:\n  {correct} correct transformations\n  {incorrect} incorrect transformations\n  {failed} failed-to-prove transformations\n  {errors} Alive2 errors\n")
    }

    #[test]
    fn requires_a_nonempty_successful_proof() {
        assert_eq!(verdict(&summary(1, 0, 0, 0)), Verdict::Proved);
        assert_eq!(verdict(&summary(0, 0, 0, 0)), Verdict::Incomplete);
        assert_eq!(verdict(&summary(1, 0, 1, 0)), Verdict::Incomplete);
        assert_eq!(verdict(&summary(1, 0, 0, 1)), Verdict::Incomplete);
        assert_eq!(
            verdict("Transformation seems to be correct!"),
            Verdict::Incomplete
        );
        assert_eq!(
            verdict("Summary:\n1 correct transformations"),
            Verdict::Incomplete
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_process_cannot_claim_a_proof() {
        use std::os::unix::process::ExitStatusExt;

        let output = Output {
            status: std::process::ExitStatus::from_raw(1 << 8),
            stdout: summary(1, 0, 0, 0).into_bytes(),
            stderr: Vec::new(),
        };
        assert_eq!(status(&output), Verdict::Incomplete);

        let output = Output {
            stdout: summary(0, 1, 0, 0).into_bytes(),
            ..output
        };
        assert_eq!(status(&output), Verdict::Counterexample);
    }

    #[test]
    fn reports_counterexamples_even_with_other_failures() {
        assert_eq!(verdict(&summary(1, 1, 0, 0)), Verdict::Counterexample);
        assert_eq!(verdict(&summary(0, 1, 1, 1)), Verdict::Counterexample);
    }

    #[test]
    fn distinguishes_definitions_from_calls_and_declarations() {
        assert!(has_definition(
            "define internal i32 @probe(i32 %x) {",
            "probe"
        ));
        assert!(has_definition("define i32 @\"probe\"() {", "probe"));
        assert!(!has_definition("declare i32 @probe()", "probe"));
        assert!(!has_definition("define i32 @other_probe() {", "probe"));
        assert!(!has_definition("  %x = call i32 @probe()", "probe"));
    }

    #[test]
    fn requires_an_explicit_comparison_and_function() {
        assert!(options(["example.vx".to_string()]).is_err());
        let args = [
            "--mode",
            "codegen",
            "--function",
            "probe",
            "--out",
            "proof",
            "example.vx",
        ];
        let parsed = options(args.map(String::from)).unwrap();
        assert_eq!(parsed.functions, ["probe"]);
        assert_eq!(parsed.mode, "codegen");
        assert!(options(
            args.into_iter()
                .chain(["--reference", "spec.ll"])
                .map(String::from)
        )
        .is_err());
    }
}
