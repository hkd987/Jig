//! The build driver: generate C, compile the runtime once per mode, link.

use super::c::CProgram;
use crate::mir::lower::Lowerer;
use crate::mir::LowerOpts;
use crate::sema::defs::Program;
use crate::util::fx::{hash_hex, StableHasher};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const RUNTIME_H: &str = include_str!("../../runtime/jig_rt.h");
pub const RUNTIME_C: &str = include_str!("../../runtime/jig_rt.c");

#[derive(Clone, Debug)]
pub struct BuildOpts {
    /// Optimize through LLVM (`clang -O2`) and drop contract checks; asserts and arithmetic
    /// checks stay.
    pub release: bool,
    /// Where intermediate files go (`.jig/build` under the project).
    pub build_dir: PathBuf,
    /// The executable to write.
    pub out: PathBuf,
    /// Checks discharged by cached proofs (`jig prove`), removed from the build.
    pub proven: crate::verify::Proven,
}

/// The C compiler for a mode: `$JIG_CC`, else `clang` for release builds when present, else `cc`.
pub fn c_compiler(release: bool) -> String {
    if let Ok(cc) = std::env::var("JIG_CC") {
        return cc;
    }
    if release && Command::new("clang").arg("--version").output().is_ok_and(|o| o.status.success()) {
        return "clang".into();
    }
    "cc".into()
}

fn flags(release: bool) -> &'static [&'static str] {
    if release {
        &["-O2", "-std=gnu11", "-w", "-pthread"]
    } else {
        &["-O0", "-g", "-std=gnu11", "-w", "-pthread"]
    }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!(
            "C compiler failed ({:?}):\n{}{}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn write_if_changed(path: &Path, contents: &str) -> Result<(), String> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == contents) {
        return Ok(());
    }
    std::fs::write(path, contents).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Compile the runtime into an object cached by content, compiler and mode.
fn runtime_object(cc: &str, opts: &BuildOpts) -> Result<PathBuf, String> {
    let fl = flags(opts.release);
    let mut h = StableHasher::new();
    h.str(RUNTIME_H).str(RUNTIME_C).str(cc);
    for f in fl {
        h.str(f);
    }
    let key = &hash_hex(h.finish())[..16];
    let obj = opts.build_dir.join(format!("jig_rt-{key}.o"));
    if obj.exists() {
        return Ok(obj);
    }
    write_if_changed(&opts.build_dir.join("jig_rt.h"), RUNTIME_H)?;
    write_if_changed(&opts.build_dir.join("jig_rt.c"), RUNTIME_C)?;
    let tmp = opts.build_dir.join(format!("jig_rt-{key}.o.tmp{}", std::process::id()));
    run(Command::new(cc).args(fl).arg("-c").arg(opts.build_dir.join("jig_rt.c")).arg("-o").arg(&tmp))?;
    std::fs::rename(&tmp, &obj).map_err(|e| e.to_string())?;
    Ok(obj)
}

/// How many translation units to split a debug build into: one per ~4,000 lines of C, so
/// large programs compile in parallel and unchanged units come from the object cache.
fn unit_count(prog: &CProgram) -> usize {
    let lines: usize = prog.fns.iter().map(|f| f.bytes().filter(|&b| b == b'\n').count()).sum();
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    (lines / 4000).clamp(1, (jobs * 4).max(1))
}

/// Compile units concurrently, at most `jobs` at a time. Each object is cached by the hash of
/// its compiler, flags, header and source, so an unchanged unit is never recompiled.
fn compile_units(
    cc: &str,
    opts: &BuildOpts,
    header_name: &str,
    header: &str,
    units: &[String],
) -> Result<Vec<PathBuf>, String> {
    let fl = flags(opts.release);
    let obj_dir = opts.build_dir.join("obj");
    std::fs::create_dir_all(&obj_dir).map_err(|e| e.to_string())?;
    let mut objs = Vec::new();
    let mut todo = Vec::new();
    for (i, u) in units.iter().enumerate() {
        let mut h = StableHasher::new();
        h.str(cc).str(header).str(u);
        for f in fl {
            h.str(f);
        }
        let key = &hash_hex(h.finish())[..20];
        let obj = obj_dir.join(format!("{key}.o"));
        if !obj.exists() {
            let src = opts.build_dir.join(format!("unit{i}.c"));
            write_if_changed(&src, &format!("#include \"{header_name}\"\n{u}"))?;
            todo.push((src, obj.clone()));
        }
        objs.push(obj);
    }
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    for batch in todo.chunks(jobs) {
        let mut children = Vec::new();
        for (src, obj) in batch {
            let tmp = obj.with_extension(format!("o.tmp{}", std::process::id()));
            let child = Command::new(cc)
                .args(fl)
                .arg("-I")
                .arg(&opts.build_dir)
                .arg("-c")
                .arg(src)
                .arg("-o")
                .arg(&tmp)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("cannot run {cc}: {e}"))?;
            children.push((child, tmp, obj));
        }
        let mut failure = None;
        for (child, tmp, obj) in children {
            let out = child.wait_with_output().map_err(|e| e.to_string())?;
            if out.status.success() {
                std::fs::rename(&tmp, obj).map_err(|e| e.to_string())?;
            } else if failure.is_none() {
                failure = Some(format!(
                    "C compiler failed ({cc}):\n{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ));
            }
        }
        if let Some(f) = failure {
            return Err(f);
        }
    }
    Ok(objs)
}

/// Build `main` of an analyzed, error-free program into a native executable.
pub fn build(prog: &mut Program, opts: &BuildOpts) -> Result<PathBuf, String> {
    let c = {
        let mut lw = Lowerer::new(prog, LowerOpts { check_contracts: !opts.release });
        lw.mir.elide = opts.proven.clone();
        super::c::generate(&mut lw)?
    };
    std::fs::create_dir_all(&opts.build_dir).map_err(|e| format!("cannot create {}: {e}", opts.build_dir.display()))?;
    let cc = c_compiler(opts.release);
    let rt = runtime_object(&cc, opts)?;
    write_if_changed(&opts.build_dir.join("jig_rt.h"), RUNTIME_H)?;
    let stem = opts.out.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "main".into());
    if let Some(dir) = opts.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut objs = Vec::new();
    if opts.release {
        // One unit: the optimizer sees the whole program.
        let src = opts.build_dir.join(format!("{stem}.c"));
        write_if_changed(&src, &c.unity())?;
        objs.push(src);
    } else {
        let n = unit_count(&c);
        let mut units = vec![c.base.clone()];
        let per = c.fns.len().div_ceil(n).max(1);
        for chunk in c.fns.chunks(per) {
            units.push(chunk.concat());
        }
        let header_name = format!("{stem}.h");
        write_if_changed(&opts.build_dir.join(&header_name), &c.header)?;
        objs = compile_units(&cc, opts, &header_name, &c.header, &units)?;
    }
    run(Command::new(&cc)
        .args(flags(opts.release))
        .arg("-I")
        .arg(&opts.build_dir)
        .args(&objs)
        .arg(&rt)
        .arg("-lm")
        .arg("-o")
        .arg(&opts.out))?;
    Ok(opts.out.clone())
}
